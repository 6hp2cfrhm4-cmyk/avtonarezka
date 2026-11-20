//! Общий браузерный механизм на настоящем Chromium и только loopback-странице.
use avtonarezka_lib::publish::browser::{
    drive, find_chromium, AfterClick, Cdp, DriveConfig, NeedsHuman, Selectors,
};
use axum::{response::Html, routing::get, Router};
use serde_json::{json, Value};
use std::time::Duration;

const PAGE: &str = r#"<!doctype html><meta charset=utf-8><title>Изолированный публикатор</title>
<input type=file id=file><div id=wrap></div><script>
const q=new URLSearchParams(location.search);window.clicks=0;
function captcha(){const c=document.createElement('div');c.id='captcha';c.style='width:200px;height:100px';document.body.append(c);}
if(q.has('captcha_start'))captcha();
document.querySelector('#file').onchange=()=>{
 const e=document.createElement(q.get('editor')||'textarea');e.id='text';
 if(e.tagName==='DIV'){e.contentEditable='true';e.style.whiteSpace='pre-wrap';e.innerText='старое имя.mp4';}else{e.value='старое имя.mp4';}
 if(q.has('tamper'))e.oninput=()=>{if(e.tagName==='DIV')e.innerText='чужой текст';else e.value='чужой текст';};
 const b=document.createElement('button');b.id='publish';b.textContent='Опубликовать';
 b.onclick=()=>{window.clicks++;if(q.has('captcha_after'))captcha();else if(!q.has('never')){const done=document.createElement('p');done.id='done';done.textContent='Готово';document.body.append(done);}};
 document.querySelector('#wrap').append(e,b);
 if(q.has('upload_error')){const x=document.createElement('p');x.id='upload-error';document.body.append(x);b.disabled=true;}
};</script>"#;

struct Env {
    cfg: DriveConfig,
    task: tokio::task::JoinHandle<()>,
}
impl Env {
    async fn new(tag: &str, query: &str) -> Option<Self> {
        let chromium = find_chromium().or_else(|| {
            eprintln!("Chromium не установлен; браузерный тест пропущен");
            None
        })?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/?{}", listener.local_addr().unwrap(), query);
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/", get(|| async { Html(PAGE) })),
            )
            .await
            .unwrap()
        });
        let dir =
            std::env::temp_dir().join(format!("avto-common-browser-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clip.mp4");
        std::fs::write(&file, [7u8; 100]).unwrap();
        Some(Self {
            cfg: DriveConfig {
                label: "Тестовая площадка".into(),
                caption_limit: 5000,
                upload_failed_expression: "!!document.querySelector('#upload-error')".into(),
                chromium,
                profile: dir.join("profile"),
                headless: true,
                upload_url: url,
                file,
                caption: "Своя подпись «с кавычками»\nВторая строка 😀".into(),
                auto_publish: true,
                selectors: Selectors {
                    file_input: "#file".into(),
                    caption: "#text".into(),
                    post_button: "#publish".into(),
                    confirm_button: String::new(),
                    success_url: String::new(),
                    success_selector: "#done".into(),
                    captcha: "#captcha".into(),
                },
                login_timeout: Duration::from_secs(10),
                step_timeout: Duration::from_secs(10),
                require_confirmation: true,
                confirm_timeout: Duration::from_secs(2),
                close_on_success: false,
            },
            task,
        })
    }
    fn port(&self) -> u16 {
        std::fs::read_to_string(self.cfg.profile.join("DevToolsActivePort"))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }
    async fn inspect(&self) -> Value {
        let pages: Value = reqwest::get(format!("http://127.0.0.1:{}/json/list", self.port()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let ws = pages
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["url"].as_str() == Some(self.cfg.upload_url.as_str()))
            .unwrap()["webSocketDebuggerUrl"]
            .as_str()
            .unwrap();
        Cdp::connect(ws).await.unwrap().eval("({clicks:window.clicks,text:document.querySelector('#text')?.value??document.querySelector('#text')?.innerText,file:document.querySelector('#file').files[0]?.name})").await.unwrap()
    }
    async fn close(self) {
        if self.cfg.profile.join("DevToolsActivePort").is_file() {
            let version: Value =
                reqwest::get(format!("http://127.0.0.1:{}/json/version", self.port()))
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
            if let Ok(mut cdp) =
                Cdp::connect(version["webSocketDebuggerUrl"].as_str().unwrap()).await
            {
                let _ = cdp.call("Browser.close", json!({})).await;
            }
        }
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn different_platform_editors_and_limits_share_the_real_driver() {
    for editor in ["textarea", "input", "div"] {
        let Some(mut e) = Env::new(editor, &format!("editor={editor}")).await else {
            return;
        };
        if editor == "input" {
            e.cfg.caption = "Подпись без перевода строки 😀".into();
        }
        // 4500 exceeds TikTok's limit; the shared driver uses the adapter's limit.
        if editor == "textarea" {
            e.cfg.caption = "Я".repeat(4500);
        }
        let mut steps = vec![];
        let out = drive(&e.cfg, &mut |_, text| steps.push(text.to_owned()))
            .await
            .unwrap();
        let actual = e.inspect().await;
        assert!(out.clicked_post && out.confirmed);
        assert_eq!(actual["clicks"], 1);
        assert_eq!(actual["text"], e.cfg.caption);
        assert_eq!(actual["file"], "clip.mp4");
        assert!(steps.iter().any(|s| s.contains("Тестовая площадка")));
        e.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_caption_is_verified_for_every_editor_before_manual_handoff() {
    for editor in ["textarea", "input", "div"] {
        let Some(mut e) = Env::new(&format!("empty-{editor}"), &format!("editor={editor}")).await
        else {
            return;
        };
        e.cfg.caption.clear();
        e.cfg.auto_publish = false;
        let out = drive(&e.cfg, &mut |_, _| {}).await.unwrap();
        let state = e.inspect().await;
        assert!(!out.clicked_post && !out.confirmed);
        assert_eq!(state["clicks"], 0);
        assert_eq!(state["text"].as_str().unwrap().trim(), "");
        e.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn human_checks_upload_failure_and_ambiguous_click_keep_their_meaning() {
    for (tag, q) in [
        ("captcha_start", "captcha_start"),
        ("captcha_after", "captcha_after"),
        ("never", "never"),
        ("tamper", "tamper"),
        ("upload_error", "upload_error"),
    ] {
        let Some(e) = Env::new(tag, q).await else {
            return;
        };
        let err = drive(&e.cfg, &mut |_, _| {}).await.unwrap_err();
        let state = e.inspect().await;
        match tag {
            "captcha_start" => {
                assert!(err.is::<NeedsHuman>());
                assert_eq!(state["clicks"], 0);
            }
            "captcha_after" | "never" => {
                assert!(err.is::<AfterClick>());
                assert_eq!(state["clicks"], 1);
            }
            _ => {
                assert!(!err.is::<AfterClick>());
                assert_eq!(state["clicks"], 0);
            }
        }
        e.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_configuration_does_not_start_a_browser() {
    let Some(mut e) = Env::new("validation", "").await else {
        return;
    };
    e.cfg.caption_limit = 2;
    assert!(drive(&e.cfg, &mut |_, _| {}).await.is_err());
    e.cfg.caption_limit = 5000;
    e.cfg.selectors.success_selector.clear();
    assert!(drive(&e.cfg, &mut |_, _| {}).await.is_err());
    e.cfg.selectors.success_selector = "#done".into();
    e.cfg.upload_url = "file:///tmp/test.html".into();
    assert!(drive(&e.cfg, &mut |_, _| {}).await.is_err());
    assert!(!e.cfg.profile.exists());
    e.close().await;
}
