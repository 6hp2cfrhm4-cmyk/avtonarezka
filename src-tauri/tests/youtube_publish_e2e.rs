//! Поддельные Google OAuth и resumable API. Ни одного внешнего запроса.
use avtonarezka_lib::publish::{
    youtube::{accounts::Google, Settings, YouTube},
    FailureKind, Platform, Upload,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Rec {
    base: String,
    init: usize,
    probes: usize,
    chunks: Vec<(String, usize)>,
    metadata: Value,
    position: usize,
    fail_once: bool,
    final_ambiguous: bool,
    foreign: bool,
    code: u16,
    form: String,
}
type Shared = Arc<Mutex<Rec>>;
async fn init(State(s): State<Shared>, h: HeaderMap, Json(v): Json<Value>) -> Response {
    assert_eq!(h["authorization"], "Bearer TEST_ACCESS");
    assert_eq!(h["x-upload-content-type"], "video/mp4");
    let mut r = s.lock().unwrap();
    r.init += 1;
    r.metadata = v;
    if r.code != 0 {
        return (StatusCode::from_u16(r.code).unwrap(),[("retry-after","1")],Json(json!({"error":{"errors":[{"reason":if r.code==403{"quotaExceeded"}else{"error"}}]}}))).into_response();
    }
    let loc = if r.foreign {
        "https://example.com/leak".into()
    } else {
        format!("{}/session", r.base)
    };
    (StatusCode::OK, [("location", loc)]).into_response()
}
async fn session(State(s): State<Shared>, h: HeaderMap, body: Bytes) -> Response {
    assert_eq!(h["authorization"], "Bearer TEST_ACCESS");
    let range = h["content-range"].to_str().unwrap().to_string();
    let mut r = s.lock().unwrap();
    if range.starts_with("bytes */") {
        r.probes += 1;
        assert!(body.is_empty());
        if r.final_ambiguous {
            return Json(json!({"id":"final_id_123"})).into_response();
        }
        return (
            StatusCode::PERMANENT_REDIRECT,
            [("range", format!("bytes=0-{}", r.position - 1))],
        )
            .into_response();
    }
    let values = range
        .strip_prefix("bytes ")
        .unwrap()
        .replace(['-', '/'], " ")
        .split_whitespace()
        .map(|s| s.parse::<usize>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        values[0], r.position,
        "должны продолжить с подтверждённой позиции"
    );
    assert_eq!(body.len(), values[1] - values[0] + 1);
    assert!(body.iter().all(|b| *b == b'v'));
    r.position += body.len();
    r.chunks.push((range, body.len()));
    if r.fail_once {
        r.fail_once = false;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if r.position == values[2] {
        if r.final_ambiguous {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Json(json!({"id":"clip_id_123"})).into_response()
    } else {
        (
            StatusCode::PERMANENT_REDIRECT,
            [("range", format!("bytes=0-{}", r.position - 1))],
        )
            .into_response()
    }
}
async fn token(State(s): State<Shared>, body: Bytes) -> Response {
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("client_id=desktop.apps.googleusercontent.com"));
    assert!(text.contains("client_secret=TEST_SECRET"));
    s.lock().unwrap().form = text;
    Json(json!({"access_token":"TEST_ACCESS","refresh_token":"TEST_REFRESH","expires_in":3600,"token_type":"Bearer"})).into_response()
}
async fn channel(h: HeaderMap) -> Json<Value> {
    assert_eq!(h["authorization"], "Bearer TEST_ACCESS");
    Json(json!({"items":[{"id":"UC_test","snippet":{"title":"Канал"}}]}))
}
async fn server() -> (String, Shared, tokio::task::JoinHandle<()>) {
    let rec = Arc::new(Mutex::new(Rec::default()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    rec.lock().unwrap().base = base.clone();
    let app = Router::new()
        .route("/upload/youtube/v3/videos", post(init))
        .route("/session", put(session))
        .route("/token", post(token))
        .route("/youtube/v3/channels", get(channel))
        .layer(DefaultBodyLimit::disable())
        .with_state(rec.clone());
    (
        base,
        rec,
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() }),
    )
}
fn upload(name: &str, bytes: usize) -> Upload {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "avto-youtube-publish-{}-{name}.mp4",
        std::process::id()
    ));
    std::fs::write(&path, vec![b'v'; bytes]).unwrap();
    Upload {
        path,
        caption: "Текст\nИсточник: автор\nhttps://example.com".into(),
        duration: 30.,
        scheduled_at: None,
    }
}
#[tokio::test]
async fn multipart_chunks_probe_same_session_after_server_error() {
    let (base, rec, task) = server().await;
    rec.lock().unwrap().fail_once = true;
    let c = YouTube::with_endpoint(
        "TEST_ACCESS".into(),
        "Заголовок".into(),
        Settings::default(),
        &base,
    )
    .unwrap();
    let u = upload("chunks", 8 * 1024 * 1024 + 311);
    let out = c.publish(&u, &mut |_, _| {}).await.unwrap();
    assert_eq!(out.external_id, "clip_id_123");
    assert!(out.published);
    assert!(out.url.unwrap().contains("clip_id_123"));
    {
        let r = rec.lock().unwrap();
        assert_eq!(r.init, 1);
        assert_eq!(r.probes, 1);
        assert_eq!(r.chunks.len(), 2);
        assert_eq!(r.chunks[0].1, 8 * 1024 * 1024);
        assert_eq!(r.chunks[1].1, 311);
        assert_eq!(r.metadata["status"]["privacyStatus"], "private");
        assert_eq!(r.metadata["snippet"]["description"], u.caption);
    }
    task.abort();
    std::fs::remove_file(u.path).unwrap();
}
#[tokio::test]
async fn uncertain_final_is_confirmed_by_probe_without_resending() {
    let (base, rec, task) = server().await;
    rec.lock().unwrap().final_ambiguous = true;
    let c = YouTube::with_endpoint(
        "TEST_ACCESS".into(),
        "Название".into(),
        Settings::default(),
        &base,
    )
    .unwrap();
    let mut u = upload("final", 10);
    u.scheduled_at = Some(avtonarezka_lib::tiktok::api::now_unix() + 3600);
    let out = c.publish(&u, &mut |_, _| {}).await.unwrap();
    assert_eq!(out.external_id, "final_id_123");
    assert!(!out.published);
    {
        let r = rec.lock().unwrap();
        assert_eq!(r.init, 1);
        assert_eq!(r.chunks.len(), 1);
        assert_eq!(r.probes, 1);
        assert!(r.metadata["status"]["publishAt"]
            .as_str()
            .unwrap()
            .ends_with('Z'));
    }
    task.abort();
    std::fs::remove_file(u.path).unwrap();
}
#[tokio::test]
async fn rate_auth_quota_and_foreign_session_do_not_upload() {
    for (n, code, kind) in [
        ("rate", 429, FailureKind::RetryAfter(1)),
        ("auth", 401, FailureKind::Auth),
        ("quota", 403, FailureKind::Rejected),
    ] {
        let (base, r, t) = server().await;
        r.lock().unwrap().code = code;
        let c = YouTube::with_endpoint(
            "TEST_ACCESS".into(),
            "Заголовок".into(),
            Settings::default(),
            &base,
        )
        .unwrap();
        let u = upload(n, 10);
        let e = c.publish(&u, &mut |_, _| {}).await.unwrap_err();
        assert_eq!(e.kind, kind);
        assert!(!e.message.contains("TEST_ACCESS"));
        assert!(r.lock().unwrap().chunks.is_empty());
        t.abort();
        std::fs::remove_file(u.path).unwrap();
    }
    let (base, r, t) = server().await;
    r.lock().unwrap().foreign = true;
    let c = YouTube::with_endpoint(
        "TEST_ACCESS".into(),
        "Заголовок".into(),
        Settings::default(),
        &base,
    )
    .unwrap();
    let u = upload("foreign", 10);
    assert_eq!(
        c.publish(&u, &mut |_, _| {}).await.unwrap_err().kind,
        FailureKind::NeedsCheck
    );
    assert!(r.lock().unwrap().chunks.is_empty());
    t.abort();
    std::fs::remove_file(u.path).unwrap();
    assert!(YouTube::with_endpoint(
        "TEST_ACCESS".into(),
        "Название".into(),
        Settings::default(),
        "https://example.com"
    )
    .is_err());
}
#[tokio::test]
async fn oauth_code_pkce_refresh_and_verified_channel() {
    let (base, r, t) = server().await;
    let client = Google::with_endpoint(&base).unwrap();
    let tokens = client
        .exchange(
            "desktop.apps.googleusercontent.com",
            "TEST_SECRET",
            "code",
            "http://127.0.0.1:1234/callback",
            "verifier",
        )
        .await
        .unwrap();
    assert_eq!(tokens.refresh.as_deref(), Some("TEST_REFRESH"));
    assert!(r.lock().unwrap().form.contains("code_verifier=verifier"));
    assert_eq!(client.channel(&tokens.access).await.unwrap().0, "UC_test");
    let tokens = client
        .refresh(
            "desktop.apps.googleusercontent.com",
            "TEST_SECRET",
            "TEST_REFRESH",
        )
        .await
        .unwrap();
    assert_eq!(tokens.access, "TEST_ACCESS");
    assert!(r.lock().unwrap().form.contains("grant_type=refresh_token"));
    t.abort();
}
