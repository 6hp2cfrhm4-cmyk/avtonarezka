use std::path::PathBuf;

use rusqlite::params;
use serde::Serialize;
use tauri::State;

use crate::{
    media::{self, EnvReport, VideoInfo},
    state::AppState,
};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    data_dir: String,
    env: EnvReport,
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> CmdResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: state.data_dir.display().to_string(),
        env: media::check_env().await,
    })
}

/// URL для `<video src>` / `<img src>`.
#[tauri::command]
pub fn media_url(state: State<'_, AppState>, path: String) -> String {
    state.media.register(PathBuf::from(path))
}

#[derive(Serialize, Clone)]
pub struct Project {
    pub(crate) id: i64,
    path: String,
    pub(crate) title: String,
    duration: f64,
    width: u32,
    height: u32,
    fps: f64,
    has_audio: bool,
    status: String,
    created_at: i64,
    /// Видео уже размечено: анализ режет его целиком подряд.
    tight: bool,
}

const PROJECT_COLS: &str =
    "id, path, title, duration, width, height, fps, has_audio, status, created_at, tight";

fn project_from_row(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        path: r.get(1)?,
        title: r.get(2)?,
        duration: r.get(3)?,
        width: r.get(4)?,
        height: r.get(5)?,
        fps: r.get(6)?,
        has_audio: r.get::<_, i64>(7)? != 0,
        status: r.get(8)?,
        created_at: r.get(9)?,
        tight: r.get::<_, i64>(10)? != 0,
    })
}

/// Добавляет видео в проекты. Тот же файл (по отпечатку) второй раз не дублируется.
#[tauri::command]
pub async fn import_video(state: State<'_, AppState>, path: String) -> CmdResult<Project> {
    let p = PathBuf::from(&path);
    let info: VideoInfo = media::probe(&p).await.map_err(err)?;
    let hash = media::quick_hash(&p).await.map_err(err)?;
    let title = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Без названия".into());

    let db = state.db.0.lock().unwrap();
    db.execute(
        "INSERT INTO projects (path, hash, title, duration, width, height, fps, has_audio)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(hash) DO UPDATE SET path = excluded.path",
        params![path, hash, title, info.duration, info.width, info.height, info.fps, info.has_audio as i64],
    )
    .map_err(err)?;
    db.query_row(
        &format!("SELECT {PROJECT_COLS} FROM projects WHERE hash = ?1"),
        [hash],
        project_from_row,
    )
    .map_err(err)
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> CmdResult<Vec<Project>> {
    let db = state.db.0.lock().unwrap();
    let mut stmt = db
        .prepare(&format!("SELECT {PROJECT_COLS} FROM projects ORDER BY created_at DESC, id DESC"))
        .map_err(err)?;
    let rows = stmt.query_map([], project_from_row).map_err(err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// Отметка «видео уже размечено, резать вплотную». Применяется при следующем анализе.
#[tauri::command]
pub fn project_set_tight(state: State<'_, AppState>, id: i64, tight: bool) -> CmdResult<Project> {
    let db = state.db.0.lock().unwrap();
    let n = db.execute("UPDATE projects SET tight = ?2 WHERE id = ?1", params![id, tight as i64]).map_err(err)?;
    if n == 0 {
        return Err("проект не найден".into());
    }
    db.query_row(&format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1"), [id], project_from_row).map_err(err)
}

#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let db = state.db.0.lock().unwrap();
    db.execute("DELETE FROM projects WHERE id = ?1", [id]).map_err(err)?;
    Ok(())
}
