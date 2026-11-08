//! Отладочные инструменты. Компилируются только в debug-сборке (`cargo tauri dev`).
//! Нужны, чтобы снимать настоящее окно приложения и переключать экраны без мыши.

use std::path::PathBuf;

use tauri::{AppHandle, Emitter, Manager};

/// Экраны, на которые разрешено переключаться извне.
const SCREENS: [&str; 5] = ["import", "analysis", "moments", "drafts", "final"];

pub fn go(app: &AppHandle, screen: &str) -> Result<(), String> {
    if !SCREENS.contains(&screen) {
        return Err(format!("неизвестный экран: {screen}"));
    }
    app.emit("dev-go", screen).map_err(|e| e.to_string())
}

/// Импортирует видео так же, как кнопка «Выбрать файл».
pub async fn import(app: &AppHandle, path: String) -> Result<String, String> {
    let p = std::path::Path::new(&path);
    let ok = p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["mp4", "mov", "mkv", "webm"].contains(&e.to_lowercase().as_str()));
    if !ok {
        return Err("нужен существующий видеофайл".into());
    }
    let project = crate::commands::import_video(app.state(), path).await?;
    serde_json::to_string(&project).map_err(|e| e.to_string())
}

/// Проверка ссылки на YouTube, как вставка в поле на экране «Импорт».
pub async fn yt_analyze(app: &AppHandle, url: String) -> Result<String, String> {
    let info = crate::youtube_cmds::youtube_analyze(app.state(), url).await?;
    serde_json::to_string(&info).map_err(|e| e.to_string())
}

/// Скачивание с YouTube, как кнопка «Скачать»: сначала читаем сведения о ролике, потом ставим загрузку в очередь.
pub async fn yt_download(app: &AppHandle, q: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let url = q.get("url").cloned().ok_or("нужен ?url=<ссылка>")?;
    let info = crate::youtube_cmds::youtube_analyze(app.state(), url.clone()).await?;
    let args = crate::youtube_cmds::StartArgs {
        url,
        preset: q.get("preset").cloned().unwrap_or_else(|| "1080".into()),
        title: info.title,
        channel: info.channel,
        thumb: info.thumb,
    };
    crate::youtube_cmds::youtube_download(app.clone(), app.state(), args)
}

/// Кнопка «Остановить» у загрузки с YouTube.
pub fn yt_cancel(app: &AppHandle, job_id: String) -> Result<String, String> {
    crate::youtube_cmds::youtube_cancel(app.state(), job_id);
    Ok("ok".into())
}

/// Переключает экран «Импорт» на вкладку YouTube и подставляет ссылку (для снимков окна).
pub fn yt_tab(app: &AppHandle, url: String) -> Result<(), String> {
    app.emit("dev-yt", url).map_err(|e| e.to_string())
}

/// Добавляет ассет в библиотеку так же, как кнопка «Добавить».
pub async fn asset(app: &AppHandle, kind: String, path: String) -> Result<String, String> {
    let a = crate::assets_cmds::add(&app.state(), &kind, &path).await?;
    serde_json::to_string(&a).map_err(|e| e.to_string())
}

/// Принимает все моменты проекта (как если бы пользователь нажал → на каждом).
pub fn accept_all(app: &AppHandle, project_id: i64) -> Result<String, String> {
    let n = app
        .state::<crate::state::AppState>()
        .db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE moments SET status = 'accepted' WHERE project_id = ?1 AND status = 'candidate'", [project_id])
        .map_err(|e| e.to_string())?;
    Ok(format!("принято моментов: {n}"))
}

/// Собирает черновики, как кнопка «Собрать черновики».
pub fn generate(app: &AppHandle, project_id: i64) -> Result<String, String> {
    let n = crate::drafts::drafts_generate(app.clone(), app.state(), project_id)?;
    Ok(format!("создано черновиков: {n}"))
}

/// Пересобирает черновик, как кнопка «Повторить» (с подстановкой выбранного позже фона).
pub fn rerender(app: &AppHandle, draft_id: i64) {
    crate::drafts::draft_rerender(app.clone(), app.state(), draft_id);
}

/// Запускает финальный рендер одобренных черновиков, как кнопка «Финальный рендер».
pub fn finals(app: &AppHandle, project_id: i64) -> Result<String, String> {
    let n = crate::drafts::finals_start(app.clone(), app.state(), project_id)?;
    Ok(format!("запущено финалов: {n}"))
}

/// Настройки TikTok, как кнопка «Сохранить» (только для проверки на макете).
pub async fn tiktok_settings(app: &AppHandle, q: &std::collections::HashMap<String, String>) -> Result<String, String> {
    use crate::tiktok_cmds::{tiktok_save_settings, SettingsArgs};
    let get = |k: &str, d: &str| q.get(k).cloned().unwrap_or_else(|| d.to_string());
    let args = SettingsArgs {
        client_key: get("key", ""),
        client_secret: q.get("secret").cloned(),
        port: get("port", "38475").parse().map_err(|_| "port".to_string())?,
        mode: get("mode", "inbox"),
        method: get("method", "api"),
        privacy: get("privacy", "SELF_ONLY"),
        auto_publish: get("auto", "0") == "1",
    };
    let st = tiktok_save_settings(app.state(), args).await?;
    serde_json::to_string(&st).map_err(|e| e.to_string())
}

pub async fn tiktok_status(app: &AppHandle) -> Result<String, String> {
    serde_json::to_string(&crate::tiktok_cmds::tiktok_status(app.state())).map_err(|e| e.to_string())
}

/// Кнопка «Подключить аккаунт».
pub async fn tiktok_login(app: &AppHandle) -> Result<String, String> {
    crate::tiktok_cmds::tiktok_login_start(app.clone(), app.state()).await.map(|_| "вход запущен".into())
}

/// Кнопка «Отправить в TikTok» в диалоге.
pub async fn tiktok_publish(app: &AppHandle, q: &std::collections::HashMap<String, String>) -> Result<String, String> {
    use crate::tiktok_cmds::{tiktok_publish, PublishArgs};
    let get = |k: &str, d: &str| q.get(k).cloned().unwrap_or_else(|| d.to_string());
    let args = PublishArgs {
        draft_id: get("draft", "0").parse().map_err(|_| "нужен ?draft=<id>".to_string())?,
        caption: get("caption", ""),
        hashtags: get("tags", "").split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
        privacy: get("privacy", "SELF_ONLY"),
        allow_comment: get("comment", "0") == "1",
        allow_duet: false,
        allow_stitch: false,
        force: get("force", "0") == "1",
    };
    let r = tiktok_publish(app.clone(), app.state(), args).await?;
    serde_json::to_string(&r).map_err(|e| e.to_string())
}

/// Очередь автовыкладки: состояние, написание описаний, запуск (с согласием, как кнопка в диалоге), остановка.
pub fn queue_state(app: &AppHandle, project_id: i64) -> Result<String, String> {
    let q = crate::publish_queue::queue_state(app.state(), project_id)?;
    serde_json::to_string(&q).map_err(|e| e.to_string())
}

pub async fn queue_prepare(app: &AppHandle, project_id: i64) -> Result<String, String> {
    crate::publish_queue::queue_prepare(app.clone(), app.state(), project_id).await.map(|_| "описания пишутся".into())
}

pub fn queue_start(app: &AppHandle, project_id: i64) -> Result<String, String> {
    crate::publish_queue::queue_start(app.clone(), app.state(), project_id, true).map(|_| "очередь запущена".into())
}

pub fn queue_stop(app: &AppHandle, project_id: i64) -> Result<String, String> {
    crate::publish_queue::queue_stop(app.clone(), app.state(), project_id).map(|_| "очередь остановлена".into())
}

pub fn queue_apply_text(app: &AppHandle, project_id: i64, q: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let tags = q.get("tags").map(|t| t.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    crate::publish_queue::queue_apply_text(app.clone(), app.state(), project_id, q.get("text").cloned().unwrap_or_default(), tags).map(|n| format!("текст назначен роликам: {n}"))
}

pub fn queue_settings(app: &AppHandle, q: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let get = |k: &str| q.get(k).and_then(|v| v.parse::<i64>().ok()).ok_or_else(|| format!("нужен ?{k}=<число>"));
    let s = crate::publish_queue::queue_settings_set(app.state(), get("pause")?, get("limit")?)?;
    serde_json::to_string(&s).map_err(|e| e.to_string())
}

/// Убирает все следы проверки: секрет из связки ключей, вход и настройки TikTok.
pub async fn tiktok_reset(app: &AppHandle) -> Result<String, String> {
    let st = app.state::<crate::state::AppState>();
    crate::tiktok_cmds::tiktok_forget_secret(app.state()).await?;
    st.db.0.lock().unwrap().execute("DELETE FROM kv WHERE key LIKE 'tiktok_%'", []).map_err(|e| e.to_string())?;
    Ok("TikTok сброшен".into())
}

/// Открывает редактор черновика (как кнопка «Править»).
pub fn edit(app: &AppHandle, draft_id: i64) -> Result<(), String> {
    app.emit("dev-edit", draft_id).map_err(|e| e.to_string())
}

/// Выбирает проект в интерфейсе (как клик по карточке).
pub fn select(app: &AppHandle, project_id: i64) -> Result<(), String> {
    app.emit("dev-select", project_id).map_err(|e| e.to_string())
}

/// Запускает анализ так же, как кнопка «Начать анализ» (настройки по умолчанию).
pub fn analyze(app: &AppHandle, project_id: i64) -> Result<(), String> {
    crate::analysis_cmds::analysis_start(app.clone(), app.state(), project_id, None)
}

/// PNG видимой области веб-вью. Снимает сам WebKitGTK, так что WebGL и backdrop-filter видны как есть.
#[cfg(target_os = "linux")]
pub async fn snapshot(app: &AppHandle, path: PathBuf) -> Result<(), String> {
    use webkit2gtk::{gio, SnapshotOptions, SnapshotRegion, WebViewExt};

    let win = app.get_webview_window("main").ok_or("окно main не найдено")?;
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    win.with_webview(move |wv| {
        wv.inner().snapshot(
            SnapshotRegion::Visible,
            SnapshotOptions::NONE,
            gio::Cancellable::NONE,
            move |res| {
                let r = res.map_err(|e| e.to_string()).and_then(|surface| {
                    let img = cairo::ImageSurface::try_from(surface)
                        .map_err(|_| "снимок не является ImageSurface".to_string())?;
                    let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                    img.write_to_png(&mut f).map_err(|e| e.to_string())
                });
                let _ = tx.send(r);
            },
        );
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "linux"))]
pub async fn snapshot(_app: &AppHandle, _path: PathBuf) -> Result<(), String> {
    Err("снимок окна реализован только для Linux".into())
}
