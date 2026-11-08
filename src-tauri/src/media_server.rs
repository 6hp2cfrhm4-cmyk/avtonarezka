//! Локальный HTTP-сервер для видео: WebKitGTK не умеет читать произвольные файлы,
//! а `<video>` нужен Range. Отдаём только то, что зарегистрировано через `register`,
//! и только с токеном — чужие процессы и сайты файлы не прочитают.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rand::Rng;
use tower::ServiceExt;
use tower_http::{cors::CorsLayer, services::ServeFile};

#[derive(Clone)]
struct Shared {
    token: Arc<str>,
    registry: Arc<RwLock<HashMap<String, PathBuf>>>,
    #[cfg(debug_assertions)]
    app: tauri::AppHandle,
    #[cfg(debug_assertions)]
    snapshots_dir: PathBuf,
}

#[derive(Clone)]
pub struct MediaServer {
    port: u16,
    shared: Shared,
}

impl MediaServer {
    pub async fn start(app: tauri::AppHandle, data_dir: &std::path::Path) -> anyhow::Result<Self> {
        let token: [u8; 16] = rand::rng().random();
        let shared = Shared {
            token: hex::encode(token).into(),
            registry: Arc::default(),
            #[cfg(debug_assertions)]
            app,
            #[cfg(debug_assertions)]
            snapshots_dir: data_dir.join("snapshots"),
        };
        #[cfg(not(debug_assertions))]
        let _ = (app, data_dir);

        let router = Router::new().route("/m/{id}", get(serve));
        #[cfg(debug_assertions)]
        let router = router
            .route("/dev/snapshot", get(dev_snapshot))
            .route("/dev/go", get(dev_go))
            .route("/dev/import", get(dev_import))
            .route("/dev/select", get(dev_select))
            .route("/dev/analyze", get(dev_analyze))
            .route("/dev/asset", get(dev_asset))
            .route("/dev/accept_all", get(dev_accept_all))
            .route("/dev/generate", get(dev_generate))
            .route("/dev/edit", get(dev_edit))
            .route("/dev/rerender", get(dev_rerender))
            .route("/dev/finals", get(dev_finals))
            .route("/dev/tiktok_settings", get(dev_tt_settings))
            .route("/dev/tiktok_status", get(dev_tt_status))
            .route("/dev/tiktok_login", get(dev_tt_login))
            .route("/dev/tiktok_publish", get(dev_tt_publish))
            .route("/dev/tiktok_reset", get(dev_tt_reset))
            .route("/dev/queue_state", get(dev_queue_state))
            .route("/dev/queue_prepare", get(dev_queue_prepare))
            .route("/dev/queue_start", get(dev_queue_start))
            .route("/dev/queue_stop", get(dev_queue_stop))
            .route("/dev/queue_apply", get(dev_queue_apply))
            .route("/dev/queue_settings", get(dev_queue_settings))
            .route("/dev/yt_analyze", get(dev_yt_analyze))
            .route("/dev/yt_download", get(dev_yt_download))
            .route("/dev/yt_cancel", get(dev_yt_cancel))
            .route("/dev/yt_tab", get(dev_yt_tab));
        let app = router.layer(CorsLayer::permissive()).with_state(shared.clone());

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let port = listener.local_addr()?.port();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("медиа-сервер остановился: {e}");
            }
        });
        tracing::info!("медиа-сервер на 127.0.0.1:{port}");
        Ok(Self { port, shared })
    }

    /// Регистрирует файл и возвращает URL, который можно отдать в `<video src>`.
    /// Один и тот же путь всегда получает один и тот же id.
    pub fn register(&self, path: PathBuf) -> String {
        use sha2::{Digest, Sha256};
        let id = hex::encode(&Sha256::digest(path.to_string_lossy().as_bytes())[..8]);
        self.shared.registry.write().unwrap().insert(id.clone(), path);
        format!("http://127.0.0.1:{}/m/{}?t={}", self.port, id, self.shared.token)
    }
}

async fn serve(
    State(st): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    req: Request<Body>,
) -> Response {
    if q.get("t").map(String::as_str) != Some(&*st.token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = st.registry.read().unwrap().get(&id).cloned();
    let Some(path) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match ServeFile::new(path).oneshot(req).await {
        Ok(resp) => resp.into_response(),
        Err(e) => match e {},
    }
}

#[cfg(debug_assertions)]
async fn dev_snapshot(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let name = q.get("name").map(String::as_str).unwrap_or("snap.png");
    // только простое имя файла, писать можно лишь в каталог снимков
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    if let Err(e) = std::fs::create_dir_all(&st.snapshots_dir) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let path = st.snapshots_dir.join(name);
    match crate::dev_tools::snapshot(&st.app, path.clone()).await {
        Ok(()) => path.display().to_string().into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[cfg(debug_assertions)]
async fn dev_go(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    match crate::dev_tools::go(&st.app, q.get("screen").map(String::as_str).unwrap_or("")) {
        Ok(()) => "ok".into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(debug_assertions)]
fn reply(r: Result<String, String>) -> Response {
    match r {
        Ok(s) => s.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(debug_assertions)]
fn project_id(q: &HashMap<String, String>) -> Result<i64, String> {
    q.get("project").and_then(|v| v.parse().ok()).ok_or_else(|| "нужен ?project=<id>".to_string())
}

#[cfg(debug_assertions)]
async fn dev_import(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::import(&st.app, q.get("path").cloned().unwrap_or_default()).await)
}

#[cfg(debug_assertions)]
async fn dev_select(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::select(&st.app, id)).map(|_| "ok".into()))
}

#[cfg(debug_assertions)]
async fn dev_analyze(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::analyze(&st.app, id)).map(|_| "ok".into()))
}

#[cfg(debug_assertions)]
async fn dev_asset(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::asset(&st.app, q.get("kind").cloned().unwrap_or_default(), q.get("path").cloned().unwrap_or_default()).await)
}

#[cfg(debug_assertions)]
async fn dev_accept_all(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::accept_all(&st.app, id)))
}

#[cfg(debug_assertions)]
async fn dev_generate(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::generate(&st.app, id)))
}

#[cfg(debug_assertions)]
async fn dev_edit(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(q.get("draft").and_then(|v| v.parse().ok()).ok_or_else(|| "нужен ?draft=<id>".to_string()).and_then(|id| crate::dev_tools::edit(&st.app, id)).map(|_| "ok".into()))
}

#[cfg(debug_assertions)]
async fn dev_rerender(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    match q.get("draft").and_then(|v| v.parse().ok()) {
        Some(id) => {
            crate::dev_tools::rerender(&st.app, id);
            "ok".into_response()
        }
        None => (StatusCode::BAD_REQUEST, "нужен ?draft=<id>").into_response(),
    }
}

#[cfg(debug_assertions)]
async fn dev_finals(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::finals(&st.app, id)))
}

#[cfg(debug_assertions)]
async fn dev_queue_state(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_state(&st.app, id)))
}

#[cfg(debug_assertions)]
async fn dev_queue_prepare(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    match project_id(&q) {
        Ok(id) => reply(crate::dev_tools::queue_prepare(&st.app, id).await),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(debug_assertions)]
async fn dev_queue_start(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_start(&st.app, id)))
}

#[cfg(debug_assertions)]
async fn dev_queue_stop(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_stop(&st.app, id)))
}

#[cfg(debug_assertions)]
async fn dev_queue_apply(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_apply_text(&st.app, id, &q)))
}

#[cfg(debug_assertions)]
async fn dev_queue_settings(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::queue_settings(&st.app, &q))
}

#[cfg(debug_assertions)]
async fn dev_tt_settings(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::tiktok_settings(&st.app, &q).await)
}

#[cfg(debug_assertions)]
async fn dev_tt_status(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::tiktok_status(&st.app).await)
}

#[cfg(debug_assertions)]
async fn dev_tt_login(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::tiktok_login(&st.app).await)
}

#[cfg(debug_assertions)]
async fn dev_tt_publish(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::tiktok_publish(&st.app, &q).await)
}

#[cfg(debug_assertions)]
async fn dev_tt_reset(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::tiktok_reset(&st.app).await)
}

#[cfg(debug_assertions)]
async fn dev_yt_analyze(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::yt_analyze(&st.app, q.get("url").cloned().unwrap_or_default()).await)
}

#[cfg(debug_assertions)]
async fn dev_yt_download(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::yt_download(&st.app, &q).await)
}

#[cfg(debug_assertions)]
async fn dev_yt_cancel(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::yt_cancel(&st.app, q.get("job").cloned().unwrap_or_default()))
}

#[cfg(debug_assertions)]
async fn dev_yt_tab(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(crate::dev_tools::yt_tab(&st.app, q.get("url").cloned().unwrap_or_default()).map(|_| "ok".into()))
}
