//! Только loopback Bot API, никаких настоящих ботов/публикаций.
use avtonarezka_lib::publish::{telegram::Telegram, FailureKind, Platform, Upload};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Rec {
    calls: usize,
    bodies: Vec<Vec<u8>>,
    headers: Vec<HeaderMap>,
    code: u16,
    reply: Value,
}
async fn send(
    State(s): State<Arc<Mutex<Rec>>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let mut r = s.lock().unwrap();
    r.calls += 1;
    r.bodies.push(body.to_vec());
    r.headers.push(headers);
    (StatusCode::from_u16(r.code).unwrap(), Json(r.reply.clone()))
}
async fn server(
    code: u16,
    reply: Value,
) -> (Telegram, Arc<Mutex<Rec>>, tokio::task::JoinHandle<()>) {
    let rec = Arc::new(Mutex::new(Rec {
        code,
        reply,
        ..Default::default()
    }));
    let router = Router::new()
        .route("/bot1:test/sendVideo", post(send))
        .with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        Telegram::with_endpoint("1:test".into(), "@factory_sandbox".into(), url).unwrap(),
        rec,
        task,
    )
}

struct ProbeRec {
    calls: Vec<(String, String)>,
    method: String,
    code: u16,
    reply: Value,
}
async fn probe(
    State(s): State<Arc<Mutex<ProbeRec>>>,
    uri: axum::http::Uri,
    body: Bytes,
) -> impl IntoResponse {
    let method = uri.path().rsplit('/').next().unwrap().to_string();
    let mut rec = s.lock().unwrap();
    rec.calls
        .push((method.clone(), String::from_utf8_lossy(&body).into()));
    let normal = match method.as_str() {
        "getMe" => json!({"id":7,"is_bot":true,"username":"factory_bot"}),
        "getChat" => {
            json!({"id":-100123,"type":"channel","username":"factory_sandbox","title":"Тестовый канал"})
        }
        "getChatMember" => {
            json!({"status":"administrator","user":{"id":7,"is_bot":true},"can_post_messages":true})
        }
        _ => panic!("Unexpected method: {method}"),
    };
    let (code, value) = if rec.method == method {
        (rec.code, rec.reply.clone())
    } else {
        (200, json!({"ok":true,"result":normal}))
    };
    (StatusCode::from_u16(code).unwrap(), Json(value))
}
async fn probe_server(
    method: &str,
    code: u16,
    reply: Value,
    chat: &str,
) -> (Telegram, Arc<Mutex<ProbeRec>>, tokio::task::JoinHandle<()>) {
    let rec = Arc::new(Mutex::new(ProbeRec {
        calls: vec![],
        method: method.into(),
        code,
        reply,
    }));
    let router = Router::new().fallback(post(probe)).with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        Telegram::with_endpoint("1:test".into(), chat.into(), endpoint).unwrap(),
        rec,
        task,
    )
}
#[tokio::test]
async fn readonly_connection_proves_bot_channel_and_post_permission_without_send() {
    for chat in ["@FACTORY_SANDBOX", "-100123"] {
        let (client, rec, task) = probe_server("", 200, Value::Null, chat).await;
        let result = client.check_connection().await.unwrap();
        assert_eq!(result.bot_username, "factory_bot");
        assert_eq!(result.chat_id, "-100123");
        assert_eq!(result.chat_title, "Тестовый канал");
        assert!(result.checked_at > 0);
        let calls = rec.lock().unwrap();
        assert_eq!(
            calls.calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            ["getMe", "getChat", "getChatMember"]
        );
        assert!(calls.calls[0].1.is_empty());
        assert!(
            calls.calls[2].1.contains("chat_id=-100123") && calls.calls[2].1.contains("user_id=7")
        );
        assert!(!serde_json::to_string(&result).unwrap().contains("1:test"));
        task.abort();
    }
}
#[tokio::test]
async fn readonly_connection_rejects_missing_rights_wrong_bot_and_channel_without_retry() {
    for (method, result, expected_calls) in [
        (
            "getMe",
            json!({"id":7,"is_bot":false,"username":"factory_bot"}),
            1,
        ),
        ("getMe", json!({"id":7,"is_bot":true}), 1),
        (
            "getChat",
            json!({"id":-100123,"type":"supergroup","username":"factory_sandbox"}),
            2,
        ),
        (
            "getChat",
            json!({"id":-100123,"type":"channel","username":"another_channel"}),
            2,
        ),
        (
            "getChatMember",
            json!({"status":"member","user":{"id":7,"is_bot":true}}),
            3,
        ),
        (
            "getChatMember",
            json!({"status":"administrator","user":{"id":7,"is_bot":true},"can_post_messages":false}),
            3,
        ),
        (
            "getChatMember",
            json!({"status":"administrator","user":{"id":8,"is_bot":true},"can_post_messages":true}),
            3,
        ),
    ] {
        let (client, rec, task) = probe_server(
            method,
            200,
            json!({"ok":true,"result":result}),
            "@factory_sandbox",
        )
        .await;
        let e = client.check_connection().await.unwrap_err();
        assert!(!e.message.contains("1:test"));
        assert_eq!(rec.lock().unwrap().calls.len(), expected_calls);
        task.abort();
    }
    let (client, rec, task) = probe_server("", 200, Value::Null, "-100124").await;
    assert!(client
        .check_connection()
        .await
        .unwrap_err()
        .message
        .contains("другой канал"));
    assert_eq!(rec.lock().unwrap().calls.len(), 2);
    task.abort();
}
#[tokio::test]
async fn readonly_connection_sanitizes_errors_rate_and_redirects() {
    for (code, reply, kind) in [
        (401, json!({"description":"1:test"}), FailureKind::Auth),
        (
            200,
            json!({"ok":false,"error_code":403,"description":"1:test"}),
            FailureKind::Auth,
        ),
        (
            429,
            json!({"ok":false,"parameters":{"retry_after":999999},"description":"1:test"}),
            FailureKind::RetryAfter(86400),
        ),
        (302, json!({"description":"1:test"}), FailureKind::Rejected),
        (503, json!({"description":"1:test"}), FailureKind::Rejected),
        (
            200,
            json!({"ok":false,"description":"1:test"}),
            FailureKind::Rejected,
        ),
        (
            200,
            json!({"ok":true,"result":{"padding":"x".repeat(65537)}}),
            FailureKind::Rejected,
        ),
    ] {
        let (client, rec, task) = probe_server("getMe", code, reply, "@factory_sandbox").await;
        let e = client.check_connection().await.unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains("1:test"));
        assert_eq!(rec.lock().unwrap().calls.len(), 1);
        task.abort();
    }
}
#[tokio::test]
async fn readonly_connection_validates_input_before_api() {
    for chat in ["0", "@x", "not_a_chat"] {
        let (client, rec, task) = probe_server("", 200, Value::Null, chat).await;
        assert!(client.check_connection().await.is_err());
        assert!(rec.lock().unwrap().calls.is_empty());
        task.abort();
    }
}
fn upload(name: &str) -> Upload {
    let path: PathBuf =
        std::env::temp_dir().join(format!("avto-telegram-{}-{name}.mp4", std::process::id()));
    std::fs::write(&path, b"TEST_VIDEO_BINARY\0\xff").unwrap();
    Upload {
        path,
        caption: "Кредит: тест\nhttps://example.com/?utm_content=42".into(),
        duration: 15.0,
        scheduled_at: None,
    }
}
#[tokio::test]
async fn sends_streamed_multipart_and_receives_stable_message_identity() {
    let (client,rec,task)=server(200,json!({"ok":true,"result":{"message_id":42,"chat":{"id":-100123,"username":"factory_sandbox"}}})).await;
    let u = upload("ok");
    let mut progress = Vec::new();
    let out = client
        .publish(&u, &mut |p, _| progress.push(p))
        .await
        .unwrap();
    assert_eq!(out.external_id, "-100123:42");
    assert_eq!(out.url.as_deref(), Some("https://t.me/factory_sandbox/42"));
    assert!(out.published);
    assert_eq!(progress.last(), Some(&1.0));
    {
        let r = rec.lock().unwrap();
        assert_eq!(r.calls, 1);
        assert!(r.headers[0]["content-type"]
            .to_str()
            .unwrap()
            .contains("multipart/form-data; boundary="));
        let body = &r.bodies[0];
        assert!(body.windows(17).any(|s| s == b"TEST_VIDEO_BINARY"));
        let text = String::from_utf8_lossy(body);
        assert!(text.contains(&u.caption));
        assert!(text.contains("@factory_sandbox"));
        assert!(!text.contains("parse_mode"));
    }
    task.abort();
    let _ = std::fs::remove_file(u.path);
}
#[tokio::test]
async fn classifies_rate_auth_rejected_and_ambiguous_without_duplicate_retry() {
    for (name, code, reply, kind) in [
        (
            "rate",
            429,
            json!({"ok":false,"error_code":429,"parameters":{"retry_after":21}}),
            FailureKind::RetryAfter(21),
        ),
        (
            "auth",
            401,
            json!({"ok":false,"error_code":401}),
            FailureKind::Auth,
        ),
        (
            "rights",
            403,
            json!({"ok":false,"error_code":403}),
            FailureKind::Auth,
        ),
        (
            "bad",
            400,
            json!({"ok":false,"error_code":400}),
            FailureKind::Rejected,
        ),
        ("server", 503, json!({"ok":false}), FailureKind::NeedsCheck),
        (
            "noid",
            200,
            json!({"ok":true,"result":{}}),
            FailureKind::NeedsCheck,
        ),
    ] {
        let (client, r, task) = server(code, reply).await;
        let u = upload(name);
        let err = client.publish(&u, &mut |_, _| {}).await.unwrap_err();
        assert_eq!(err.kind, kind, "{name}");
        assert_eq!(r.lock().unwrap().calls, 1);
        assert!(!err.message.contains("1:test"));
        task.abort();
        let _ = std::fs::remove_file(u.path);
    }
}
#[tokio::test]
async fn guards_caption_file_limit_and_untrusted_endpoint_before_network() {
    let (client, r, task) = server(200, json!({"ok":true})).await;
    let mut u = upload("limits");
    u.caption = "🙂".repeat(600);
    assert_eq!(
        client.publish(&u, &mut |_, _| {}).await.unwrap_err().kind,
        FailureKind::Rejected
    );
    u.caption.clear();
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&u.path)
        .unwrap();
    f.set_len(50_000_001).unwrap();
    assert!(client.publish(&u, &mut |_, _| {}).await.is_err());
    assert_eq!(r.lock().unwrap().calls, 0);
    task.abort();
    let _ = std::fs::remove_file(u.path);
    assert!(Telegram::with_endpoint(
        "1:test".into(),
        "@channel".into(),
        "https://example.com".into()
    )
    .is_err());
}
