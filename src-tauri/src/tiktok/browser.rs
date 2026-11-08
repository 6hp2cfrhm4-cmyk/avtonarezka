//! Запасной путь: ролик заливается через обычную страницу загрузки TikTok в Chromium.
//!
//! Программа запускает Chromium с отдельным профилем и управляет им по протоколу отладки (CDP) на локальном порту.
//! Входит в TikTok сам пользователь, в окне браузера: пароль программа не видит и не хранит, а куки остаются в профиле.
//!
//! Хрупкость: путь зависит от вёрстки сайта TikTok. Селекторы ниже — рабочая гипотеза, не гарантия; сверить их с настоящей
//! страницей загрузки может только человек с аккаунтом. Они вынесены в `Selectors`, чтобы чинить в одном месте.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::{
    drafts::get as get_draft,
    state::AppState,
    tiktok::api::make_title,
    tiktok_cmds::{finish_post, begin_post, PostDone, PostFailed, PostProgress, PublishArgs, PublishStart},
};

pub const UPLOAD_URL: &str = "https://www.tiktok.com/tiktokstudio/upload";

#[derive(Clone, Debug)]
pub struct Selectors {
    pub file_input: String,
    pub caption: String,
    pub post_button: String,
}

impl Default for Selectors {
    fn default() -> Self {
        Self {
            file_input: r#"input[type="file"]"#.into(),
            caption: r#".public-DraftEditor-content[contenteditable="true"], div[contenteditable="true"]"#.into(),
            post_button: r#"button[data-e2e="post_video_button"]"#.into(),
        }
    }
}

/// Браузер на движке Chromium. Linux: сначала настоящий Chromium, затем Chrome (из PATH).
/// Windows: Chrome, Chromium, затем Edge (он есть в любой Windows 11 и говорит на том же протоколе управления).
pub fn find_chromium() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"]
            .iter()
            .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
            .collect();
        windows_browser_candidates(&roots).into_iter().find(|p| p.is_file())
    }
    #[cfg(not(windows))]
    {
        let path = std::env::var_os("PATH")?;
        ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"]
            .iter()
            .find_map(|name| std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file()))
    }
}

/// Где Windows-версии браузеров лежат по умолчанию: порядок — от самого желаемого.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_browser_candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
    const REL: [&[&str]; 3] = [
        &["Google", "Chrome", "Application", "chrome.exe"],
        &["Chromium", "Application", "chrome.exe"],
        &["Microsoft", "Edge", "Application", "msedge.exe"],
    ];
    REL.iter().flat_map(|rel| roots.iter().map(move |root| rel.iter().fold(root.clone(), |p, part| p.join(part)))).collect()
}

// ---------------------------------------------------------------- CDP

pub struct Cdp {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next: u64,
}

impl Cdp {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let (ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| anyhow!("браузер не ответил"))?
            .context("не удалось подключиться к браузеру")?;
        Ok(Self { ws, next: 1 })
    }

    /// Команда и её ответ. События браузера между ними пропускаются.
    pub async fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next;
        self.next += 1;
        self.ws.send(Message::text(json!({ "id": id, "method": method, "params": params }).to_string())).await.context("браузер закрыт")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.ws.next()).await.map_err(|_| anyhow!("браузер не ответил на {method}"))?.ok_or_else(|| anyhow!("окно браузера закрыто"))?.context("связь с браузером оборвалась")?;
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(text.as_str()).context("непонятный ответ браузера")?;
            if v["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                bail!("{method}: {}", e["message"].as_str().unwrap_or("ошибка"));
            }
            return Ok(v["result"].clone());
        }
    }

    /// Результат JS-выражения как значение JSON.
    pub async fn eval(&mut self, expr: &str) -> anyhow::Result<Value> {
        let r = self.call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true })).await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("ошибка на странице: {}", ex["exception"]["description"].as_str().or(ex["text"].as_str()).unwrap_or("?"));
        }
        Ok(r["result"]["value"].clone())
    }
}

/// Строка для подстановки в JS: JSON-литерал безопасен для любых символов.
fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

// ---------------------------------------------------------------- запуск браузера

fn port_file(profile: &Path) -> PathBuf {
    profile.join("DevToolsActivePort")
}

async fn alive(port: u16) -> bool {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .map(|c| async move { c.get(format!("http://127.0.0.1:{port}/json/version")).send().await.is_ok_and(|r| r.status().is_success()) })
        .map_or(std::future::ready(false).left_future(), |f| f.right_future())
        .await
}

use futures_util::FutureExt as _;

/// Уже запущенный браузер с этим профилем или новый. Профиль принадлежит одному процессу, поэтому второй запуск
/// открыл бы окно в первом и сразу завершился: нужный порт берём из файла профиля.
pub async fn ensure_browser(chromium: &Path, profile: &Path, headless: bool, start_url: &str) -> anyhow::Result<u16> {
    std::fs::create_dir_all(profile).context("не удалось создать профиль браузера")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // в профиле лежат куки входа в TikTok
        let _ = std::fs::set_permissions(profile, std::fs::Permissions::from_mode(0o700));
    }
    if let Some(port) = read_port(profile) {
        if alive(port).await {
            return Ok(port);
        }
    }
    let _ = std::fs::remove_file(port_file(profile)); // остаток прошлого запуска

    let mut cmd = std::process::Command::new(chromium);
    cmd.arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--no-first-run", "--no-default-browser-check"]);
    #[cfg(not(windows))]
    cmd.arg("--ozone-platform-hint=auto");
    if headless {
        cmd.arg("--headless=new");
    }
    // Chromium кладёт служебный сокет в TMPDIR, а у unix-сокета путь не длиннее 108 символов:
    // при длинном TMPDIR браузер падает с «Socket path too long». Короткий /tmp надёжнее.
    #[cfg(not(windows))]
    {
        if std::env::temp_dir().as_os_str().len() > 60 {
            cmd.env("TMPDIR", "/tmp");
        }
    }
    // вывод браузера — в файл профиля: если он не запустится, причину можно прочитать, а не гадать
    let log = std::fs::OpenOptions::new().create(true).append(true).open(profile.join("chromium.log")).ok();
    let (out, errs) = match log.and_then(|f| f.try_clone().ok().map(|g| (f, g))) {
        Some((a, b)) => (std::process::Stdio::from(a), std::process::Stdio::from(b)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    cmd.arg(start_url).stdin(std::process::Stdio::null()).stdout(out).stderr(errs);
    let mut child = cmd.spawn().with_context(|| format!("не удалось запустить {}", chromium.display()))?;
    // браузер остаётся жить после программы (пользователь может быть посреди входа); дожидаемся его, чтобы не копить зомби
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    tokio::pin!(rx);

    for _ in 0..150 {
        if let Some(port) = read_port(profile) {
            if alive(port).await {
                return Ok(port);
            }
        }
        tokio::select! {
            code = &mut rx => bail!("браузер сразу закрылся ({:?}). Возможно, этот профиль уже открыт в другом окне Chromium.", code.ok().and_then(Result::ok).and_then(|s| s.code())),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    bail!("браузер не открыл порт управления за 15 секунд")
}

fn read_port(profile: &Path) -> Option<u16> {
    std::fs::read_to_string(port_file(profile)).ok()?.lines().next()?.trim().parse().ok()
}

/// Новая вкладка с адресом; возвращает адрес управления ею.
async fn open_tab(port: u16, url: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let mut endpoint = reqwest::Url::parse(&format!("http://127.0.0.1:{port}/json/new"))?;
    endpoint.set_query(Some(url)); // сам адрес и есть строка запроса; кодирует его Url
    // Chromium 111+ принимает здесь только PUT
    let v: Value = client.put(endpoint).send().await.context("не удалось открыть вкладку")?.json().await.context("браузер ответил не JSON")?;
    v["webSocketDebuggerUrl"].as_str().map(str::to_string).ok_or_else(|| anyhow!("у новой вкладки нет адреса управления"))
}

// ---------------------------------------------------------------- сценарий страницы

pub struct DriveConfig {
    pub chromium: PathBuf,
    pub profile: PathBuf,
    pub headless: bool,
    pub upload_url: String,
    pub file: PathBuf,
    pub caption: String,
    pub auto_publish: bool,
    pub selectors: Selectors,
    /// Сколько ждать, пока пользователь войдёт в аккаунт.
    pub login_timeout: Duration,
    /// Сколько ждать каждого следующего шага страницы (загрузка файла, редактор, кнопка).
    pub step_timeout: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DriveOutcome {
    /// Кнопка «Опубликовать» нажата программой. Иначе ролик ждёт нажатия пользователя.
    pub clicked_post: bool,
}

async fn wait_for(cdp: &mut Cdp, expr: &str, timeout: Duration, what: &str) -> anyhow::Result<()> {
    let end = tokio::time::Instant::now() + timeout;
    loop {
        if cdp.eval(expr).await?.as_bool() == Some(true) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= end {
            bail!("{what}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn drive(cfg: &DriveConfig, on: &mut (dyn FnMut(f64, &str) + Send)) -> anyhow::Result<DriveOutcome> {
    let sel = &cfg.selectors;
    on(0.02, "Открываю браузер");
    let port = ensure_browser(&cfg.chromium, &cfg.profile, cfg.headless, "about:blank").await?;
    let ws = open_tab(port, &cfg.upload_url).await?;
    let mut cdp = Connected::new(&ws).await?;
    let cdp = &mut cdp.0;
    cdp.call("Page.enable", json!({})).await?;
    cdp.call("DOM.enable", json!({})).await?;

    // страница загрузки появляется только после входа: ждём поле выбора файла, пока пользователь входит
    let has_input = format!("!!document.querySelector({})", js(&sel.file_input));
    on(0.08, "Жду страницу загрузки TikTok. Если попросит, войди в аккаунт в окне браузера");
    wait_for(cdp, &has_input, cfg.login_timeout, "Не дождался страницы загрузки TikTok: войди в аккаунт в окне браузера и повтори отправку").await?;

    on(0.25, "Выбираю файл");
    let doc = cdp.call("DOM.getDocument", json!({ "depth": 0 })).await?;
    let root = doc["root"]["nodeId"].as_i64().context("нет корня страницы")?;
    let node = cdp.call("DOM.querySelector", json!({ "nodeId": root, "selector": sel.file_input })).await?["nodeId"].as_i64().unwrap_or(0);
    if node == 0 {
        bail!("Поле выбора файла пропало со страницы");
    }
    let path = cfg.file.to_str().context("путь к файлу не в UTF-8")?;
    cdp.call("DOM.setFileInputFiles", json!({ "files": [path], "nodeId": node })).await?;

    on(0.45, "Жду, пока TikTok примет файл");
    let has_caption = format!("!!document.querySelector({})", js(&sel.caption));
    wait_for(cdp, &has_caption, cfg.step_timeout, "TikTok не показал поле подписи. Возможно, изменилась вёрстка страницы").await?;

    on(0.7, "Вставляю подпись");
    let focus = format!(
        "(() => {{ const el = document.querySelector({}); if (!el) return false; el.focus(); document.execCommand('selectAll', false); document.execCommand('delete', false); return document.activeElement === el; }})()",
        js(&sel.caption)
    );
    if cdp.eval(&focus).await?.as_bool() != Some(true) {
        bail!("Не удалось встать в поле подписи");
    }
    if !cfg.caption.is_empty() {
        cdp.call("Input.insertText", json!({ "text": cfg.caption })).await?;
    }

    if !cfg.auto_publish {
        on(1.0, "Файл и подпись внесены. Проверь и нажми «Опубликовать» в окне TikTok");
        return Ok(DriveOutcome { clicked_post: false });
    }

    on(0.85, "Жду, когда станет доступна кнопка «Опубликовать»");
    let ready = format!("(() => {{ const b = document.querySelector({}); return !!b && !b.disabled && b.getAttribute('aria-disabled') !== 'true'; }})()", js(&sel.post_button));
    wait_for(cdp, &ready, cfg.step_timeout, "Кнопка «Опубликовать» не стала доступной: TikTok ещё обрабатывает файл или изменилась вёрстка").await?;
    if cdp.eval(&format!("(() => {{ const b = document.querySelector({}); if (!b) return false; b.click(); return true; }})()", js(&sel.post_button))).await?.as_bool() != Some(true) {
        bail!("Кнопка «Опубликовать» пропала");
    }
    on(1.0, "Нажата кнопка «Опубликовать». Проверь результат в окне TikTok");
    Ok(DriveOutcome { clicked_post: true })
}

struct Connected(Cdp);

impl Connected {
    async fn new(ws: &str) -> anyhow::Result<Self> {
        Ok(Self(Cdp::connect(ws).await?))
    }
}

// ---------------------------------------------------------------- стык с приложением

pub(crate) async fn start(app: AppHandle, state: State<'_, AppState>, args: PublishArgs, file: PathBuf, auto_publish: bool) -> Result<PublishStart, String> {
    let chromium = find_chromium().ok_or("Не найден Chromium. Установи пакет chromium или выбери способ «через API»")?;
    let (post_id, caption) = {
        let db = state.db.0.lock().unwrap();
        let d = get_draft(&db, args.draft_id)?;
        db.execute(
            "UPDATE moments SET caption = ?2, hashtags = ?3 WHERE id = ?1",
            rusqlite::params![d.moment_id, args.caption.trim(), serde_json::to_string(&crate::tiktok::api::normalize_hashtags(&args.hashtags)).unwrap_or_else(|_| "[]".into())],
        )
        .map_err(|e| e.to_string())?;
        (begin_post(&db, args.draft_id, "browser", "browser").map_err(|e| e.to_string())?, make_title(&args.caption, &args.hashtags))
    };
    state.posting.lock().unwrap().insert(args.draft_id);
    let profile = state.data_dir.join("tiktok-browser");
    let draft_id = args.draft_id;

    tauri::async_runtime::spawn(async move {
        let url = {
            #[cfg(debug_assertions)]
            {
                std::env::var("AVTO_TIKTOK_UPLOAD_URL").unwrap_or_else(|_| UPLOAD_URL.into())
            }
            #[cfg(not(debug_assertions))]
            {
                UPLOAD_URL.to_string()
            }
        };
        let cfg = DriveConfig {
            chromium,
            profile,
            headless: false,
            upload_url: url,
            file,
            caption,
            auto_publish,
            selectors: Selectors::default(),
            login_timeout: Duration::from_secs(600),
            step_timeout: Duration::from_secs(180),
        };
        let reporter = app.clone();
        let result = drive(&cfg, &mut |fraction, message| {
            let _ = reporter.emit("post-progress", PostProgress { draft_id, fraction, message: message.to_string() });
        })
        .await;

        let state = app.state::<AppState>();
        state.posting.lock().unwrap().remove(&draft_id);
        let stored = match &result {
            Ok(o) if o.clicked_post => Ok(("Нажата кнопка «Опубликовать» в окне TikTok. Проверь, что ролик вышел.".to_string(), None)),
            Ok(_) => Ok(("Файл и подпись внесены в окне TikTok. Осталось проверить и нажать «Опубликовать».".to_string(), None)),
            Err(e) => Err(format!("{e:#}")),
        };
        finish_post(&state.db.0.lock().unwrap(), post_id, &stored);
        let _ = match stored {
            Ok((detail, _)) => app.emit("post-done", PostDone { draft_id, detail }),
            Err(message) => app.emit("post-error", PostFailed { draft_id, message }),
        };
    });
    Ok(PublishStart { post_id: Some(post_id), already_sent: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_literals_escape_quotes_newlines_and_unicode() {
        assert_eq!(js("a\"b"), r#""a\"b""#);
        assert_eq!(js("строка\nдве"), "\"строка\\nдве\"");
        // закрывающий тег не должен вырываться из литерала
        assert!(js("</script>").starts_with('"'));
    }

    #[test]
    fn windows_candidates_put_chrome_before_edge_and_try_every_root() {
        let roots = [PathBuf::from("PF"), PathBuf::from("PF86")];
        let c = windows_browser_candidates(&roots);
        assert_eq!(c.len(), 6);
        assert_eq!(c[0], ["PF", "Google", "Chrome", "Application", "chrome.exe"].iter().collect::<PathBuf>());
        assert_eq!(c[1], ["PF86", "Google", "Chrome", "Application", "chrome.exe"].iter().collect::<PathBuf>());
        assert!(c[4].ends_with("msedge.exe") && c[5].ends_with("msedge.exe"));
    }

    #[test]
    fn port_file_is_read_from_the_first_line() {
        let dir = std::env::temp_dir().join(format!("avto-port-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_port(&dir), None);
        std::fs::write(port_file(&dir), "41234\n/devtools/browser/abc-def").unwrap();
        assert_eq!(read_port(&dir), Some(41234));
        std::fs::write(port_file(&dir), "мусор").unwrap();
        assert_eq!(read_port(&dir), None);
    }

    #[test]
    fn default_selectors_cover_the_three_page_elements() {
        let s = Selectors::default();
        assert!(s.file_input.contains("file") && s.caption.contains("contenteditable") && s.post_button.contains("post_video_button"));
    }
}
