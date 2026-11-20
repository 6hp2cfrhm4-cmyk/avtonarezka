//! Локальный HTTP-сервер для видео: WebKitGTK не умеет читать произвольные файлы,
//! а `<video>` нужен Range. Отдаём только то, что зарегистрировано через `register`,
//! и только с токеном — чужие процессы и сайты файлы не прочитают.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};

#[cfg(all(debug_assertions, feature = "desktop"))]
use crate::host::Manager;
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
    #[cfg(all(debug_assertions, feature = "desktop"))]
    app: crate::host::AppHandle,
    #[cfg(all(debug_assertions, feature = "desktop"))]
    snapshots_dir: PathBuf,
}

#[derive(Clone)]
pub struct MediaServer {
    port: u16,
    shared: Shared,
}

impl MediaServer {
    pub async fn start(
        app: crate::host::AppHandle,
        data_dir: &std::path::Path,
    ) -> anyhow::Result<Self> {
        let token: [u8; 16] = rand::rng().random();
        let shared = Shared {
            token: hex::encode(token).into(),
            registry: Arc::default(),
            #[cfg(all(debug_assertions, feature = "desktop"))]
            app,
            #[cfg(all(debug_assertions, feature = "desktop"))]
            snapshots_dir: data_dir.join("snapshots"),
        };
        #[cfg(not(all(debug_assertions, feature = "desktop")))]
        let _ = (app, data_dir);

        let router = Router::new().route("/m/{id}", get(serve));
        #[cfg(all(debug_assertions, feature = "desktop"))]
        let router = router
            .route("/dev/snapshot", get(dev_snapshot))
            .route("/dev/factory", get(dev_factory_get).post(dev_factory_post))
            .route(
                "/dev/analysis_options",
                get(dev_analysis_options_get).post(dev_analysis_options_set),
            )
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
            .route("/dev/yt_tab", get(dev_yt_tab))
            .route("/dev/batch_add", get(dev_batch_add))
            .route("/dev/batch_start", get(dev_batch_start))
            .route("/dev/batch_approve", get(dev_batch_approve))
            .route("/dev/batch_list", get(dev_batch_list));
        // Безголовая сборка (демон на сервере) ничего не слушает (правило G2): порт не открываем.
        #[cfg(not(feature = "desktop"))]
        {
            let _ = router;
            return Ok(Self { port: 0, shared });
        }
        #[cfg(feature = "desktop")]
        {
            let app = router
                .layer(CorsLayer::permissive())
                .with_state(shared.clone());
            let listener =
                tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
            let port = listener.local_addr()?.port();
            crate::host::spawn(async move {
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!("медиа-сервер остановился: {e}");
                }
            });
            tracing::info!("медиа-сервер на 127.0.0.1:{port}");
            Ok(Self { port, shared })
        }
    }

    /// Регистрирует файл и возвращает URL, который можно отдать в `<video src>`.
    /// Один и тот же путь всегда получает один и тот же id.
    pub fn register(&self, path: PathBuf) -> String {
        if self.port == 0 {
            return String::new();
        }
        use sha2::{Digest, Sha256};
        let id = hex::encode(&Sha256::digest(path.to_string_lossy().as_bytes())[..8]);
        self.shared
            .registry
            .write()
            .unwrap()
            .insert(id.clone(), path);
        format!(
            "http://127.0.0.1:{}/m/{}?t={}",
            self.port, id, self.shared.token
        )
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

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_snapshot(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let name = q.get("name").map(String::as_str).unwrap_or("snap.png");
    // только простое имя файла, писать можно лишь в каталог снимков
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    if let Err(e) = std::fs::create_dir_all(&st.snapshots_dir) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let path = st.snapshots_dir.join(name);
    let label = q.get("window").map(String::as_str).unwrap_or("main");
    if label != "main" && !label.starts_with("campaign-report-") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let result = if label == "main" {
        crate::dev_tools::snapshot(&st.app, path.clone()).await
    } else {
        crate::dev_tools::snapshot_window(&st.app, path.clone(), label).await
    };
    match result {
        Ok(()) => path.display().to_string().into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_go(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    match crate::dev_tools::go(&st.app, q.get("screen").map(String::as_str).unwrap_or("")) {
        Ok(()) => "ok".into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(all(debug_assertions, feature = "desktop"))]
fn reply(r: Result<String, String>) -> Response {
    match r {
        Ok(s) => s.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(all(debug_assertions, feature = "desktop"))]
fn project_id(q: &HashMap<String, String>) -> Result<i64, String> {
    q.get("project")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "нужен ?project=<id>".to_string())
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_import(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::import(&st.app, q.get("path").cloned().unwrap_or_default()).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_select(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(
        project_id(&q)
            .and_then(|id| crate::dev_tools::select(&st.app, id))
            .map(|_| "ok".into()),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_analyze(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(
        project_id(&q)
            .and_then(|id| crate::dev_tools::analyze(&st.app, id))
            .map(|_| "ok".into()),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_analysis_options_get(State(st): State<Shared>) -> Response {
    reply(
        serde_json::to_string(&crate::analysis_cmds::analysis_options_get(st.app.state()))
            .map_err(|e| e.to_string()),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_analysis_options_set(
    State(st): State<Shared>,
    axum::Json(options): axum::Json<crate::analysis_cmds::UiOptions>,
) -> Response {
    reply(
        crate::analysis_cmds::analysis_options_set(st.app.state(), options)
            .and_then(|o| serde_json::to_string(&o).map_err(|e| e.to_string())),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_asset(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(
        crate::dev_tools::asset(
            &st.app,
            q.get("kind").cloned().unwrap_or_default(),
            q.get("path").cloned().unwrap_or_default(),
        )
        .await,
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_accept_all(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::accept_all(&st.app, id)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_generate(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::generate(&st.app, id)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_edit(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(
        q.get("draft")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| "нужен ?draft=<id>".to_string())
            .and_then(|id| crate::dev_tools::edit(&st.app, id))
            .map(|_| "ok".into()),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_rerender(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match q.get("draft").and_then(|v| v.parse().ok()) {
        Some(id) => {
            crate::dev_tools::rerender(&st.app, id);
            "ok".into_response()
        }
        None => (StatusCode::BAD_REQUEST, "нужен ?draft=<id>").into_response(),
    }
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_finals(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::finals(&st.app, id)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_queue_state(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_state(&st.app, id)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_queue_prepare(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match project_id(&q) {
        Ok(id) => reply(crate::dev_tools::queue_prepare(&st.app, id).await),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_queue_start(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_start(&st.app, id)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_queue_stop(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_stop(&st.app, id)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_queue_apply(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(project_id(&q).and_then(|id| crate::dev_tools::queue_apply_text(&st.app, id, &q)))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_queue_settings(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::queue_settings(&st.app, &q))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_tt_settings(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::tiktok_settings(&st.app, &q).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_tt_status(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::tiktok_status(&st.app).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_tt_login(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::tiktok_login(&st.app).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_tt_publish(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::tiktok_publish(&st.app, &q).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_tt_reset(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::tiktok_reset(&st.app).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_yt_analyze(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::yt_analyze(&st.app, q.get("url").cloned().unwrap_or_default()).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_yt_download(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::yt_download(&st.app, &q).await)
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_yt_cancel(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(crate::dev_tools::yt_cancel(
        &st.app,
        q.get("job").cloned().unwrap_or_default(),
    ))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_yt_tab(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(
        crate::dev_tools::yt_tab(&st.app, q.get("url").cloned().unwrap_or_default())
            .map(|_| "ok".into()),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_batch_add(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let precut = q.get("precut").is_some_and(|v| v == "1");
    reply(
        q.get("path")
            .cloned()
            .ok_or_else(|| "нужен ?path=".to_string())
            .and_then(|p| crate::dev_tools::batch_add(&st.app, p, precut)),
    )
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_batch_start(State(st): State<Shared>) -> Response {
    crate::dev_tools::batch_start(&st.app);
    "ok".into_response()
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_batch_approve(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::batch_approve(&st.app))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_batch_list(State(st): State<Shared>) -> Response {
    reply(crate::dev_tools::batch_list(&st.app))
}

#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_factory_get(State(st): State<Shared>) -> Response {
    reply(
        crate::factory::factory_state(st.app.state())
            .and_then(|s| serde_json::to_string(&s).map_err(|e| e.to_string())),
    )
}
#[cfg(all(debug_assertions, feature = "desktop"))]
async fn dev_factory_post(
    State(st): State<Shared>,
    axum::Json(value): axum::Json<serde_json::Value>,
) -> Response {
    let action = value["action"].as_str().unwrap_or("");
    if action == "analytics_collect" {
        return reply(
            crate::analytics::analytics_collect(st.app.clone())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "vk_metrics_options" {
        return reply(
            crate::analytics::vk::vk_metrics_options(st.app.state())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "vk_metrics_account_save" {
        let args = match serde_json::from_value(value["payload"].clone()) {
            Ok(a) => a,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::analytics::vk::vk_metrics_account_save(st.app.clone(), args)
                .await
                .map(|id| id.to_string()),
        );
    }
    if action == "vk_metrics_link" {
        let args = match serde_json::from_value(value["payload"].clone()) {
            Ok(a) => a,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::analytics::vk::vk_metrics_link(st.app.clone(), args)
                .await
                .map(|id| id.to_string()),
        );
    }
    if action == "publication_autofill" {
        return reply(
            crate::publish::autofill::publication_autofill(st.app.clone())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "report_state" {
        return reply(
            crate::dev_tools::ui_state_window(
                &st.app,
                &format!(
                    "campaign-report-{}",
                    value["payload"]["campaignId"].as_i64().unwrap_or(0)
                ),
            )
            .await,
        );
    }
    if action == "report_print_probe" {
        return reply(
            crate::dev_tools::report_print_probe(
                &st.app,
                format!(
                    "campaign-report-{}",
                    value["payload"]["campaignId"].as_i64().unwrap_or(0)
                ),
            )
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "campaign_report_open" {
        return reply(
            crate::campaigns_report::campaign_report_open(
                st.app.clone(),
                st.app.state(),
                value["payload"]["campaignId"].as_i64().unwrap_or(0),
            )
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "campaign_report_export" {
        let payload = &value["payload"];
        return reply(
            crate::campaigns_report::campaign_report_export(
                st.app.state(),
                payload["campaignId"].as_i64().unwrap_or(0),
                payload["path"].as_str().unwrap_or("").into(),
            )
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "caption_generate" {
        let payload = &value["payload"];
        let result = async {
            let draft = payload["draftId"].as_i64().ok_or("Нужен draftId")?;
            let saved =
                serde_json::from_value(payload["saved"].clone()).map_err(|e| e.to_string())?;
            serde_json::to_string(
                &crate::caption::commands::caption_draft_generate(
                    st.app.clone(),
                    draft,
                    saved,
                    payload["accountId"].as_i64(),
                )
                .await?,
            )
            .map_err(|e| e.to_string())
        }
        .await;
        return reply(result);
    }
    if action == "output_ready_preview" {
        let payload = &value["payload"];
        let result=async {
            let ready=payload["readyId"].as_i64().ok_or("Нужен готовый файл")?;
            let platform=serde_json::from_value(payload["platform"].clone()).map_err(|e|e.to_string())?;
            let path:String=st.app.state::<crate::state::AppState>().db.0.lock().unwrap().query_row("SELECT path FROM ready_videos WHERE id=?1",[ready],|r|r.get(0)).map_err(|e|e.to_string())?;
            let path=crate::publish::output::prepare_ready(&st.app,ready,std::path::Path::new(&path),platform,None).await?;
            Ok(serde_json::json!({"url":st.app.state::<crate::state::AppState>().media.register(path)}).to_string())
        }.await;
        return reply(result);
    }
    if action == "output_preview" {
        let payload = &value["payload"];
        let result = async {
            let draft = payload["draftId"].as_i64().ok_or("Нужен ролик")?;
            let platform =
                serde_json::from_value(payload["platform"].clone()).map_err(|e| e.to_string())?;
            serde_json::to_string(
                &crate::publish::output::output_preview(st.app.clone(), draft, platform).await?,
            )
            .map_err(|e| e.to_string())
        }
        .await;
        return reply(result);
    }
    if action == "ui_state" {
        return reply(crate::dev_tools::ui_state(&st.app).await);
    }
    if action == "lifecycle_status" {
        return reply(
            serde_json::to_string(&crate::operations::lifecycle::lifecycle_status(
                st.app.clone(),
                st.app.state(),
            ))
            .map_err(|e| e.to_string()),
        );
    }
    if action == "lifecycle_tray_set" {
        return reply(
            crate::operations::lifecycle::lifecycle_tray_set(
                st.app.clone(),
                st.app.state(),
                value["payload"]["enabled"].as_bool().unwrap_or(false),
            )
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "lifecycle_autostart_set" {
        return reply(
            crate::operations::lifecycle::lifecycle_autostart_set(
                st.app.clone(),
                st.app.state(),
                value["payload"]["enabled"].as_bool().unwrap_or(false),
            )
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "lifecycle_hide" {
        return reply(
            crate::operations::lifecycle::lifecycle_hide(st.app.clone()).map(|_| "null".into()),
        );
    }
    if action == "disk_snapshot" {
        return reply(
            crate::operations::disk::disk_snapshot(st.app.clone())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "diagnostics_collect" {
        return reply(
            crate::operations::diagnostics::diagnostics_collect(st.app.clone())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "diagnostics_export" {
        return reply(
            crate::operations::diagnostics::diagnostics_export(
                st.app.clone(),
                value["payload"]["path"].as_str().unwrap_or("").into(),
            )
            .await
            .map(|_| "null".into()),
        );
    }
    if action == "disk_cleanup" {
        return reply(
            crate::operations::disk::disk_cleanup(st.app.clone())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "ui_scroll" {
        return reply(
            crate::dev_tools::ui_scroll(
                &st.app,
                value["payload"]["heading"].as_str().unwrap_or("").into(),
            )
            .await,
        );
    }
    if action == "youtube_oauth_settings" {
        return reply(
            crate::publish::youtube::accounts::youtube_oauth_settings(st.app.state())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "youtube_oauth_save" {
        let p = &value["payload"];
        return reply(
            crate::publish::youtube::accounts::youtube_oauth_save(
                st.app.state(),
                p["clientId"].as_str().unwrap_or("").into(),
                p["clientSecret"].as_str().map(str::to_string),
            )
            .await
            .map(|_| "null".into()),
        );
    }
    if action == "youtube_login_start" {
        return reply(
            crate::publish::youtube::accounts::youtube_login_start(st.app.clone(), st.app.state())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "publication_accounts" {
        return reply(
            crate::publish::calendar::publication_accounts(st.app.state())
                .await
                .and_then(|a| serde_json::to_string(&a).map_err(|e| e.to_string())),
        );
    }
    if action == "publication_account_policy" {
        let p = &value["payload"];
        let policy = match serde_json::from_value(p["policy"].clone()) {
            Ok(a) => a,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::publish::calendar::publication_account_policy(
                st.app.clone(),
                st.app.state(),
                p["id"].as_i64().unwrap_or(0),
                policy,
            )
            .map(|_| "null".into()),
        );
    }
    if action == "instagram_account_save" {
        let args = match serde_json::from_value(value["payload"].clone()) {
            Ok(a) => a,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::publish::instagram::accounts::instagram_account_save(
                st.app.clone(),
                st.app.state(),
                args,
            )
            .await
            .map(|id| id.to_string()),
        );
    }
    if action == "telegram_account_save" {
        let args = match serde_json::from_value(value["payload"].clone()) {
            Ok(a) => a,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::publish::calendar::telegram_account_save(st.app.clone(), st.app.state(), args)
                .await
                .map(|id| id.to_string()),
        );
    }
    if action == "telegram_account_check" {
        let args = match serde_json::from_value(value["payload"].clone()) {
            Ok(a) => a,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::publish::calendar::telegram_account_check(st.app.state(), args)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "camera_frame" {
        let Some(id) = value["payload"]["projectId"].as_i64() else {
            return reply(Err("Нужен projectId".into()));
        };
        let Some(at) = value["payload"]["at"].as_f64() else {
            return reply(Err("Нужна секунда кадра".into()));
        };
        return reply(
            crate::camera::camera_frame(st.app.state(), id, at)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "camera_detect" {
        let Some(id) = value["payload"]["projectId"].as_i64() else {
            return reply(Err("Нужен projectId".into()));
        };
        let Some(at) = value["payload"]["at"].as_f64() else {
            return reply(Err("Нужна секунда".into()));
        };
        return reply(
            crate::camera::camera_detect(st.app.state(), id, at)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "camera_scan" {
        let p = &value["payload"];
        let Some(project) = p["projectId"].as_i64() else {
            return reply(Err("Нужна запись".into()));
        };
        let camera = match serde_json::from_value(p["camera"].clone()) {
            Ok(r) => r,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::camera::camera_scan(
                st.app.clone(),
                st.app.state(),
                project,
                p["id"].as_str().unwrap_or("").into(),
                camera,
            )
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "camera_scan_cancel" {
        crate::camera::camera_scan_cancel(
            st.app.state(),
            value["payload"]["id"].as_str().unwrap_or("").into(),
        );
        return reply(Ok("null".into()));
    }
    if action == "layout_preview" {
        let p = &value["payload"];
        let Some(channel) = p["channelId"].as_i64() else {
            return reply(Err("Нужен канал".into()));
        };
        let Some(project) = p["projectId"].as_i64() else {
            return reply(Err("Нужна запись".into()));
        };
        let t = match serde_json::from_value(p["template"].clone()) {
            Ok(t) => t,
            Err(e) => return reply(Err(e.to_string())),
        };
        return reply(
            crate::camera::layout_preview(
                st.app.state(),
                channel,
                project,
                p["at"].as_f64().unwrap_or(0.0),
                t,
            )
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "source_check" {
        let Some(id) = value["payload"].as_i64() else {
            return reply(Err("Нужен id канала".into()));
        };
        return reply(
            crate::factory::monitor::source_check(st.app.clone(), st.app.state(), id)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    if action == "chat_import" {
        let Some(id) = value["payload"]["projectId"].as_i64() else {
            return reply(Err("Нужен projectId".into()));
        };
        let Some(path) = value["payload"]["path"].as_str() else {
            return reply(Err("Нужен путь".into()));
        };
        return reply(
            crate::analysis_inputs::analysis_chat_import(st.app.clone(), id, path.to_string())
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())),
        );
    }
    let result = (|| -> Result<serde_json::Value, String> {
        let payload = value["payload"].clone();
        let parse_err = |e: serde_json::Error| e.to_string();
        match action {
            "telegram_ui" => {
                let script = match payload["step"].as_str().unwrap_or("") {
                    "field" => {
                        let label = payload["label"]
                            .as_str()
                            .filter(|s| {
                                [
                                    "Название канала Telegram",
                                    "Канал Telegram",
                                    "Токен бота Telegram",
                                ]
                                .contains(s)
                            })
                            .ok_or("Неизвестное поле Telegram")?;
                        let value = serde_json::to_string(
                            payload["value"].as_str().ok_or("Нужно значение")?,
                        )
                        .map_err(parse_err)?;
                        format!("{{const e=[...document.querySelectorAll('label')].find(l=>l.firstElementChild?.textContent==='{label}')?.querySelector('input');if(e){{Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,{value});e.dispatchEvent(new Event('input',{{bubbles:true}}));}}}}")
                    }
                    "click" => {
                        let label = payload["label"]
                            .as_str()
                            .filter(|s| {
                                [
                                    "Проверить доступ Telegram",
                                    "Сохранить Telegram",
                                    "Новый канал",
                                ]
                                .contains(s)
                            })
                            .ok_or("Неизвестная кнопка Telegram")?;
                        format!("[...document.querySelectorAll('button')].find(b=>b.textContent==='{label}'&&!b.disabled)?.click()")
                    }
                    "profile" => {
                        let id = payload["id"].as_i64().ok_or("Нужен id")?;
                        format!(
                            "document.querySelector('[data-telegram-account=\"{id}\"]')?.click()"
                        )
                    }
                    _ => return Err("Неизвестный шаг Telegram".into()),
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "vk_metrics_ui" => {
                let p = &payload;
                let script = match p["step"].as_str().unwrap_or("") {
                    "open" => {
                        "document.querySelector('[data-vk-link]')?.setAttribute('open','')".into()
                    }
                    "profile" => format!(
                        "document.querySelector('[data-vk-profile=\"{}\"]')?.click()",
                        p["id"].as_i64().ok_or("Нужен профиль VK")?
                    ),
                    "clip" => {
                        let id = p["id"]
                            .as_str()
                            .filter(|s| {
                                s.len() < 30 && s.bytes().all(|c| c.is_ascii_alphanumeric())
                            })
                            .ok_or("Нужен ID финала")?;
                        format!("document.querySelector('[data-vk-clip=\"{id}\"]')?.click()")
                    }
                    "click" => {
                        let label = p["label"]
                            .as_str()
                            .filter(|s| {
                                [
                                    "Новый профиль VK",
                                    "Проверить и сохранить VK",
                                    "Проверить и связать ролик",
                                ]
                                .contains(s)
                            })
                            .ok_or("Неизвестная кнопка VK")?;
                        format!("[...document.querySelectorAll('.vk-metrics button')].find(b=>b.textContent==='{label}')?.click()")
                    }
                    "field" => {
                        let label = p["label"]
                            .as_str()
                            .filter(|s| {
                                [
                                    "Название профиля VK",
                                    "Проверочный ролик VK",
                                    "Пользовательский токен VK",
                                    "Профиль статистики VK",
                                    "Найти локальный финал",
                                    "Опубликованный ролик VK",
                                    "Дата публикации VK",
                                ]
                                .contains(s)
                            })
                            .ok_or("Неизвестное поле VK")?;
                        let value =
                            serde_json::to_string(p["value"].as_str().ok_or("Нужно значение")?)
                                .map_err(parse_err)?;
                        format!("{{const e=[...document.querySelectorAll('.vk-metrics label')].find(l=>l.firstElementChild?.textContent==='{label}')?.querySelector('input,select');if(e){{Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLInputElement.prototype,'value').set.call(e,{value});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{{bubbles:true}}));}}}}")
                    }
                    _ => return Err("Неизвестный шаг VK".into()),
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "vk_metrics_account_enabled" => {
                crate::analytics::vk::vk_metrics_account_enabled(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужен профиль VK")?,
                    payload["enabled"]
                        .as_bool()
                        .ok_or("Нужен переключатель сбора")?,
                )?;
                Ok(serde_json::json!(null))
            }
            "notifications_enabled" => Ok(serde_json::json!(
                crate::notifications::notifications_enabled(st.app.state())
            )),
            "notifications_set" => {
                crate::notifications::notifications_set(
                    st.app.state(),
                    payload["enabled"].as_bool().ok_or("Нужен переключатель")?,
                )?;
                Ok(serde_json::json!(null))
            }
            "notification_probe" => {
                if std::env::var_os("AVTO_TEST_NOTIFY").is_none() {
                    return Err("Нужен изолированный тестовый отправитель уведомлений".into());
                }
                crate::notifications::attention(
                    &st.app,
                    "probe:failure".into(),
                    "Проверка уведомлений",
                    "Изолированный тест фабрики",
                );
                Ok(serde_json::json!(null))
            }
            "analytics_ui" => {
                let script=match payload["step"].as_str().unwrap_or("") {
                    "open"=>format!("document.querySelector('[data-publication-id=\"{}\"] button')?.click()",payload["id"].as_i64().ok_or("Нужна публикация")?),
                    "save"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Сохранить замер')?.click()".into(),
                    "apply"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Применить')?.click()".into(),
                    "field"=>{let label=payload["label"].as_str().filter(|s|["Канал","Период","Просмотры","Нравится","Комментарии","Репосты","Дата замера"].contains(s)).ok_or("Неизвестное поле аналитики")?;let value=serde_json::to_string(payload["value"].as_str().ok_or("Нужно значение")?).map_err(parse_err)?;format!("{{const e=[...document.querySelectorAll('label')].find(l=>l.firstElementChild?.textContent==='{label}')?.querySelector('input,select');if(e){{Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLInputElement.prototype,'value').set.call(e,{value});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{{bubbles:true}}));}}}}")},
                    _=>return Err("Неизвестный шаг аналитики".into())
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "analytics_dashboard" => serde_json::to_value(crate::analytics::analytics_dashboard(
                st.app.state(),
                payload["days"].as_u64().unwrap_or(30).clamp(1, 3650) as u32,
                payload["channelId"].as_i64(),
            )?)
            .map_err(parse_err),
            "analytics_history" => serde_json::to_value(crate::analytics::analytics_history(
                st.app.state(),
                payload["publicationId"]
                    .as_i64()
                    .ok_or("Нужна публикация")?,
            )?)
            .map_err(parse_err),
            "analytics_manual" => {
                crate::analytics::analytics_manual(
                    st.app.clone(),
                    st.app.state(),
                    payload["publicationId"]
                        .as_i64()
                        .ok_or("Нужна публикация")?,
                    payload["observedAt"].as_i64(),
                    serde_json::from_value(payload["counts"].clone()).map_err(parse_err)?,
                )?;
                Ok(serde_json::json!(null))
            }
            "analytics_auto" => {
                crate::analytics::analytics_auto(
                    st.app.clone(),
                    st.app.state(),
                    payload["enabled"].as_bool().ok_or("Нужен переключатель")?,
                )?;
                Ok(serde_json::json!(null))
            }
            "analytics_feedback" => {
                serde_json::to_value(crate::analytics::feedback::analytics_feedback(
                    st.app.state(),
                    payload["channelId"].as_i64().ok_or("Нужен канал")?,
                )?)
                .map_err(parse_err)
            }
            "analytics_apply" => {
                crate::analytics::feedback::analytics_apply(
                    st.app.clone(),
                    st.app.state(),
                    payload["channelId"].as_i64().ok_or("Нужен канал")?,
                    payload["signal"].as_str().ok_or("Нужен сигнал")?.into(),
                    payload["current"].as_f64().ok_or("Нужен текущий вес")?,
                )?;
                Ok(serde_json::json!(null))
            }
            "face_model_ready" => Ok(serde_json::json!(crate::camera::face_model_ready(
                st.app.state()
            ))),
            "face_model_download" => Ok(serde_json::json!(
                crate::model_download::face_model_download(st.app.clone())
            )),
            "calendar_autofill_ui" => {
                st.app.get_webview_window("main").ok_or("Нет окна")?.eval("[...document.querySelectorAll('button')].find(b=>b.textContent==='Заполнить по целям каналов')?.click()").map_err(|e|e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "calendar_caption_ui" => {
                let script=match payload["step"].as_str().unwrap_or("") {
                    "open"=>format!("document.querySelector('.factory-calendar-row[data-publication-id=\"{}\"] button')?.click()",payload["id"].as_i64().ok_or("Нужен id")?),
                    "refresh"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Обновить подпись из редактора')?.click()".into(),
                    "scroll"=>"[...document.querySelectorAll('.caption-preview')].find(e=>e.closest('.factory-form'))?.closest('section')?.scrollIntoView({block:'start'})".into(),
                    _=>return Err("Неизвестный шаг подписи календаря".into()),
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "output_profiles" => {
                serde_json::to_value(crate::publish::output::output_profiles(st.app.state())?)
                    .map_err(parse_err)
            }
            "output_profile_save" => {
                serde_json::to_value(crate::publish::output::output_profile_save(
                    st.app.state(),
                    serde_json::from_value(payload).map_err(parse_err)?,
                )?)
                .map_err(parse_err)
            }
            "platform_caps" => {
                serde_json::to_value(crate::publish::platform_caps()).map_err(parse_err)
            }
            "calendar_clips" => {
                serde_json::to_value(crate::factory::factory_calendar(st.app.state())?)
                    .map_err(parse_err)
            }
            "youtube_login_cancel" => {
                crate::publish::youtube::accounts::youtube_login_cancel(st.app.state());
                Ok(serde_json::json!(null))
            }
            "youtube_account_save" => {
                crate::publish::youtube::accounts::youtube_account_save(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужен id")?,
                    payload["label"].as_str().ok_or("Нужно название")?.into(),
                    payload["enabled"].as_bool().ok_or("Нужен enabled")?,
                    serde_json::from_value(payload["policy"].clone()).map_err(parse_err)?,
                    serde_json::from_value(payload["settings"].clone()).map_err(parse_err)?,
                )?;
                Ok(serde_json::json!(null))
            }
            "youtube_ui" => {
                let step = payload["step"].as_str().ok_or("Нужен шаг")?;
                let script=match step{
                "scroll"=>"[...document.querySelectorAll('h2')].find(e=>e.textContent.startsWith('YouTube Shorts ·'))?.closest('section')?.scrollIntoView({block:'start'})".to_string(),
                "select"=>{let id=payload["id"].as_i64().ok_or("Нужен id")?;format!("{{const e=[...document.querySelectorAll('label')].find(l=>l.firstElementChild?.textContent==='Аккаунт YouTube')?.querySelector('select');if(e){{e.value='{id}';e.dispatchEvent(new Event('change',{{bubbles:true}}));}}}}")},
                _=>return Err("Неизвестный шаг YouTube".into()),
            };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "publication_cancel" => {
                crate::publish::calendar::publication_cancel(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужен id")?,
                )?;
                Ok(serde_json::json!("ok"))
            }
            "publication_resolve" => {
                crate::publish::calendar::publication_resolve(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужен id")?,
                    payload["action"].as_str().ok_or("Нужно действие")?.into(),
                )?;
                Ok(serde_json::json!("ok"))
            }
            "publications_pause" => {
                crate::publish_queue::queue_pause(st.app.state());
                Ok(serde_json::json!("ok"))
            }
            "publications" => {
                serde_json::to_value(crate::publish::calendar::publications_list(st.app.state())?)
                    .map_err(parse_err)
            }
            "publication_schedule" => {
                serde_json::to_value(crate::publish::calendar::publication_schedule(
                    st.app.clone(),
                    st.app.state(),
                    payload["draftId"].as_i64().ok_or("Нужен draftId")?,
                    payload["accountId"].as_i64().ok_or("Нужен accountId")?,
                    payload["at"].as_i64(),
                )?)
                .map_err(parse_err)
            }
            "publication_move" => {
                crate::publish::calendar::publication_move(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужен id")?,
                    payload["at"].as_i64().ok_or("Нужно время")?,
                )?;
                Ok(serde_json::json!("ok"))
            }
            "publication_refresh_caption" => {
                crate::publish::calendar::publication_refresh_caption(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужен id")?,
                )?;
                Ok(serde_json::json!("ok"))
            }
            "publications_start" => {
                if std::env::var_os("AVTO_TEST_TELEGRAM_ENDPOINT").is_none() {
                    return Err("Отладочная отправка требует поддельного Bot API".into());
                }
                crate::publish_queue::publications_start(st.app.clone(), st.app.state())?;
                Ok(serde_json::json!("ok"))
            }
            "campaigns" => serde_json::to_value(crate::campaigns::campaigns_list(st.app.state())?)
                .map_err(parse_err),
            "campaign_save" => serde_json::to_value(crate::campaigns::campaign_save(
                st.app.clone(),
                st.app.state(),
                serde_json::from_value(payload).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "campaign_preview" => serde_json::to_value(crate::campaigns::campaign_preview(
                st.app.state(),
                payload["channelId"].as_i64().ok_or("Нужен канал")?,
            )?)
            .map_err(parse_err),
            "campaign_assignments" => {
                serde_json::to_value(crate::campaigns::campaign_assignments(st.app.state())?)
                    .map_err(parse_err)
            }
            "campaign_report_rows" => {
                serde_json::to_value(crate::campaigns_report::campaign_report_rows(
                    st.app.state(),
                    payload["campaignId"].as_i64().ok_or("Нужна кампания")?,
                )?)
                .map_err(parse_err)
            }
            "open_campaign" => {
                let id = payload.as_i64().ok_or("Нужна кампания")?;
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&format!(
                        "document.querySelector('[data-campaign-id=\"{id}\"]')?.click()"
                    ))
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!(null))
            }
            "caption_load" => serde_json::to_value(crate::caption::commands::caption_draft_load(
                st.app.state(),
                payload["draftId"].as_i64().ok_or("Нужен draftId")?,
                serde_json::from_value(payload["platform"].clone()).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "caption_save" => serde_json::to_value(crate::caption::commands::caption_draft_save(
                st.app.state(),
                payload["draftId"].as_i64().ok_or("Нужен draftId")?,
                serde_json::from_value(payload["saved"].clone()).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "caption_apply_body" => {
                serde_json::to_value(crate::caption::commands::caption_apply_body(
                    st.app.state(),
                    serde_json::from_value(payload["draftIds"].clone()).map_err(parse_err)?,
                    serde_json::from_value(payload["platform"].clone()).map_err(parse_err)?,
                    payload["accountId"].as_i64(),
                    payload["body"].as_str().ok_or("Нужен текст")?.into(),
                )?)
                .map_err(parse_err)
            }
            "caption_account_load" => {
                serde_json::to_value(crate::caption::commands::caption_account_load(
                    st.app.state(),
                    payload["draftId"].as_i64().ok_or("Нужен draftId")?,
                    serde_json::from_value(payload["platform"].clone()).map_err(parse_err)?,
                    payload["accountId"].as_i64(),
                )?)
                .map_err(parse_err)
            }
            "caption_account_save" => {
                serde_json::to_value(crate::caption::commands::caption_account_save(
                    st.app.state(),
                    payload["draftId"].as_i64().ok_or("Нужен draftId")?,
                    serde_json::from_value(payload["saved"].clone()).map_err(parse_err)?,
                    payload["accountId"].as_i64(),
                )?)
                .map_err(parse_err)
            }
            "caption_ui" => {
                let step = payload["step"].as_str().ok_or("Нужен шаг caption_ui")?;
                let script=match step {
                    "open"=>"[...document.querySelectorAll('summary')].find(b=>b.textContent==='Подписи для площадок')?.click()".to_string(),
                    "save"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Проверить и сохранить подпись')?.click()".to_string(),
                    "all"=>"[...document.querySelectorAll('button')].find(b=>b.textContent.startsWith('Применить текст ко всем ('))?.click()".to_string(),
                    "generate"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Сгенерировать хэштеги')?.click()".to_string(),
                    "scroll"=>"[...document.querySelectorAll('h2')].find(e=>e.textContent==='Подпись для площадки')?.closest('section')?.scrollIntoView({block:'start'})".to_string(),
                    "field"=>{
                        let label=payload["label"].as_str().filter(|s|["Текст ролика","Рекламная ссылка","Промокод","Название в подписи","Заголовок YouTube","Площадка подписи","Аккаунт подписи","Ролик для подписи"].contains(s)).ok_or("Неизвестное поле подписи")?;
                        let value=serde_json::to_string(payload["value"].as_str().ok_or("Нужно значение")?).map_err(parse_err)?;
                        format!("{{const e=[...document.querySelectorAll('label')].find(l=>l.firstElementChild?.textContent==='{label}')?.querySelector('input,textarea,select');if(e){{Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:e.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value').set.call(e,{value});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{{bubbles:true}}));}}}}")
                    },
                    _=>return Err("Неизвестный шаг caption_ui".into()),
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!("ok"))
            }
            "camera_ui" => {
                let step = payload["step"].as_str().ok_or("Нужен шаг camera_ui")?;
                let script=match step {
                    "open"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Настроить по кадру')?.click()".to_string(),
                    "load"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Показать кадр камеры')?.click()".to_string(),
                    "detect"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Найти лица на кадре')?.click()".to_string(),
                    "scan"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Проверить всю запись')?.click()".to_string(),
                    "cancel"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Остановить проверку')?.click()".to_string(),
                    "face"=>{let i=payload["index"].as_u64().filter(|v|*v<20).ok_or("Номер лица 0–19")?;format!("[...document.querySelectorAll('button')].find(b=>b.textContent.startsWith('Лицо {} ·'))?.click()",i+1)},
                    "accept"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Принять рамку камеры')?.click()".to_string(),
                    "save"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Сохранить профиль')?.click()".to_string(),
                    "scroll"=>"document.querySelector('.camera-profile')?.scrollIntoView({block:'start'})".to_string(),
                    "rect"=>{
                        let key=payload["key"].as_str().filter(|s|["слева","сверху","ширина","высота"].contains(s)).ok_or("Неизвестный параметр камеры")?;
                        let v=payload["value"].as_f64().filter(|n|n.is_finite()&&*n>=0.0&&*n<=100.0).ok_or("Нужны проценты")?;
                        format!("{{const el=[...document.querySelectorAll('label')].find(l=>l.firstElementChild?.textContent==='Камера: {key}, %')?.querySelector('input');if(el){{Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(el,{v});el.dispatchEvent(new Event('input',{{bubbles:true}}));}}}}")
                    },
                    _=>return Err("Неизвестный шаг camera_ui".into()),
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!("ok"))
            }
            "template_ui" => {
                let script=match payload["step"].as_str().unwrap_or("") {
                    "open"=>{let id=payload["id"].as_i64().ok_or("Нужен шаблон")?;format!("document.querySelector('[data-template-id=\"{id}\"]')?.click()")},
                    "preview"=>"[...document.querySelectorAll('button')].find(b=>b.textContent==='Обновить предпросмотр')?.click()".into(),
                    "scroll"=>"document.querySelector('.template-preview-section')?.scrollIntoView({block:'start'})".into(),
                    "field"=>{
                        let label=payload["label"].as_str().filter(|s|["Канал для предпросмотра","Запись для предпросмотра","Секунда предпросмотра","Раскладка"].contains(s)).ok_or("Неизвестное поле шаблона")?;
                        let value=serde_json::to_string(&payload["value"].as_str().ok_or("Нужно строковое значение")?).map_err(parse_err)?;
                        format!("{{const e=[...document.querySelectorAll('label')].find(l=>l.firstElementChild?.textContent==='{label}')?.querySelector('input,select');if(e){{Object.getOwnPropertyDescriptor(e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLInputElement.prototype,'value').set.call(e,{value});e.dispatchEvent(new Event(e.tagName==='SELECT'?'change':'input',{{bubbles:true}}));}}}}")
                    },
                    _=>return Err("Неизвестный шаг template_ui".into()),
                };
                st.app
                    .get_webview_window("main")
                    .ok_or("Нет окна")?
                    .eval(&script)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!("ok"))
            }
            "channel_incidents" => serde_json::to_value(crate::factory::rights::channel_incidents(
                st.app.state(),
                payload["channelId"].as_i64().ok_or("Нужен канал")?,
            )?)
            .map_err(parse_err),
            "channel_incident_add" => {
                serde_json::to_value(crate::factory::rights::channel_incident_add(
                    st.app.clone(),
                    st.app.state(),
                    serde_json::from_value(payload).map_err(parse_err)?,
                )?)
                .map_err(parse_err)
            }
            "channel_incident_resolve" => {
                crate::factory::rights::channel_incident_resolve(
                    st.app.clone(),
                    st.app.state(),
                    payload["id"].as_i64().ok_or("Нужно событие")?,
                )?;
                Ok(serde_json::json!("ok"))
            }
            "channel_rights_resume" => {
                crate::factory::rights::channel_rights_resume(
                    st.app.clone(),
                    st.app.state(),
                    payload["channelId"].as_i64().ok_or("Нужен канал")?,
                )?;
                Ok(serde_json::json!("ok"))
            }
            "channel_save" => serde_json::to_value(crate::factory::profiles::channel_save(
                st.app.clone(),
                st.app.state(),
                serde_json::from_value(payload).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "template_save" => serde_json::to_value(crate::factory::profiles::template_save(
                st.app.clone(),
                st.app.state(),
                serde_json::from_value(payload).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "templates" => {
                serde_json::to_value(crate::factory::profiles::templates_list(st.app.state())?)
                    .map_err(parse_err)
            }
            "assets" => serde_json::to_value(crate::assets_cmds::assets_list(st.app.state())?)
                .map_err(parse_err),
            "projects" => serde_json::to_value(crate::commands::list_projects(st.app.state())?)
                .map_err(parse_err),
            "defaults" => {
                serde_json::to_value(crate::drafts::defaults_get(st.app.state())).map_err(parse_err)
            }
            "timeline" => serde_json::to_value(crate::analysis_inputs::analysis_timeline(
                st.app.state(),
                payload.as_i64().ok_or("Нужен projectId")?,
            )?)
            .map_err(parse_err),
            "inputs" => serde_json::to_value(crate::analysis_inputs::analysis_inputs_get(
                st.app.state(),
                payload.as_i64().ok_or("Нужен projectId")?,
            ))
            .map_err(parse_err),
            "chat_clear" => {
                crate::analysis_inputs::analysis_chat_clear(
                    st.app.state(),
                    payload.as_i64().ok_or("Нужен projectId")?,
                )?;
                Ok(serde_json::json!("ok"))
            }
            "signal_add" => {
                st.app.get_webview_window("main").ok_or("нет окна")?.eval("document.querySelector('[aria-label=\"Начало своего момента\"]')?.closest('section')?.querySelector('button')?.click()").map_err(|e|e.to_string())?;
                Ok(serde_json::json!("ok"))
            }
            "signal_select" => {
                let at = payload
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .ok_or("Нужно время")?;
                st.app.get_webview_window("main").ok_or("нет окна")?.eval(&format!("{{const e=document.querySelector('#signal-time');if(e){{Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,{at});e.dispatchEvent(new Event('input',{{bubbles:true}}));}}}}")).map_err(|e|e.to_string())?;
                Ok(serde_json::json!("ok"))
            }
            "channels" => {
                serde_json::to_value(crate::factory::profiles::channels_list(st.app.state())?)
                    .map_err(parse_err)
            }
            "open_channel" => {
                let id = payload.as_i64().ok_or("нужен id канала")?;
                let window = st.app.get_webview_window("main").ok_or("нет окна")?;
                window
                    .eval(&format!(
                        "document.querySelector('[data-channel-id=\"{id}\"]')?.click()"
                    ))
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!("ok"))
            }
            "output_dir" => {
                crate::drafts::output_dir_set(
                    st.app.state(),
                    payload.as_str().ok_or("нужен путь")?.to_string(),
                )?;
                Ok(serde_json::json!("ok"))
            }
            "power_status" => {
                serde_json::to_value(crate::operations::power::power_status(st.app.state()))
                    .map_err(parse_err)
            }
            "disk_policy" => serde_json::to_value(crate::operations::disk::disk_policy_save(
                st.app.state(),
                serde_json::from_value(payload).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "settings" => serde_json::to_value(crate::factory::resources::factory_settings_set(
                st.app.clone(),
                st.app.state(),
                serde_json::from_value(payload).map_err(parse_err)?,
            )?)
            .map_err(parse_err),
            "add" => Ok(serde_json::json!(crate::factory::batch_add(
                st.app.clone(),
                st.app.state(),
                serde_json::from_value(payload).map_err(parse_err)?
            )?)),
            "start" => {
                crate::factory::batch_start(st.app.clone());
                Ok(serde_json::json!("ok"))
            }
            "pause" => {
                crate::factory::batch_cancel(st.app.state());
                Ok(serde_json::json!("ok"))
            }
            "decide" => Ok(serde_json::json!(crate::factory::factory_decide(
                st.app.clone(),
                st.app.state(),
                serde_json::from_value(payload["ids"].clone()).map_err(parse_err)?,
                payload["approve"].as_bool().unwrap_or(false)
            )?)),
            _ => Err("Неизвестная операция фабрики".into()),
        }
    })();
    reply(result.and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())))
}
