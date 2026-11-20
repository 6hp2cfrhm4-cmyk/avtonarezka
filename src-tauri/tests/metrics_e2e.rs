//! Только loopback: счётчики, пропуски, чужой владелец, scopes/квоты и недоверенный endpoint.
use avtonarezka_lib::analytics::collectors::{Collector, Counts, Error};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Fake {
    mode: u8,
    calls: usize,
}
async fn handle(State(s): State<Arc<Mutex<Fake>>>, h: HeaderMap, uri: axum::http::Uri) -> Response {
    assert_eq!(h["authorization"], "Bearer secret-test");
    let mut s = s.lock().unwrap();
    s.calls += 1;
    if s.mode == 1 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":190,"message":"SECRET MUST NOT BE DISPLAYED"}})),
        )
            .into_response();
    }
    if s.mode == 2 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "120")],
            Json(json!({})),
        )
            .into_response();
    }
    if s.mode == 3 {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":{"errors":[{"reason":"quotaExceeded"}]}})),
        )
            .into_response();
    }
    if s.mode == 6 {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "https://example.com/token-leak")],
            Json(json!({})),
        )
            .into_response();
    }
    let v: Value = if uri.path() == "/youtube/v3/videos" {
        assert!(uri.query().unwrap().contains("statistics"));
        let mut v = json!({"items":[{"id":"vid","snippet":{"channelId":if s.mode==4{"foreign"}else{"channel"}},"statistics":{"viewCount":"0","commentCount":"4"}}]});
        if s.mode == 5 {
            v["items"][0]["statistics"]["viewCount"] = json!("-5");
        }
        if s.mode == 7 {
            v["items"] = json!([]);
        }
        v
    } else if uri.path() == "/v27.0/123" {
        json!({"id":"123","owner":{"id":if s.mode==4{"foreign"}else{"456"}}})
    } else {
        assert_eq!(uri.path(), "/v27.0/123/insights");
        if s.mode == 8 {
            json!({"data":[{"name":"views","total_value":{"value":157}},{"name":"shares","total_value":{"value":0}}]})
        } else {
            json!({"data":[{"name":"views","values":[{"value":157}]},{"name":"shares","values":[{"value":0}]}]})
        }
    };
    Json(v).into_response()
}
#[tokio::test]
async fn readonly_collectors_preserve_missing_zero_owner_auth_and_rate_errors() {
    let state = Arc::new(Mutex::new(Fake::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(get(handle))
        .with_state(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let y = Collector::new("youtube", "secret-test".into(), Some(&base)).unwrap();
    let i = Collector::new("instagram", "secret-test".into(), Some(&base)).unwrap();
    assert_eq!(
        y.youtube("vid", "channel").await.unwrap(),
        Counts {
            views: Some(0),
            comments: Some(4),
            ..Default::default()
        }
    );
    assert_eq!(
        i.instagram("123", "456").await.unwrap(),
        Counts {
            views: Some(157),
            shares: Some(0),
            ..Default::default()
        }
    );
    state.lock().unwrap().mode = 8;
    assert_eq!(i.instagram("123", "456").await.unwrap().views, Some(157));
    for (mode, error) in [
        (1, Error::Auth),
        (2, Error::Rate(120)),
        (3, Error::Rate(3600)),
        (4, Error::Auth),
        (5, Error::Invalid),
        (6, Error::Unavailable),
        (7, Error::Unavailable),
    ] {
        state.lock().unwrap().mode = mode;
        assert_eq!(y.youtube("vid", "channel").await.unwrap_err(), error);
    }
    state.lock().unwrap().mode = 4;
    assert_eq!(i.instagram("123", "456").await.unwrap_err(), Error::Auth);
    let calls = state.lock().unwrap().calls;
    assert!(y.youtube("../../token", "channel").await.is_err());
    assert_eq!(state.lock().unwrap().calls, calls);
    for endpoint in [
        "https://example.com/",
        "https://www.googleapis.com@evil.com/",
        "http://127.0.0.1:1/?token=x",
        "https://www.googleapis.com/foreign/",
    ] {
        assert!(Collector::new("youtube", "secret-test".into(), Some(endpoint)).is_err());
    }
    task.abort();
}

#[derive(Default)]
struct VkFake {
    body: Value,
    status: u16,
    calls: usize,
    redirected: usize,
}
async fn vk_handle(
    State(state): State<Arc<Mutex<VkFake>>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    axum::extract::Form(query): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> Response {
    assert!(
        uri.query().is_none(),
        "the token must not appear in the URL"
    );
    assert_eq!(
        query.get("access_token").map(String::as_str),
        Some("secret-test")
    );
    assert_eq!(query.get("v").map(String::as_str), Some("5.199"));
    assert_eq!(query.get("videos").map(String::as_str), Some("-42_17"));
    assert_eq!(query.get("count").map(String::as_str), Some("1"));
    assert!(!headers.contains_key("authorization"));
    let mut s = state.lock().unwrap();
    s.calls += 1;
    if s.status == 302 {
        return (StatusCode::FOUND, [("location", "/leaked")], "").into_response();
    }
    (
        StatusCode::from_u16(if s.status == 0 { 200 } else { s.status }).unwrap(),
        [("retry-after", "123")],
        Json(s.body.clone()),
    )
        .into_response()
}
async fn leaked(State(state): State<Arc<Mutex<VkFake>>>) -> StatusCode {
    state.lock().unwrap().redirected += 1;
    StatusCode::OK
}

#[tokio::test]
async fn vk_video_get_verifies_signed_owner_nullable_counts_api_errors_and_no_redirect() {
    let state = Arc::new(Mutex::new(VkFake::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/method/video.get", post(vk_handle))
        .route("/leaked", get(leaked))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let vk = Collector::new("vk", "secret-test".into(), Some(&base)).unwrap();
    let valid = json!({"response":{"count":1,"items":[{"owner_id":-42,"id":17,"views":0,"likes":{"count":4},"reposts":{"count":0}}]}});
    state.lock().unwrap().body = valid.clone();
    assert_eq!(
        vk.vk("-42_17", "-42").await.unwrap(),
        Counts {
            views: Some(0),
            likes: Some(4),
            shares: Some(0),
            comments: None
        }
    );
    for (field, replacement) in [("owner_id", json!(42)), ("id", json!(18))] {
        let mut body = valid.clone();
        body["response"]["items"][0][field] = replacement;
        state.lock().unwrap().body = body;
        assert_eq!(vk.vk("-42_17", "-42").await.unwrap_err(), Error::Auth);
    }
    for (body, error) in [
        (json!({"response":{"items":[]}}), Error::Unavailable),
        (
            json!({"response":{"items":[{"owner_id":-42,"id":17}]}}),
            Error::Unavailable,
        ),
        (
            json!({"response":{"items":[{"owner_id":-42,"id":17,"views":-1}]}}),
            Error::Invalid,
        ),
        (
            json!({"response":{"items":[{"owner_id":-42,"id":17,"views":"many"}]}}),
            Error::Invalid,
        ),
    ] {
        state.lock().unwrap().body = body;
        assert_eq!(vk.vk("-42_17", "-42").await.unwrap_err(), error);
    }
    // Error bodies may contain request_params with a token. Expose only a category.
    for code in [5, 7, 14, 15, 27, 28, 203, 6, 9, 29, 100] {
        state.lock().unwrap().body = json!({"error":{"error_code":code,"error_msg":"secret-test","request_params":[{"key":"access_token","value":"secret-test"}]}});
        let error = vk.vk("-42_17", "-42").await.unwrap_err();
        assert_eq!(
            error,
            match code {
                6 | 9 | 29 => Error::Rate(123),
                100 => Error::Unavailable,
                _ => Error::Auth,
            }
        );
        assert!(!error.to_string().contains("secret-test"));
    }
    state.lock().unwrap().status = 302;
    assert_eq!(
        vk.vk("-42_17", "-42").await.unwrap_err(),
        Error::Unavailable
    );
    assert_eq!(state.lock().unwrap().redirected, 0);
    state.lock().unwrap().status = 429;
    assert_eq!(vk.vk("-42_17", "-42").await.unwrap_err(), Error::Rate(123));
    let calls = state.lock().unwrap().calls;
    for (video, owner) in [
        ("42_17", "-42"),
        ("-42_17_access_key", "-42"),
        ("-42_-1", "-42"),
        ("-42_017", "-42"),
        ("-42_17", "-042"),
        ("0_17", "0"),
        ("../../token", "-42"),
    ] {
        assert_eq!(vk.vk(video, owner).await.unwrap_err(), Error::Invalid);
    }
    assert_eq!(state.lock().unwrap().calls, calls);
    let youtube = Collector::new("youtube", "secret-test".into(), Some(&base)).unwrap();
    assert_eq!(
        youtube.vk("-42_17", "-42").await.unwrap_err(),
        Error::Invalid
    );
    assert_eq!(state.lock().unwrap().calls, calls);
    for endpoint in [
        "https://api.vk.com@evil.example/",
        "https://api.vk.com/method/",
        "https://vk.com/",
        "http://api.vk.com/",
    ] {
        assert!(Collector::new("vk", "secret-test".into(), Some(endpoint)).is_err());
    }
    server.abort();
}
