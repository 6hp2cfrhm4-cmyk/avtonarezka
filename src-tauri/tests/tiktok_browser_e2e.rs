//! Способ «через браузер» на настоящем Chromium (без окна) и локальной странице-макете.
//! Страница ведёт себя как форма загрузки: поле файла появляется не сразу (как после входа в аккаунт),
//! редактор подписи и кнопка — только после выбора файла. Настоящий TikTok не затрагивается.
//! Пропускается, если Chromium не установлен.

use std::{path::PathBuf, time::Duration};

use avtonarezka_lib::tiktok::browser::{drive, find_chromium, AfterClick, Cdp, DriveConfig, NeedsHuman, Selectors};
use axum::{response::Html, routing::get, Router};
use serde_json::{json, Value};

const PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>макет загрузки</title>
<p id=state>Войдите в аккаунт…</p>
<div id=wrap></div>
<script>
  window.__posted = 0;
  // поле файла появляется через 1.2 с — как страница загрузки после входа
  setTimeout(() => {
    document.getElementById('state').textContent = 'Загрузка видео';
    const f = document.createElement('input');
    f.type = 'file'; f.id = 'f';
    f.addEventListener('change', () => setTimeout(() => {
      const ed = document.createElement('div');
      ed.contentEditable = 'true'; ed.id = 'cap'; ed.textContent = 'clip.mp4';
      const b = document.createElement('button');
      b.setAttribute('data-e2e', 'post_video_button'); b.id = 'post'; b.textContent = 'Опубликовать';
      b.disabled = true; setTimeout(() => b.disabled = false, 600);
      b.addEventListener('click', () => { window.__posted++; document.title = 'POSTED'; });
      document.getElementById('wrap').append(ed, b);
    }, 400));
    document.getElementById('wrap').append(f);
  }, 1200);
</script>"#;

/// Страница «как Studio»: поле с обрезкой по лимиту, окно подтверждения, проверка «я не робот» и переход после публикации.
/// Поведение выбирается параметрами адреса: `limit=N` (поле принимает N знаков без пробелов), `confirm=1` (после «Опубликовать»
/// нужно нажать «Опубликовать сейчас»), `captcha=1` (после клика появляется проверка), `captcha_at_start=1` (проверка вместо формы),
/// `never=1` (клик принят, но публикации нет).
const STUDIO: &str = r#"<!doctype html><meta charset=utf-8><title>макет Studio</title>
<p id=state>Войдите в аккаунт…</p>
<div id=wrap></div>
<script>
  const Q = new URLSearchParams(location.search);
  window.__posted = 0; window.__clicks = 0;
  const limit = Number(Q.get('limit') || 0);
  const showCaptcha = () => {
    const c = document.createElement('div');
    c.id = 'captcha-verify'; c.style.cssText = 'width:300px;height:200px;background:#ccc'; c.textContent = 'Проверка';
    document.body.append(c);
  };
  if (Q.get('captcha_at_start')) showCaptcha();
  else setTimeout(() => {
    document.getElementById('state').textContent = 'Загрузка видео';
    const f = document.createElement('input');
    f.type = 'file'; f.id = 'f';
    f.addEventListener('change', () => setTimeout(() => {
      const ed = document.createElement('div');
      ed.contentEditable = 'true'; ed.id = 'cap'; ed.textContent = 'clip.mp4';
      if (limit) ed.addEventListener('input', () => {
        let n = 0, out = '';
        for (const ch of ed.innerText) { if (!/\s/.test(ch)) { n++; if (n > limit) break; } out += ch; }
        if (out !== ed.innerText) ed.innerText = out;
      });
      // как TikTok: при ошибке загрузки появляется окно, кнопка остаётся неактивной
      if (Q.get('upload_fails')) { const m = document.createElement('div'); m.textContent = 'Не удалось загрузить видео. Повторите попытку позже'; document.body.append(m); }
      const b = document.createElement('button');
      b.setAttribute('data-e2e', 'post_video_button'); b.id = 'post'; b.textContent = 'Опубликовать';
      b.disabled = true; if (!Q.get('upload_fails')) setTimeout(() => b.disabled = false, 600);
      b.addEventListener('click', () => {
        window.__clicks++;
        if (Q.get('captcha')) return showCaptcha();
        if (Q.get('never')) return;
        const finish = () => { window.__posted++; document.title = 'POSTED'; location.hash = '#posted'; };
        if (!Q.get('confirm')) return finish();
        const ok = document.createElement('button');
        ok.id = 'confirm'; ok.textContent = 'Опубликовать сейчас'; ok.style.cssText = 'width:140px;height:40px';
        ok.addEventListener('click', finish);
        document.body.append(ok);
      });
      document.getElementById('wrap').append(ed, b);
    }, 400));
    document.getElementById('wrap').append(f);
  }, 1200);
</script>"#;

const NEVER: &str = "<!doctype html><meta charset=utf-8><title>вход</title><p>Нужно войти в аккаунт</p>";

async fn serve() -> String {
    let app = Router::new()
        .route("/ok", get(|| async { Html(PAGE) }))
        .route("/studio", get(|| async { Html(STUDIO) }))
        .route("/never", get(|| async { Html(NEVER) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

struct Env {
    chromium: PathBuf,
    profile: PathBuf,
    file: PathBuf,
}

fn env(tag: &str) -> Option<Env> {
    let chromium = find_chromium().or_else(|| {
        eprintln!("Chromium не установлен — тест пропущен");
        None
    })?;
    let dir = std::env::temp_dir().join(format!("avto-br-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("clip.mp4");
    std::fs::write(&file, vec![7u8; 4096]).unwrap();
    Some(Env { chromium, profile: dir.join("profile"), file })
}

fn cfg(e: &Env, url: String, caption: &str, auto: bool) -> DriveConfig {
    DriveConfig {
        chromium: e.chromium.clone(),
        profile: e.profile.clone(),
        headless: true,
        upload_url: url,
        file: e.file.clone(),
        caption: caption.into(),
        auto_publish: auto,
        selectors: Selectors::default(),
        login_timeout: Duration::from_secs(20),
        step_timeout: Duration::from_secs(10),
        require_confirmation: false,
        confirm_timeout: Duration::from_secs(10),
        close_on_success: false,
    }
}

/// Как очередь: клик, подтверждение страницей и закрытие вкладки.
fn queue_cfg(e: &Env, url: String, caption: &str) -> DriveConfig {
    let mut c = cfg(e, url, caption, true);
    c.require_confirmation = true;
    c.close_on_success = true;
    c.selectors.success_url = "#posted".into();
    c
}

fn port_of(e: &Env) -> u16 {
    std::fs::read_to_string(e.profile.join("DevToolsActivePort")).unwrap().lines().next().unwrap().trim().parse().unwrap()
}

/// Состояние страницы макета глазами постороннего наблюдателя.
async fn inspect(e: &Env) -> Value {
    inspect_at(e, "/ok").await
}

async fn pages_at(e: &Env, path: &str) -> Vec<Value> {
    let port = port_of(e);
    let list: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list")).await.unwrap().json().await.unwrap();
    list.as_array().unwrap().iter().filter(|t| t["type"] == "page" && t["url"].as_str().is_some_and(|u| u.contains(path))).cloned().collect()
}

async fn inspect_at(e: &Env, path: &str) -> Value {
    let tabs = pages_at(e, path).await;
    let ws = tabs.first().expect("вкладка макета не найдена")["webSocketDebuggerUrl"].as_str().unwrap().to_string();
    let mut cdp = Cdp::connect(&ws).await.unwrap();
    cdp.eval("({ file: document.getElementById('f')?.files[0]?.name ?? null, size: document.getElementById('f')?.files[0]?.size ?? null, caption: document.getElementById('cap')?.innerText ?? null, posted: window.__posted, clicks: window.__clicks ?? window.__posted, title: document.title })").await.unwrap()
}

async fn close(e: &Env) {
    let port = port_of(e);
    let v: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/version")).await.unwrap().json().await.unwrap();
    if let Ok(mut cdp) = Cdp::connect(v["webSocketDebuggerUrl"].as_str().unwrap()).await {
        let _ = cdp.call("Browser.close", json!({})).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn file_and_caption_are_filled_and_the_post_button_is_left_to_the_user() {
    let Some(e) = env("manual") else { return };
    let base = serve().await;
    let caption = "Смотрите «до конца» — там #сюрприз 😀";
    let mut steps: Vec<String> = Vec::new();
    let out = drive(&cfg(&e, format!("{base}/ok"), caption, false), &mut |_, m| steps.push(m.to_string())).await.expect("сценарий упал");

    assert!(!out.clicked_post, "кнопку без разрешения нажимать нельзя");
    let s = inspect(&e).await;
    assert_eq!(s["file"], "clip.mp4");
    assert_eq!(s["size"], 4096);
    assert_eq!(s["caption"], caption, "старый текст «clip.mp4» должен быть заменён подписью целиком");
    assert_eq!(s["posted"], 0);
    assert!(steps.iter().any(|m| m.contains("войди в аккаунт")), "{steps:?}");
    assert!(steps.last().unwrap().contains("нажми «Опубликовать»"), "{steps:?}");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_publish_waits_for_the_button_and_clicks_it_exactly_once() {
    let Some(e) = env("auto") else { return };
    let base = serve().await;
    let out = drive(&cfg(&e, format!("{base}/ok"), "Подпись", true), &mut |_, _| {}).await.expect("сценарий упал");
    assert!(out.clicked_post);
    let s = inspect(&e).await;
    assert_eq!((s["posted"].as_i64(), s["title"].as_str(), s["caption"].as_str()), (Some(1), Some("POSTED"), Some("Подпись")));
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_caption_clears_the_editor_instead_of_leaving_the_file_name() {
    let Some(e) = env("empty") else { return };
    let base = serve().await;
    drive(&cfg(&e, format!("{base}/ok"), "", false), &mut |_, _| {}).await.expect("сценарий упал");
    // пустой contenteditable в Chromium хранит служебный <br>, и innerText даёт «\n»: важно лишь, что имени файла там нет
    let state = inspect(&e).await;
    assert_eq!(state["caption"].as_str().unwrap().trim(), "", "{state}");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_that_never_shows_the_upload_form_ends_with_advice_to_log_in() {
    let Some(e) = env("never") else { return };
    let base = serve().await;
    let mut c = cfg(&e, format!("{base}/never"), "x", false);
    c.login_timeout = Duration::from_secs(2);
    let err = drive(&c, &mut |_, _| {}).await.unwrap_err().to_string();
    assert!(err.contains("войди в аккаунт"), "{err}");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_selectors_produce_an_actionable_error_not_a_hang() {
    let Some(e) = env("selectors") else { return };
    let base = serve().await;
    let mut c = cfg(&e, format!("{base}/ok"), "x", false);
    c.selectors.caption = "#нет-такого-поля".into();
    c.step_timeout = Duration::from_secs(2);
    let err = drive(&c, &mut |_, _| {}).await.unwrap_err().to_string();
    assert!(err.contains("поле подписи") && err.contains("вёрстка"), "{err}");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_run_reuses_the_browser_that_is_already_open() {
    let Some(e) = env("reuse") else { return };
    let base = serve().await;
    drive(&cfg(&e, format!("{base}/ok"), "первая", false), &mut |_, _| {}).await.unwrap();
    let port = port_of(&e);
    drive(&cfg(&e, format!("{base}/ok"), "вторая", false), &mut |_, _| {}).await.unwrap();
    assert_eq!(port_of(&e), port, "второй запуск должен был подключиться к тому же браузеру");
    let list: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list")).await.unwrap().json().await.unwrap();
    let tabs = list.as_array().unwrap().iter().filter(|t| t["type"] == "page" && t["url"].as_str().is_some_and(|u| u.ends_with("/ok"))).count();
    assert_eq!(tabs, 2, "каждая отправка открывает свою вкладку");
    close(&e).await;
}

// ---------------------------------------------------------------- режим очереди: подтверждение, проверки, обрезка

/// Описание на 3999 знаков с абзацами и хэштегами, как у очереди.
fn long_caption() -> String {
    let sentence = "Он вернулся домой и не поверил своим глазам. ";
    let mut body = String::new();
    while body.chars().count() < 3900 {
        body.push_str(sentence);
        if body.chars().count() % 7 == 0 {
            body.push('\n');
        }
    }
    let mut s = format!("{}\n#юмор #сериал #тикток #момент #смех", body.trim());
    while s.encode_utf16().count() < 3999 {
        s.insert(0, 'а');
    }
    s.chars().take(3999).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queue_post_is_confirmed_by_the_page_and_the_tab_is_closed() {
    let Some(e) = env("q_ok") else { return };
    let base = serve().await;
    let caption = long_caption();
    assert_eq!(caption.encode_utf16().count(), 3999);
    let out = drive(&queue_cfg(&e, format!("{base}/studio"), &caption), &mut |_, _| {}).await.expect("сценарий упал");
    assert!(out.clicked_post && out.confirmed);
    // вкладка закрывается не мгновенно
    for _ in 0..30 {
        if pages_at(&e, "/studio").await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(pages_at(&e, "/studio").await.is_empty(), "после подтверждённой публикации вкладка закрыта");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_whole_long_caption_lands_in_the_editor_when_the_page_accepts_it() {
    let Some(e) = env("q_len") else { return };
    let base = serve().await;
    let caption = long_caption();
    let mut c = queue_cfg(&e, format!("{base}/studio"), &caption);
    c.close_on_success = false;
    drive(&c, &mut |_, _| {}).await.expect("сценарий упал");
    let s = inspect_at(&e, "/studio").await;
    let got: String = s["caption"].as_str().unwrap().chars().filter(|c| !c.is_whitespace()).collect();
    let want: String = caption.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(got, want, "текст в поле совпал со знака на знак");
    assert_eq!(s["posted"], 1);
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_editor_that_truncates_the_caption_is_caught_before_the_button_is_clicked() {
    let Some(e) = env("q_trunc") else { return };
    let base = serve().await;
    let err = drive(&queue_cfg(&e, format!("{base}/studio?limit=3000"), &long_caption()), &mut |_, _| {}).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("не совпал") && msg.contains("не опубликован"), "{msg}");
    assert!(err.downcast_ref::<AfterClick>().is_none(), "до клика это обычная ошибка, ролик точно не вышел");
    let s = inspect_at(&e, "/studio").await;
    assert_eq!((s["clicks"].as_i64(), s["posted"].as_i64()), (Some(0), Some(0)), "кнопку не нажимали");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_upload_is_reported_as_such_and_no_caption_or_click_follows() {
    let Some(e) = env("q_upload_fail") else { return };
    let base = serve().await;
    let err = drive(&queue_cfg(&e, format!("{base}/studio?upload_fails=1"), "Подпись"), &mut |_, _| {}).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("не принял видео") && msg.contains("можно отправить снова"), "{msg}");
    assert!(err.downcast_ref::<AfterClick>().is_none(), "до клика: ролик точно не вышел, его можно отправить снова");
    let s = inspect_at(&e, "/studio").await;
    assert_eq!(s["clicks"].as_i64(), Some(0), "кнопку не нажимали");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_confirmation_dialog_is_clicked_only_when_its_selector_is_configured() {
    let Some(e) = env("q_confirm") else { return };
    let base = serve().await;
    let mut c = queue_cfg(&e, format!("{base}/studio?confirm=1"), "Подпись");
    c.selectors.confirm_button = "#confirm".into();
    c.close_on_success = false;
    let out = drive(&c, &mut |_, _| {}).await.expect("сценарий упал");
    assert!(out.confirmed);
    assert_eq!(inspect_at(&e, "/studio").await["posted"], 1);
    close(&e).await;

    // без настройки окно остаётся, и очередь честно говорит «проверь»
    let Some(e) = env("q_confirm_off") else { return };
    let mut c = queue_cfg(&e, format!("{base}/studio?confirm=1"), "Подпись");
    c.confirm_timeout = Duration::from_secs(2);
    let err = drive(&c, &mut |_, _| {}).await.unwrap_err();
    assert!(err.downcast_ref::<AfterClick>().is_some(), "{err:#}");
    assert!(format!("{err:#}").contains("Проверь") || format!("{err:#}").contains("проверь"), "{err:#}");
    assert_eq!(inspect_at(&e, "/studio?confirm=1").await["posted"], 0);
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_captcha_instead_of_the_form_stops_for_a_human() {
    let Some(e) = env("q_cap_start") else { return };
    let base = serve().await;
    let mut c = queue_cfg(&e, format!("{base}/studio?captcha_at_start=1"), "x");
    c.login_timeout = Duration::from_secs(20);
    let err = drive(&c, &mut |_, _| {}).await.unwrap_err();
    assert!(err.downcast_ref::<NeedsHuman>().is_some(), "{err:#}");
    assert!(format!("{err:#}").contains("не обходит"), "{err:#}");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_captcha_after_the_click_means_the_video_may_have_gone_out() {
    let Some(e) = env("q_cap_click") else { return };
    let base = serve().await;
    let err = drive(&queue_cfg(&e, format!("{base}/studio?captcha=1"), "x"), &mut |_, _| {}).await.unwrap_err();
    assert!(err.downcast_ref::<AfterClick>().is_some(), "{err:#}");
    assert!(format!("{err:#}").contains("проверь в TikTok Studio"), "{err:#}");
    assert_eq!(inspect_at(&e, "/studio").await["clicks"], 1);
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_that_never_confirms_ends_with_advice_to_check_and_no_second_click() {
    let Some(e) = env("q_never") else { return };
    let base = serve().await;
    let mut c = queue_cfg(&e, format!("{base}/studio?never=1"), "x");
    c.confirm_timeout = Duration::from_secs(2);
    let err = drive(&c, &mut |_, _| {}).await.unwrap_err();
    assert!(err.downcast_ref::<AfterClick>().is_some(), "{err:#}");
    let s = inspect_at(&e, "/studio").await;
    assert_eq!((s["clicks"].as_i64(), s["posted"].as_i64()), (Some(1), Some(0)), "кнопка нажата ровно один раз");
    close(&e).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_success_signal_nothing_is_sent_at_all() {
    let Some(e) = env("q_nosignal") else { return };
    let base = serve().await;
    let mut c = queue_cfg(&e, format!("{base}/studio"), "x");
    c.selectors.success_url.clear();
    c.selectors.success_selector.clear();
    let err = drive(&c, &mut |_, _| {}).await.unwrap_err();
    assert!(format!("{err:#}").contains("признак успешной публикации"), "{err:#}");
    assert!(err.downcast_ref::<AfterClick>().is_none());
    assert!(!e.profile.join("DevToolsActivePort").exists(), "браузер даже не открывали");
}
