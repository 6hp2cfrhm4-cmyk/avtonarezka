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
    accounts,
    drafts::get as get_draft,
    state::AppState,
    tiktok::api::make_caption,
    tiktok_cmds::{finish_post, begin_post, mark_sent, PostDone, PostFailed, PostProgress, PublishArgs, PublishStart},
};

pub const UPLOAD_URL: &str = "https://www.tiktok.com/tiktokstudio/upload";

#[derive(Clone, Debug)]
pub struct Selectors {
    pub file_input: String,
    pub caption: String,
    pub post_button: String,
    /// Кнопка в окне «Продолжить публикацию?» (TikTok так спрашивает, пока идёт проверка авторских прав). Пусто — окна нет.
    pub confirm_button: String,
    /// Признак успеха: адрес страницы после публикации содержит эту строку…
    pub success_url: String,
    /// …или на странице появился этот элемент. Если не задано ни то, ни другое, очередь не стартует: подтвердить публикацию нечем.
    pub success_selector: String,
    /// Окно проверки «я не робот». Видимое — значит, дальше решает человек: обходить такие проверки программа не пытается.
    pub captcha: String,
}

impl Default for Selectors {
    fn default() -> Self {
        // Рабочая гипотеза, не проверенная на живой странице (см. заголовок файла): сверять нужно человеку с аккаунтом.
        Self {
            file_input: r#"input[type="file"]"#.into(),
            caption: r#".public-DraftEditor-content[contenteditable="true"], div[contenteditable="true"]"#.into(),
            post_button: r#"button[data-e2e="post_video_button"]"#.into(),
            confirm_button: String::new(),
            success_url: "tiktokstudio/content".into(),
            success_selector: String::new(),
            captcha: r#"[id*="captcha" i], [class*="captcha" i]"#.into(),
        }
    }
}

/// TikTok просит человека: проверка «я не робот» или вход. Автоматика остановилась, дальше решает пользователь.
#[derive(Debug)]
pub struct NeedsHuman(pub String);

impl std::fmt::Display for NeedsHuman {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NeedsHuman {}

/// Кнопка «Опубликовать» нажата, но выход ролика не подтверждён: он мог выйти. Повторять отправку нельзя, пока человек не проверил.
#[derive(Debug)]
pub struct AfterClick(pub String);

impl std::fmt::Display for AfterClick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AfterClick {}

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
    /// После клика дождаться, пока TikTok подтвердит публикацию (адрес или элемент из `Selectors`). Для очереди обязательно.
    pub require_confirmation: bool,
    /// Сколько ждать этого подтверждения.
    pub confirm_timeout: Duration,
    /// После подтверждённой публикации закрыть вкладку: очередь из десятков роликов не должна копить окна.
    pub close_on_success: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DriveOutcome {
    /// Кнопка «Опубликовать» нажата программой. Иначе ролик ждёт нажатия пользователя.
    pub clicked_post: bool,
    /// TikTok подтвердил публикацию (только при `require_confirmation`).
    pub confirmed: bool,
}

/// Видимое окно проверки: скрытый контейнер с «captcha» в имени у страницы может быть всегда.
fn captcha_expr(selector: &str) -> Option<String> {
    (!selector.trim().is_empty()).then(|| {
        format!(
            "(() => {{ try {{ return [...document.querySelectorAll({})].some(e => {{ const r = e.getBoundingClientRect(); return r.width > 40 && r.height > 40 && getComputedStyle(e).visibility !== 'hidden' && getComputedStyle(e).display !== 'none'; }}); }} catch (_) {{ return false; }} }})()",
            js(selector)
        )
    })
}

const CAPTCHA_TEXT: &str = "TikTok просит пройти проверку (капчу) или войти. Реши её в окне браузера и запусти очередь снова: программа такие проверки не обходит";

async fn wait_for(cdp: &mut Cdp, expr: &str, timeout: Duration, what: &str, captcha: Option<&str>) -> anyhow::Result<()> {
    let end = tokio::time::Instant::now() + timeout;
    loop {
        if cdp.eval(expr).await?.as_bool() == Some(true) {
            return Ok(());
        }
        if let Some(c) = captcha {
            if cdp.eval(c).await?.as_bool() == Some(true) {
                return Err(anyhow::Error::new(NeedsHuman(CAPTCHA_TEXT.into())));
            }
        }
        if tokio::time::Instant::now() >= end {
            bail!("{what}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Идентификатор вкладки из адреса управления `ws://127.0.0.1:порт/devtools/page/<id>`.
fn tab_id(ws: &str) -> Option<&str> {
    ws.rsplit('/').next().filter(|s| !s.is_empty())
}

async fn close_tab(port: u16, id: &str) {
    if let Ok(c) = reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        let _ = c.get(format!("http://127.0.0.1:{port}/json/close/{id}")).send().await;
    }
}

pub async fn drive(cfg: &DriveConfig, on: &mut (dyn FnMut(f64, &str) + Send)) -> anyhow::Result<DriveOutcome> {
    let sel = &cfg.selectors;
    if cfg.auto_publish && cfg.require_confirmation && sel.success_url.is_empty() && sel.success_selector.is_empty() {
        // до клика: нажать «Опубликовать» и не узнать, вышел ли ролик, хуже, чем не нажимать
        bail!("Не задан признак успешной публикации (адрес или элемент страницы): подтвердить выход ролика нечем, поэтому ничего не отправлено");
    }
    let captcha = captcha_expr(&sel.captcha);
    on(0.02, "Открываю браузер");
    let port = ensure_browser(&cfg.chromium, &cfg.profile, cfg.headless, "about:blank").await?;
    let ws = open_tab(port, &cfg.upload_url).await?;
    let tab = tab_id(&ws).map(str::to_string);
    let mut cdp = Connected::new(&ws).await?;
    let cdp = &mut cdp.0;
    cdp.call("Page.enable", json!({})).await?;
    cdp.call("DOM.enable", json!({})).await?;

    // страница загрузки появляется только после входа: ждём поле выбора файла, пока пользователь входит
    let has_input = format!("!!document.querySelector({})", js(&sel.file_input));
    on(0.08, "Жду страницу загрузки TikTok. Если попросит, войди в аккаунт в окне браузера");
    wait_for(cdp, &has_input, cfg.login_timeout, "Не дождался страницы загрузки TikTok: войди в аккаунт в окне браузера и повтори отправку", captcha.as_deref()).await?;

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
    wait_for(cdp, &has_caption, cfg.step_timeout, "TikTok не показал поле подписи. Возможно, изменилась вёрстка страницы", captcha.as_deref()).await?;

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
        // Поле TikTok могло принять не всё (лимит, обработчик): сверяем, что в нём ровно наш текст, до клика, а не после.
        // Пробелы и переводы строк не считаем: редактор раскладывает их по абзацам по-своему.
        let expected = cfg.caption.chars().filter(|c| !c.is_whitespace()).count();
        let read = format!("(() => {{ const el = document.querySelector({}); return el ? [...(el.innerText || '')].filter(c => !/\\s/.test(c)).length : -1; }})()", js(&sel.caption));
        let mut got = -1;
        for _ in 0..10 {
            got = cdp.eval(&read).await?.as_i64().unwrap_or(-1);
            if got == expected as i64 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if got != expected as i64 {
            bail!("Текст в поле подписи не совпал с отправленным: в поле {got} знаков без пробелов, а должно быть {expected}. TikTok мог обрезать подпись, поэтому ролик не опубликован");
        }
    }

    if !cfg.auto_publish {
        on(1.0, "Файл и подпись внесены. Проверь и нажми «Опубликовать» в окне TikTok");
        return Ok(DriveOutcome { clicked_post: false, confirmed: false });
    }

    on(0.85, "Жду, когда станет доступна кнопка «Опубликовать»");
    let ready = format!("(() => {{ const b = document.querySelector({}); return !!b && !b.disabled && b.getAttribute('aria-disabled') !== 'true'; }})()", js(&sel.post_button));
    wait_for(cdp, &ready, cfg.step_timeout, "Кнопка «Опубликовать» не стала доступной: TikTok ещё обрабатывает файл или изменилась вёрстка", captcha.as_deref()).await?;
    if cdp.eval(&format!("(() => {{ const b = document.querySelector({}); if (!b) return false; b.click(); return true; }})()", js(&sel.post_button))).await?.as_bool() != Some(true) {
        bail!("Кнопка «Опубликовать» пропала");
    }
    if !cfg.require_confirmation {
        on(1.0, "Нажата кнопка «Опубликовать». Проверь результат в окне TikTok");
        return Ok(DriveOutcome { clicked_post: true, confirmed: false });
    }

    // Кнопка нажата: с этой минуты любой сбой означает «мог выйти», а не «не вышел».
    on(0.95, "Нажата «Опубликовать», жду подтверждения от TikTok");
    let success = format!(
        "(() => {{ const u = {}; const s = {}; return (u !== '' && location.href.includes(u)) || (s !== '' && !!document.querySelector(s)); }})()",
        js(&sel.success_url),
        js(&sel.success_selector)
    );
    let confirm = (!sel.confirm_button.trim().is_empty()).then(|| {
        format!("(() => {{ const b = document.querySelector({}); if (!b) return false; const r = b.getBoundingClientRect(); if (r.width < 1 || r.height < 1) return false; b.click(); return true; }})()", js(&sel.confirm_button))
    });
    let end = tokio::time::Instant::now() + cfg.confirm_timeout;
    let mut confirm_clicks = 0;
    loop {
        // страница в этот момент переходит на другой адрес, и запрос может не пройти: это не провал, спрашиваем снова
        if cdp.eval(&success).await.is_ok_and(|v| v.as_bool() == Some(true)) {
            break;
        }
        if let Some(c) = &captcha {
            if cdp.eval(c).await.unwrap_or(Value::Null).as_bool() == Some(true) {
                return Err(anyhow::Error::new(AfterClick(format!("{CAPTCHA_TEXT}. «Опубликовать» уже нажато: проверь в TikTok Studio, вышел ли ролик"))));
            }
        }
        if let (Some(c), true) = (&confirm, confirm_clicks < 3) {
            if cdp.eval(c).await.unwrap_or(Value::Null).as_bool() == Some(true) {
                confirm_clicks += 1;
                on(0.97, "Подтверждаю публикацию в окне TikTok");
            }
        }
        if tokio::time::Instant::now() >= end {
            return Err(anyhow::Error::new(AfterClick(format!(
                "TikTok не подтвердил публикацию за {} с. «Опубликовать» нажато: проверь в TikTok Studio, вышел ли ролик",
                cfg.confirm_timeout.as_secs()
            ))));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    on(1.0, "TikTok подтвердил публикацию");
    if cfg.close_on_success {
        if let Some(id) = &tab {
            close_tab(port, id).await;
        }
    }
    Ok(DriveOutcome { clicked_post: true, confirmed: true })
}

struct Connected(Cdp);

impl Connected {
    async fn new(ws: &str) -> anyhow::Result<Self> {
        Ok(Self(Cdp::connect(ws).await?))
    }
}

// ---------------------------------------------------------------- стык с приложением

/// Длина подписи, которую принимает страница загрузки TikTok Studio (у API `title` короче: 2200). Сверена по сторонним
/// источникам, не по живой странице: ошибка здесь поймается сверкой текста в поле до клика (см. `drive`).
pub const CAPTION_LIMIT: usize = 4000;

/// Что и как отправить через страницу загрузки.
pub(crate) struct BrowserPost {
    pub file: PathBuf,
    pub caption: String,
    pub auto_publish: bool,
    /// Дождаться подтверждения публикации от TikTok. Для очереди обязательно.
    pub require_confirmation: bool,
    pub close_on_success: bool,
    /// Профиль Chromium аккаунта, с которого идёт выкладка: в нём лежит вход в TikTok.
    pub profile: PathBuf,
}

/// Окно Chromium с профилем аккаунта на странице загрузки, чтобы человек вошёл в TikTok. Не ждёт входа:
/// окно остаётся открытым, а очередь потом пользуется тем же профилем.
pub(crate) async fn open_login_window(chromium: &Path, profile: &Path) -> anyhow::Result<()> {
    let port = ensure_browser(chromium, profile, false, "about:blank").await?;
    open_tab(port, &upload_url()).await?;
    Ok(())
}

fn upload_url() -> String {
    #[cfg(debug_assertions)]
    {
        std::env::var("AVTO_TIKTOK_UPLOAD_URL").unwrap_or_else(|_| UPLOAD_URL.into())
    }
    #[cfg(not(debug_assertions))]
    {
        UPLOAD_URL.to_string()
    }
}

/// Один ролик через браузер, с событиями хода `post-progress`. Записей в журнал не делает: это забота вызывающего.
pub(crate) async fn run_browser_post(app: &AppHandle, draft_id: i64, p: BrowserPost) -> anyhow::Result<DriveOutcome> {
    let chromium = find_chromium().ok_or_else(|| anyhow!("Не найден Chromium. Установи пакет chromium или Google Chrome"))?;
    // по одной выкладке за раз: очередь и ручной диалог не пишут в браузер одновременно
    let state = app.state::<AppState>();
    let _turn = state.browser_gate.lock().await;
    #[allow(unused_mut)] // в release подмен нет
    let mut selectors = Selectors::default();
    #[cfg(debug_assertions)]
    {
        // макет страницы для проверок без настоящего TikTok
        if let Ok(v) = std::env::var("AVTO_TIKTOK_SUCCESS_URL") {
            selectors.success_url = v;
        }
        if let Ok(v) = std::env::var("AVTO_TIKTOK_CONFIRM") {
            selectors.confirm_button = v;
        }
    }
    // в проверках без человека окно браузера не нужно (только debug-сборка)
    let headless = cfg!(debug_assertions) && std::env::var("AVTO_TIKTOK_HEADLESS").is_ok_and(|v| v == "1");
    let cfg = DriveConfig {
        chromium,
        profile: p.profile,
        headless,
        upload_url: upload_url(),
        file: p.file,
        caption: p.caption,
        auto_publish: p.auto_publish,
        selectors,
        login_timeout: Duration::from_secs(600),
        step_timeout: Duration::from_secs(180),
        require_confirmation: p.require_confirmation,
        confirm_timeout: Duration::from_secs(120),
        close_on_success: p.close_on_success,
    };
    let reporter = app.clone();
    drive(&cfg, &mut |fraction, message| {
        let _ = reporter.emit("post-progress", PostProgress { draft_id, fraction, message: message.to_string() });
    })
    .await
}

pub(crate) async fn start(app: AppHandle, state: State<'_, AppState>, args: PublishArgs, file: PathBuf, auto_publish: bool) -> Result<PublishStart, String> {
    find_chromium().ok_or("Не найден Chromium. Установи пакет chromium или выбери способ «через API»")?;
    // ручной диалог выкладывает с основного аккаунта: его вход лежит в его профиле
    let (post_id, caption, account, profile) = {
        let db = state.db.0.lock().unwrap();
        let d = get_draft(&db, args.draft_id)?;
        db.execute(
            "UPDATE moments SET caption = ?2, hashtags = ?3 WHERE id = ?1",
            rusqlite::params![d.moment_id, args.caption.trim(), serde_json::to_string(&crate::tiktok::api::normalize_hashtags(&args.hashtags)).unwrap_or_else(|_| "[]".into())],
        )
        .map_err(|e| e.to_string())?;
        let account = accounts::main_id(&db);
        let profile = accounts::profile_dir(&db, &state.data_dir, account)?;
        (begin_post(&db, args.draft_id, "browser", "browser").map_err(|e| e.to_string())?, make_caption(&args.caption, &args.hashtags, CAPTION_LIMIT), account, profile)
    };
    state.posting.lock().unwrap().insert(args.draft_id);
    let draft_id = args.draft_id;

    tauri::async_runtime::spawn(async move {
        let result = run_browser_post(&app, draft_id, BrowserPost { file, caption, auto_publish, require_confirmation: false, close_on_success: false, profile }).await;

        let state = app.state::<AppState>();
        state.posting.lock().unwrap().remove(&draft_id);
        let stored = match &result {
            Ok(o) if o.clicked_post => Ok(("Нажата кнопка «Опубликовать» в окне TikTok. Проверь, что ролик вышел.".to_string(), None)),
            Ok(_) => Ok(("Файл и подпись внесены в окне TikTok. Осталось проверить и нажать «Опубликовать».".to_string(), None)),
            Err(e) => Err(format!("{e:#}")),
        };
        {
            let db = state.db.0.lock().unwrap();
            finish_post(&db, post_id, &stored);
            if matches!(&result, Ok(o) if o.clicked_post) {
                mark_sent(&db, post_id, account);
            }
        }
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
