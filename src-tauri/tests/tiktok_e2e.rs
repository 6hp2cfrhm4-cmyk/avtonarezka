//! Весь путь отправки в TikTok против локального макета их сервера: вход, обновление токена,
//! запрос на загрузку, куски файла, опрос статуса. Настоящий TikTok не затрагивается.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use avtonarezka_lib::tiktok::{
    api::{self, ApiError, Endpoints, Mode, TikTok, Tokens},
    publish::{self, PostRequest, Progress, Timing},
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Form, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

#[derive(Default)]
struct Rec {
    token_forms: Vec<HashMap<String, String>>,
    inits: Vec<(String, Value, String)>,
    creator_calls: usize,
    put_attempts: usize,
    puts: Vec<(String, String, usize)>,
    body: Vec<u8>,
    status_calls: usize,
    // настройки поведения
    put_fail_first: usize,
    put_always: Option<u16>,
    init_error: Option<(&'static str, u16)>,
    statuses: Vec<Value>,
    privacy_options: Vec<String>,
}

type Shared = Arc<Mutex<Rec>>;

fn ok(data: Value) -> Json<Value> {
    Json(json!({ "data": data, "error": { "code": "ok", "message": "", "log_id": "log" } }))
}

async fn token(State(r): State<Shared>, Form(f): Form<HashMap<String, String>>) -> impl IntoResponse {
    let mut rec = r.lock().unwrap();
    let grant = f.get("grant_type").cloned().unwrap_or_default();
    rec.token_forms.push(f.clone());
    if grant == "authorization_code" && f.get("code").map(String::as_str) != Some("good-code") {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant", "error_description": "Authorization code is expired", "log_id": "L" })));
    }
    let n = rec.token_forms.len();
    (StatusCode::OK, Json(json!({ "access_token": format!("acc-{n}"), "refresh_token": format!("ref-{n}"), "expires_in": 7200, "refresh_expires_in": 86400, "open_id": "open-1", "scope": "video.upload", "token_type": "Bearer" })))
}

async fn creator(State(r): State<Shared>) -> Json<Value> {
    let mut rec = r.lock().unwrap();
    rec.creator_calls += 1;
    ok(json!({ "creator_nickname": "Тестер", "privacy_level_options": rec.privacy_options, "max_video_post_duration_sec": 600 }))
}

async fn init(State(r): State<Shared>, uri: axum::http::Uri, headers: HeaderMap, Json(body): Json<Value>) -> axum::response::Response {
    let mut rec = r.lock().unwrap();
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    rec.inits.push((uri.path().to_string(), body, auth));
    if let Some((code, http)) = rec.init_error {
        return (StatusCode::from_u16(http).unwrap(), Json(json!({ "data": {}, "error": { "code": code, "message": "m", "log_id": "L" } }))).into_response();
    }
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
    ok(json!({ "publish_id": "v_pub_file~v2.1", "upload_url": format!("http://{host}/upload/abc?sig=1") })).into_response()
}

async fn upload(State(r): State<Shared>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let mut rec = r.lock().unwrap();
    rec.put_attempts += 1;
    if let Some(code) = rec.put_always {
        return StatusCode::from_u16(code).unwrap();
    }
    if rec.put_fail_first > 0 {
        rec.put_fail_first -= 1;
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    let range = headers.get("content-range").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let ctype = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    // «bytes 0-9/100»: последний кусок — тот, что кончается на последнем байте
    let nums: Vec<u64> = range.trim_start_matches("bytes ").split(['-', '/']).filter_map(|n| n.parse().ok()).collect();
    let last = nums.len() == 3 && nums[1] + 1 == nums[2];
    rec.puts.push((range, ctype, body.len()));
    rec.body.extend_from_slice(&body);
    if last { StatusCode::CREATED } else { StatusCode::PARTIAL_CONTENT }
}

async fn status(State(r): State<Shared>) -> Json<Value> {
    let mut rec = r.lock().unwrap();
    let i = rec.status_calls.min(rec.statuses.len().saturating_sub(1));
    rec.status_calls += 1;
    let s = rec.statuses.get(i).cloned().unwrap_or_else(|| json!({ "status": "PROCESSING_UPLOAD" }));
    ok(s)
}

async fn serve(rec: Rec) -> (Endpoints, Shared) {
    let shared: Shared = Arc::new(Mutex::new(rec));
    let app = Router::new()
        .route("/v2/oauth/token/", post(token))
        .route("/v2/post/publish/creator_info/query/", post(creator))
        .route("/v2/post/publish/video/init/", post(init))
        .route("/v2/post/publish/inbox/video/init/", post(init))
        .route("/upload/abc", axum::routing::put(upload))
        .route("/v2/post/publish/status/fetch/", post(status))
        // у axum по умолчанию лимит тела 2 МБ; настоящий сервер принимает куски по десятки мегабайт
        .layer(DefaultBodyLimit::disable())
        .with_state(shared.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (Endpoints { auth: base.clone(), api: base }, shared)
}

fn rec_with(statuses: Vec<Value>) -> Rec {
    Rec { statuses, privacy_options: vec!["SELF_ONLY".into(), "MUTUAL_FOLLOW_FRIENDS".into()], ..Default::default() }
}

fn video(size: usize) -> (PathBuf, Vec<u8>) {
    // тесты идут параллельно: у каждого вызова свой файл, иначе один перезапишет файл, который читает другой
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!("avto-tt-{}-{size}-{}.mp4", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    // не повторяющийся с периодом куска узор: перестановка кусков была бы заметна
    let data: Vec<u8> = (0..size).map(|i| ((i * 31 + i / 7919) % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();
    (path, data)
}

fn req(path: PathBuf, mode: Mode, privacy: &str) -> PostRequest {
    PostRequest { video: path, duration_sec: 40.0, mode, title: "Смотрите #юмор".into(), privacy: privacy.into(), allow_comment: true, allow_duet: false, allow_stitch: false }
}

fn fast() -> Timing {
    Timing { poll_every: Duration::from_millis(10), poll_timeout: Duration::from_secs(5), retry_delay: Duration::from_millis(5) }
}

async fn run(api: &TikTok, req: &PostRequest, timing: &Timing) -> (anyhow::Result<publish::Outcome>, Vec<Progress>) {
    let mut log = Vec::new();
    let r = publish::publish(api, "tok", req, timing, &mut |p| log.push(p)).await;
    (r, log)
}

#[tokio::test(flavor = "multi_thread")]
async fn inbox_upload_sends_every_chunk_in_order_and_the_file_arrives_intact() {
    let (ep, rec) = serve(rec_with(vec![json!({"status": "PROCESSING_UPLOAD"}), json!({"status": "PROCESSING_UPLOAD"}), json!({"status": "SEND_TO_USER_INBOX"})])).await;
    let (path, original) = video(55_000_123);
    let (out, log) = run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await;
    let out = out.unwrap();

    assert_eq!((out.status.as_str(), out.publish_id.as_str()), ("SEND_TO_USER_INBOX", "v_pub_file~v2.1"));
    let r = rec.lock().unwrap();

    // запрос на загрузку: адрес черновиков, токен, числа из документации, подписи нет
    let (path, body, auth) = &r.inits[0];
    assert_eq!((path.as_str(), auth.as_str()), ("/v2/post/publish/inbox/video/init/", "Bearer tok"));
    assert_eq!(body["source_info"], json!({"source": "FILE_UPLOAD", "video_size": 55_000_123u64, "chunk_size": 10_000_000u64, "total_chunk_count": 5u64}));
    assert!(body.get("post_info").is_none(), "в черновики подпись не отправляется");
    assert_eq!(r.creator_calls, 0, "для черновиков информация об аккаунте не нужна");

    // куски: 5 штук, подряд, нужные заголовки, последний вобрал остаток
    let ranges: Vec<&str> = r.puts.iter().map(|(range, _, _)| range.as_str()).collect();
    assert_eq!(ranges, ["bytes 0-9999999/55000123", "bytes 10000000-19999999/55000123", "bytes 20000000-29999999/55000123", "bytes 30000000-39999999/55000123", "bytes 40000000-55000122/55000123"]);
    assert!(r.puts.iter().all(|(_, ct, _)| ct == "video/mp4"));
    assert_eq!(r.puts.last().unwrap().2, 15_000_123);
    assert!(r.body == original, "файл на сервере не совпал с исходным");

    // прогресс идёт вверх и заканчивается единицей
    assert!(log.windows(2).all(|w| w[1].fraction >= w[0].fraction - 1e-9));
    assert_eq!(log.last().unwrap().fraction, 1.0);
    assert!(log.iter().any(|p| p.message.contains("Загружаю")));
    assert_eq!(r.status_calls, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn small_file_goes_as_one_chunk() {
    let (ep, rec) = serve(rec_with(vec![json!({"status": "SEND_TO_USER_INBOX"})])).await;
    let (path, original) = video(3_000_000);
    run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap();
    let r = rec.lock().unwrap();
    assert_eq!(r.puts.len(), 1);
    assert_eq!(r.puts[0].0, "bytes 0-2999999/3000000");
    assert!(r.body == original);
    assert_eq!(r.inits[0].1["source_info"]["total_chunk_count"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_post_checks_the_account_first_and_sends_post_info() {
    let (ep, rec) = serve(rec_with(vec![json!({"status": "PUBLISH_COMPLETE", "publicaly_available_post_id": [111]})])).await;
    let (path, _) = video(2_000_000);
    let api = TikTok::new(ep).unwrap();
    let out = run(&api, &req(path.clone(), Mode::Direct, "SELF_ONLY"), &fast()).await.0.unwrap();
    assert_eq!((out.status.as_str(), out.post_ids.clone()), ("PUBLISH_COMPLETE", vec![111]));
    {
        let r = rec.lock().unwrap();
        assert_eq!(r.creator_calls, 1);
        let (p, body, _) = &r.inits[0];
        assert_eq!(p, "/v2/post/publish/video/init/");
        assert_eq!(body["post_info"]["title"], "Смотрите #юмор");
        assert_eq!(body["post_info"]["privacy_level"], "SELF_ONLY");
        // разрешили только комментарии: остальное выключено
        assert_eq!((body["post_info"]["disable_comment"].as_bool(), body["post_info"]["disable_duet"].as_bool(), body["post_info"]["disable_stitch"].as_bool()), (Some(false), Some(true), Some(true)));
    }

    // видимость, которой нет у аккаунта, отсекается до загрузки
    let (before_inits, before_puts) = {
        let r = rec.lock().unwrap();
        (r.inits.len(), r.put_attempts)
    };
    let e = run(&api, &req(path, Mode::Direct, "PUBLIC_TO_EVERYONE"), &fast()).await.0.unwrap_err().to_string();
    assert!(e.contains("недоступна") && e.contains("SELF_ONLY"), "{e}");
    let r = rec.lock().unwrap();
    assert_eq!((r.inits.len(), r.put_attempts), (before_inits, before_puts), "после отказа ничего не отправлялось");
}

#[tokio::test(flavor = "multi_thread")]
async fn server_errors_on_a_chunk_are_retried_then_succeed() {
    let mut base = rec_with(vec![json!({"status": "SEND_TO_USER_INBOX"})]);
    base.put_fail_first = 2;
    let (ep, rec) = serve(base).await;
    let (path, original) = video(2_000_000);
    run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap();
    let r = rec.lock().unwrap();
    assert_eq!(r.put_attempts, 3, "две неудачи и успех");
    assert!(r.body == original);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chunk_that_keeps_failing_gives_up_after_three_retries() {
    let mut base = rec_with(vec![]);
    base.put_always = Some(503);
    let (ep, rec) = serve(base).await;
    let (path, _) = video(2_000_000);
    let e = run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap_err().to_string();
    assert!(e.contains("503"), "{e}");
    assert_eq!(rec.lock().unwrap().put_attempts, 4, "первая попытка и три повтора");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_errors_on_upload_are_explained_and_not_retried() {
    for (code, needle) in [(416u16, "не в том порядке"), (403, "истекла"), (404, "не нашёл задачу")] {
        let mut base = rec_with(vec![]);
        base.put_always = Some(code);
        let (ep, rec) = serve(base).await;
        let (path, _) = video(2_000_000);
        let e = run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap_err().to_string();
        assert!(e.contains(needle), "{code}: {e}");
        assert_eq!(rec.lock().unwrap().put_attempts, 1, "{code}: повторять нечего");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn init_errors_become_readable_messages_and_nothing_is_uploaded() {
    let mut base = rec_with(vec![]);
    base.init_error = Some(("spam_risk_too_many_pending_share", 403));
    let (ep, rec) = serve(base).await;
    let (path, _) = video(2_000_000);
    let err = run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap_err();
    let api_err = err.downcast_ref::<ApiError>().expect("ошибка TikTok должна быть типизирована");
    assert_eq!((api_err.code.as_str(), api_err.http, api_err.log_id.as_deref()), ("spam_risk_too_many_pending_share", 403, Some("L")));
    assert!(err.to_string().contains("5 неопубликованных"));
    assert_eq!(rec.lock().unwrap().put_attempts, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_video_reports_the_reason_from_tiktok() {
    let (ep, _) = serve(rec_with(vec![json!({"status": "PROCESSING_UPLOAD"}), json!({"status": "FAILED", "fail_reason": "file_format_check_failed"})])).await;
    let (path, _) = video(2_000_000);
    let e = run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap_err().to_string();
    assert!(e.contains("формат"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn endless_processing_times_out_with_a_helpful_message() {
    let (ep, _) = serve(rec_with(vec![json!({"status": "PROCESSING_UPLOAD"})])).await;
    let (path, _) = video(2_000_000);
    let timing = Timing { poll_timeout: Duration::from_millis(250), ..fast() };
    let e = run(&TikTok::new(ep).unwrap(), &req(path, Mode::Inbox, "SELF_ONLY"), &timing).await.0.unwrap_err().to_string();
    assert!(e.contains("слишком долго"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_and_empty_files_are_refused_before_any_request() {
    let (ep, rec) = serve(rec_with(vec![])).await;
    let api = TikTok::new(ep).unwrap();
    let e = run(&api, &req(PathBuf::from("/нет/такого.mp4"), Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap_err().to_string();
    assert!(e.contains("не найден файл"), "{e}");
    let empty = std::env::temp_dir().join(format!("avto-tt-empty-{}.mp4", std::process::id()));
    std::fs::write(&empty, b"").unwrap();
    assert!(run(&api, &req(empty, Mode::Inbox, "SELF_ONLY"), &fast()).await.0.unwrap_err().to_string().contains("пустой"));
    assert!(rec.lock().unwrap().inits.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn code_exchange_sends_the_pkce_verifier_and_reports_a_bad_code() {
    let (ep, rec) = serve(rec_with(vec![])).await;
    let api = TikTok::new(ep).unwrap();
    let t = api.exchange_code("key", "secret", "good-code", "http://127.0.0.1:38475/callback/", "verifier-123", 1000).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_str(), t.expires_at, t.open_id.as_str()), ("acc-1", "ref-1", 8200, "open-1"));
    {
        let f = &rec.lock().unwrap().token_forms[0];
        for (k, v) in [("client_key", "key"), ("client_secret", "secret"), ("code", "good-code"), ("grant_type", "authorization_code"), ("redirect_uri", "http://127.0.0.1:38475/callback/"), ("code_verifier", "verifier-123")] {
            assert_eq!(f.get(k).map(String::as_str), Some(v), "поле {k}");
        }
    }
    let e = api.exchange_code("key", "secret", "stale", "http://127.0.0.1:38475/callback/", "v", 0).await.unwrap_err().to_string();
    assert!(e.contains("Authorization code is expired"), "{e}");
}

fn tokens(expires_at: i64, refresh_expires_at: i64) -> Tokens {
    Tokens { access_token: "old-acc".into(), refresh_token: "old-ref".into(), expires_at, refresh_expires_at, open_id: "o".into(), scope: "video.upload".into() }
}

#[tokio::test(flavor = "multi_thread")]
async fn tokens_are_refreshed_early_and_the_rotated_one_is_saved() {
    let (ep, rec) = serve(rec_with(vec![])).await;
    let api = TikTok::new(ep).unwrap();
    let saved: Arc<Mutex<Vec<Tokens>>> = Arc::default();
    let keep = |saved: &Arc<Mutex<Vec<Tokens>>>| {
        let saved = saved.clone();
        move |t: Tokens| -> api::SaveFuture {
            let saved = saved.clone();
            Box::pin(async move {
                saved.lock().unwrap().push(t);
                Ok(())
            })
        }
    };

    // ещё свежий — запросов нет
    let t = api::ensure_fresh(&api, "k", "s", tokens(10_000, 99_999), 1000, &mut keep(&saved)).await.unwrap();
    assert_eq!(t.access_token, "old-acc");
    assert!(rec.lock().unwrap().token_forms.is_empty() && saved.lock().unwrap().is_empty());

    // меньше 5 минут до конца — обновляем и сохраняем новый refresh-токен
    let t = api::ensure_fresh(&api, "k", "s", tokens(1200, 99_999), 1000, &mut keep(&saved)).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_str()), ("acc-1", "ref-1"));
    assert_eq!(*saved.lock().unwrap(), vec![t]);
    let f = &rec.lock().unwrap().token_forms[0];
    assert_eq!((f["grant_type"].as_str(), f["refresh_token"].as_str()), ("refresh_token", "old-ref"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_refresh_token_asks_to_log_in_again_and_a_failed_save_is_not_hidden() {
    let (ep, rec) = serve(rec_with(vec![])).await;
    let api = TikTok::new(ep).unwrap();
    let e = api::ensure_fresh(&api, "k", "s", tokens(500, 900), 1000, &mut |_| Box::pin(async { Ok(()) })).await.unwrap_err().to_string();
    assert!(e.contains("Подключи аккаунт заново"), "{e}");
    assert!(rec.lock().unwrap().token_forms.is_empty(), "с истёкшим токеном запрос не нужен");

    let e = api::ensure_fresh(&api, "k", "s", tokens(1200, 99_999), 1000, &mut |_| Box::pin(async { anyhow::bail!("связка заблокирована") })).await.unwrap_err().to_string();
    assert!(e.contains("не удалось сохранить"), "{e}");
}
