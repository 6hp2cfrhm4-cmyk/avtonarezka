//! Команды скачивания с YouTube: проверка ссылки, загрузка, остановка. Сам движок — в `youtube::engine`.
//!
//! Загрузка идёт в фоне: команда сразу возвращает id задачи, а ход сообщают события `youtube-progress`, `youtube-done`
//! и `youtube-error`. Каждое progress-событие — полный снимок задачи, так что интерфейс просто заменяет у себя запись.
//! Готовый файл попадает в проекты тем же `import_video`, что и выбранный вручную.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

use crate::{
    commands::{self, Project},
    drafts::output_dir,
    state::AppState,
    youtube::{
        engine::{self, DownloadError, DownloadRequest, Stage, VideoInfo},
        progress::Snapshot,
        select,
        url::extract_video_id,
    },
};

type CmdResult<T> = Result<T, String>;

/// Куда складываются скачанные видео: внутри папки готовых роликов.
const SUBDIR: &str = "Скачанное";
const MAX_THUMB: u64 = 2 * 1024 * 1024;
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

fn canonical(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

/// Скачивает обложку в `thumbs/` (один раз на ролик). Без обложки интерфейс показывает заглушку.
async fn fetch_thumb(data_dir: &Path, id: &str, from_info: Option<&str>) -> Option<PathBuf> {
    let dir = data_dir.join("thumbs");
    for ext in ["webp", "jpg", "png"] {
        let cached = dir.join(format!("yt-{id}.{ext}"));
        if cached.is_file() {
            return Some(cached);
        }
    }
    let fallback = format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg");
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().ok()?;
    for url in from_info.into_iter().chain(Some(fallback.as_str())) {
        // только картинки YouTube: адрес пришёл из ответа yt-dlp, ходить по произвольным адресам незачем
        let host_ok = reqwest::Url::parse(url).ok().is_some_and(|u| {
            u.scheme() == "https" && u.host_str().is_some_and(|h| h == "ytimg.com" || h.ends_with(".ytimg.com"))
        });
        if !host_ok {
            continue;
        }
        let ext = match url.split('?').next().and_then(|u| u.rsplit('.').next()) {
            Some("webp") => "webp",
            Some("png") => "png",
            _ => "jpg",
        };
        let Ok(resp) = client.get(url).send().await.and_then(|r| r.error_for_status()) else { continue };
        if resp.content_length().is_some_and(|n| n > MAX_THUMB) {
            continue;
        }
        let Ok(bytes) = resp.bytes().await else { continue };
        if bytes.is_empty() || bytes.len() as u64 > MAX_THUMB {
            continue;
        }
        tokio::fs::create_dir_all(&dir).await.ok()?;
        // пишем во временный файл и переименовываем: оборванная обложка не должна остаться на месте
        let path = dir.join(format!("yt-{id}.{ext}"));
        let tmp = dir.join(format!("yt-{id}.{ext}.part"));
        if tokio::fs::write(&tmp, &bytes).await.is_ok() && tokio::fs::rename(&tmp, &path).await.is_ok() {
            return Some(path);
        }
    }
    None
}

/// Проверяет ссылку и читает сведения о ролике: название, канал, длительность, обложка, доступное качество.
#[tauri::command]
pub async fn youtube_analyze(state: State<'_, AppState>, url: String) -> CmdResult<VideoInfo> {
    let id = extract_video_id(&url).map_err(|e| e.to_string())?;
    let ytdlp = engine::find_ytdlp().ok_or_else(|| engine::NOT_FOUND.to_string())?;
    let mut info = engine::analyze(&ytdlp, &canonical(&id), &id).await?;
    if let Some(path) = fetch_thumb(&state.data_dir, &id, info.thumbnail_url.as_deref()).await {
        info.thumb = Some(state.media.register(path));
    }
    Ok(info)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartArgs {
    pub url: String,
    pub preset: String,
    /// Название, канал и обложка нужны только для показа: задача возвращает их в событиях как есть.
    pub title: String,
    pub channel: String,
    pub thumb: Option<String>,
}

#[derive(Clone)]
struct Job {
    id: String,
    video_id: String,
    url: String,
    preset: String,
    title: String,
    channel: String,
    thumb: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent<'a> {
    job_id: &'a str,
    video_id: &'a str,
    url: &'a str,
    preset: &'a str,
    title: &'a str,
    channel: &'a str,
    thumb: Option<&'a str>,
    /// `queued`, `downloading`, `merging`, `finishing` или `importing`.
    stage: &'a str,
    #[serde(flatten)]
    snapshot: &'a Snapshot,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DoneEvent<'a> {
    job_id: &'a str,
    project: Project,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent<'a> {
    job_id: &'a str,
    message: String,
    cancelled: bool,
}

fn emit_progress(app: &AppHandle, job: &Job, stage: &str, snapshot: &Snapshot) {
    let _ = app.emit(
        "youtube-progress",
        ProgressEvent {
            job_id: &job.id,
            video_id: &job.video_id,
            url: &job.url,
            preset: &job.preset,
            title: &job.title,
            channel: &job.channel,
            thumb: job.thumb.as_deref(),
            stage,
            snapshot,
        },
    );
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Downloading => "downloading",
        Stage::Merging => "merging",
        Stage::Finishing => "finishing",
    }
}

/// Ждёт сигнала отмены.
async fn cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Ставит загрузку в очередь. Возвращает id задачи; дальше всё сообщают события.
#[tauri::command]
pub fn youtube_download(app: AppHandle, state: State<'_, AppState>, args: StartArgs) -> CmdResult<String> {
    let video_id = extract_video_id(&args.url).map_err(|e| e.to_string())?;
    select::height_limit(&args.preset)?;
    let ytdlp = engine::find_ytdlp().ok_or_else(|| engine::NOT_FOUND.to_string())?;
    let out_dir = output_dir(&state.db.0.lock().unwrap()).join(SUBDIR);

    let job = Job {
        id: format!("yt{}", NEXT_JOB.fetch_add(1, Ordering::Relaxed)),
        url: canonical(&video_id),
        video_id,
        preset: args.preset,
        title: args.title,
        channel: args.channel,
        thumb: args.thumb,
    };
    let (tx, rx) = watch::channel(false);
    {
        let mut jobs = state.downloads.lock().unwrap();
        if jobs.values().any(|(video, _)| *video == job.video_id) {
            return Err("Это видео уже скачивается.".into());
        }
        jobs.insert(job.id.clone(), (job.video_id.clone(), tx));
    }

    let id = job.id.clone();
    tauri::async_runtime::spawn(run(app, job, ytdlp, out_dir, rx));
    Ok(id)
}

async fn run(app: AppHandle, job: Job, ytdlp: PathBuf, out_dir: PathBuf, mut cancel: watch::Receiver<bool>) {
    let state = app.state::<AppState>();
    emit_progress(&app, &job, "queued", &Snapshot::default());

    // Ждём свободное место; пока ждём, загрузку можно остановить.
    let permit = tokio::select! {
        p = state.youtube_slots.clone().acquire_owned() => p.ok(),
        _ = cancelled(&mut cancel) => None,
    };

    let result = match permit {
        None => Err(DownloadError::Cancelled),
        Some(_permit) => {
            // очередь прошли; пока yt-dlp разбирается с YouTube, интерфейс пишет «Подключаюсь…»
            emit_progress(&app, &job, "downloading", &Snapshot::default());
            let req = DownloadRequest {
                url: job.url.clone(),
                video_id: job.video_id.clone(),
                preset: job.preset.clone(),
                out_dir,
                ffmpeg_dir: engine::find_ffmpeg_dir(),
            };
            engine::download(&ytdlp, &req, cancel.clone(), |u| emit_progress(&app, &job, stage_name(u.stage), &u.snapshot)).await
        }
    };

    let outcome: Result<Project, (String, bool)> = match result {
        Ok(path) => {
            emit_progress(&app, &job, "importing", &Snapshot { fraction: Some(1.0), ..Snapshot::default() });
            commands::import_video(app.state(), path.display().to_string())
                .await
                .map_err(|e| (format!("Видео скачано ({}), но не добавилось в проекты: {e}", path.display()), false))
        }
        Err(DownloadError::Cancelled) => Err(("Остановлено — можно продолжить".to_string(), true)),
        Err(DownloadError::Failed(message)) => Err((message, false)),
    };

    state.downloads.lock().unwrap().remove(&job.id);
    match outcome {
        Ok(project) => {
            let _ = app.emit("youtube-done", DoneEvent { job_id: &job.id, project });
        }
        Err((message, cancelled)) => {
            let _ = app.emit("youtube-error", ErrorEvent { job_id: &job.id, message, cancelled });
        }
    }
}

/// Останавливает загрузку. Частично скачанное остаётся: повторный запуск продолжит с того же места.
#[tauri::command]
pub fn youtube_cancel(state: State<'_, AppState>, job_id: String) {
    if let Some((_, tx)) = state.downloads.lock().unwrap().get(&job_id) {
        let _ = tx.send(true);
    }
}
