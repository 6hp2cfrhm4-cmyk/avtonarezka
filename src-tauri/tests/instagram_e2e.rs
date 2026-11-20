//! Только loopback Graph/rupload. Проверяем отсутствие повтора media_publish.
use avtonarezka_lib::publish::{instagram::Instagram, FailureKind, Platform, Upload};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Rec {
    base: String,
    init: usize,
    uploads: usize,
    checks: usize,
    published: usize,
    used: u64,
    total: u64,
    mode: String,
    caption: String,
}
type Shared = Arc<Mutex<Rec>>;
async fn identity(h: HeaderMap) -> Json<Value> {
    assert_eq!(h["authorization"], "Bearer TEST_TOKEN");
    Json(json!({"id":"12345","username":"test_creator","account_type":"MEDIA_CREATOR"}))
}
async fn limit(State(s): State<Shared>) -> Response {
    let r = s.lock().unwrap();
    if r.mode == "auth" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":190,"message":"TEST_TOKEN must never enter logs"}})),
        )
            .into_response();
    }
    if r.mode == "429" {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "12")],
            Json(json!({"error":{"code":4}})),
        )
            .into_response();
    }
    Json(json!({"data":[{"quota_usage":r.used,"config":{"quota_total":r.total,"quota_duration":86400}}]})).into_response()
}
async fn init(State(s): State<Shared>, h: HeaderMap, Json(v): Json<Value>) -> Json<Value> {
    assert_eq!(h["authorization"], "Bearer TEST_TOKEN");
    assert_eq!(v["media_type"], "REELS");
    assert_eq!(v["upload_type"], "resumable");
    let mut r = s.lock().unwrap();
    r.init += 1;
    r.caption = v["caption"].as_str().unwrap().into();
    let uri = if r.mode == "foreign" {
        "https://example.com/leak".into()
    } else {
        format!("{}/ig-api-upload/v27.0/111", r.base)
    };
    Json(json!({"id":"111","uri":uri}))
}
async fn upload(State(s): State<Shared>, h: HeaderMap, body: Bytes) -> Response {
    assert_eq!(h["authorization"], "OAuth TEST_TOKEN");
    assert_eq!(h["offset"], "0");
    assert_eq!(
        h["file_size"].to_str().unwrap().parse::<usize>().unwrap(),
        body.len()
    );
    assert!(body.iter().all(|b| *b == b'v'));
    let mut r = s.lock().unwrap();
    r.uploads += 1;
    if r.mode == "upload5xx" {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({}))).into_response();
    }
    Json(json!({"success":true})).into_response()
}
async fn status(State(s): State<Shared>) -> Json<Value> {
    let mut r = s.lock().unwrap();
    r.checks += 1;
    let code = match r.mode.as_str() {
        "expired" => "EXPIRED",
        "error" => "ERROR",
        "timeout" => "IN_PROGRESS",
        _ if r.checks < 2 => "IN_PROGRESS",
        _ => "FINISHED",
    };
    Json(json!({"status_code":code}))
}
async fn publish(State(s): State<Shared>, h: HeaderMap, Json(v): Json<Value>) -> Response {
    assert_eq!(h["authorization"], "Bearer TEST_TOKEN");
    assert_eq!(v["creation_id"], "111");
    let mut r = s.lock().unwrap();
    r.published += 1;
    if r.mode == "publish5xx" {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({}))).into_response();
    }
    Json(json!({"id":"222"})).into_response()
}
async fn link(State(s): State<Shared>) -> Response {
    if s.lock().unwrap().mode == "nolink" {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({}))).into_response();
    }
    Json(json!({"permalink":"https://www.instagram.com/reel/confirmed/"})).into_response()
}
async fn server(mode: &str) -> (Instagram, Shared, tokio::task::JoinHandle<()>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    let s = Arc::new(Mutex::new(Rec {
        base: base.trim_end_matches('/').into(),
        total: 7,
        mode: mode.into(),
        ..Default::default()
    }));
    let app = Router::new()
        .route("/v27.0/12345", get(identity))
        .route("/v27.0/12345/content_publishing_limit", get(limit))
        .route("/v27.0/12345/media", post(init))
        .route("/ig-api-upload/v27.0/111", post(upload))
        .route("/v27.0/111", get(status))
        .route("/v27.0/12345/media_publish", post(publish))
        .route("/v27.0/222", get(link))
        .layer(DefaultBodyLimit::disable())
        .with_state(s.clone());
    (
        Instagram::with_endpoints("TEST_TOKEN".into(), "12345".into(), &base, &base).unwrap(),
        s,
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() }),
    )
}
fn clip(name: &str) -> Upload {
    let path =
        std::env::temp_dir().join(format!("avto-instagram-{}-{name}.mp4", std::process::id()));
    std::fs::write(&path, vec![b'v'; 80_000]).unwrap();
    Upload {
        path,
        caption: "Момент\nИсточник: Автор\nПромокод: КОД".into(),
        duration: 30.0,
        scheduled_at: None,
    }
}
#[tokio::test]
async fn container_stream_poll_publish_receipt_and_optional_permalink() {
    for mode in ["ok", "nolink"] {
        let (client, s, task) = server(mode).await;
        let u = clip(mode);
        let receipt = client.publish(&u, &mut |_, _| {}).await.unwrap();
        assert_eq!(receipt.external_id, "222");
        assert!(receipt.published);
        assert_eq!(receipt.url.is_none(), mode == "nolink");
        let r = s.lock().unwrap();
        assert_eq!((r.init, r.uploads, r.checks, r.published), (1, 1, 2, 1));
        assert_eq!(r.caption, u.caption);
        std::fs::remove_file(u.path).unwrap();
        task.abort();
    }
}
#[tokio::test]
async fn dynamic_quota_auth_and_retry_after_stop_before_container() {
    for mode in ["quota", "auth", "429"] {
        let (client, s, task) = server(mode).await;
        if mode == "quota" {
            s.lock().unwrap().used = 7;
        }
        let u = clip(mode);
        let e = client.publish(&u, &mut |_, _| {}).await.unwrap_err();
        assert_eq!(
            e.kind,
            match mode {
                "auth" => FailureKind::Auth,
                "429" => FailureKind::RetryAfter(12),
                _ => FailureKind::RetryAfter(3600),
            }
        );
        assert!(!e.message.contains("TEST_TOKEN"));
        assert_eq!(s.lock().unwrap().init, 0);
        std::fs::remove_file(u.path).unwrap();
        task.abort();
    }
}
#[tokio::test]
async fn uncertain_response_does_not_replay_publish_and_processing_is_bounded() {
    for mode in [
        "upload5xx",
        "publish5xx",
        "timeout",
        "expired",
        "error",
        "foreign",
    ] {
        let (client, s, task) = server(mode).await;
        let u = clip(mode);
        let e = client.publish(&u, &mut |_, _| {}).await.unwrap_err();
        assert_eq!(
            e.kind,
            if ["expired", "error", "foreign"].contains(&mode) {
                FailureKind::Rejected
            } else {
                FailureKind::NeedsCheck
            }
        );
        let r = s.lock().unwrap();
        assert_eq!(r.init, 1);
        assert_eq!(r.published, usize::from(mode == "publish5xx"));
        assert!(r.checks <= 6);
        if mode == "foreign" {
            assert_eq!(r.uploads, 0);
        }
        std::fs::remove_file(u.path).unwrap();
        task.abort();
    }
}
#[test]
fn refuses_foreign_endpoints_credentials_and_mixed_test_production() {
    for base in [
        "https://example.com/",
        "https://graph.facebook.com@evil.example/",
        "https://graph.facebook.com/path",
        "http://graph.facebook.com/",
    ] {
        assert!(Instagram::with_endpoints(
            "TOKEN".into(),
            "123".into(),
            base,
            "https://rupload.facebook.com/"
        )
        .is_err());
    }
    assert!(Instagram::with_endpoints(
        "TOKEN".into(),
        "123".into(),
        "http://127.0.0.1:42/",
        "https://rupload.facebook.com/"
    )
    .is_err());
    assert!(Instagram::new("TOKEN\n".into(), "123".into()).is_err());
}
