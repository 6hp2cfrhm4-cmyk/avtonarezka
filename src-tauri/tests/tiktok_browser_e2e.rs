//! Способ «через браузер» на настоящем Chromium (без окна) и локальной странице-макете.
//! Страница ведёт себя как форма загрузки: поле файла появляется не сразу (как после входа в аккаунт),
//! редактор подписи и кнопка — только после выбора файла. Настоящий TikTok не затрагивается.
//! Пропускается, если Chromium не установлен.

use std::{path::PathBuf, time::Duration};

use avtonarezka_lib::tiktok::browser::{drive, find_chromium, Cdp, DriveConfig, Selectors};
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

const NEVER: &str = "<!doctype html><meta charset=utf-8><title>вход</title><p>Нужно войти в аккаунт</p>";

async fn serve() -> String {
    let app = Router::new().route("/ok", get(|| async { Html(PAGE) })).route("/never", get(|| async { Html(NEVER) }));
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
    }
}

fn port_of(e: &Env) -> u16 {
    std::fs::read_to_string(e.profile.join("DevToolsActivePort")).unwrap().lines().next().unwrap().trim().parse().unwrap()
}

/// Состояние страницы макета глазами постороннего наблюдателя.
async fn inspect(e: &Env) -> Value {
    let port = port_of(e);
    let list: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list")).await.unwrap().json().await.unwrap();
    let ws = list.as_array().unwrap().iter().find(|t| t["type"] == "page" && t["url"].as_str().is_some_and(|u| u.ends_with("/ok"))).expect("вкладка макета не найдена")["webSocketDebuggerUrl"].as_str().unwrap().to_string();
    let mut cdp = Cdp::connect(&ws).await.unwrap();
    cdp.eval("({ file: document.getElementById('f')?.files[0]?.name ?? null, size: document.getElementById('f')?.files[0]?.size ?? null, caption: document.getElementById('cap')?.innerText ?? null, posted: window.__posted, title: document.title })").await.unwrap()
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
