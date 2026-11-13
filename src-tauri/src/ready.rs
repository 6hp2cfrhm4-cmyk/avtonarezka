//! Готовые ролики из папки: уже отрендеренные вертикальные видео выкладываются в TikTok без рендера.
//! Каждый такой ролик — своя строка `ready_videos` и своя задача очереди: текст им задаётся «один на все» или вручную.

use std::path::Path;

use rusqlite::params;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{media, publish_queue, state::AppState};

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyDto {
    pub id: i64,
    pub name: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// Не вертикальный кадр 9:16 — ролик всё равно добавляется, но человек должен знать, что TikTok его обрежет или покажет с полями.
    pub warning: Option<String>,
}

fn name_of(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// Добавляет выбранные файлы в проект. Файл, который уже лежит в проекте, не дублируется.
#[tauri::command]
pub async fn ready_add(app: AppHandle, state: State<'_, AppState>, project_id: i64, paths: Vec<String>) -> CmdResult<Vec<ReadyDto>> {
    let mut added = Vec::new();
    for path in paths {
        let info = media::probe(Path::new(&path))
            .await
            .map_err(|e| format!("«{}» не открывается как видео: {e:#}", name_of(&path)))?;
        let name = name_of(&path);
        let id = {
            let db = state.db.0.lock().unwrap();
            db.execute(
                "INSERT INTO ready_videos (project_id, path, name, duration, width, height) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, path) DO NOTHING",
                params![project_id, path, name, info.duration, info.width, info.height],
            )
            .map_err(|e| e.to_string())?;
            let id: i64 = db
                .query_row("SELECT id FROM ready_videos WHERE project_id = ?1 AND path = ?2", params![project_id, path], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            // задача без текста: его зададут «один текст на все» или вручную; уже существующую не трогаем
            db.execute("INSERT INTO publish_jobs (ready_id, state) VALUES (?1, 'drafting') ON CONFLICT(ready_id) DO NOTHING", [id])
                .map_err(|e| e.to_string())?;
            id
        };
        let vertical = info.width * 16 == info.height * 9;
        let warning = (!vertical).then(|| format!("кадр {}×{}, а нужен вертикальный 9:16 (1080×1920)", info.width, info.height));
        added.push(ReadyDto { id, name, duration: info.duration, width: info.width, height: info.height, warning });
    }
    publish_queue::changed(&app, project_id);
    Ok(added)
}

/// Убирает готовый ролик из проекта. Выложенный и уходящий сейчас не трогаем: иначе сбился бы суточный лимит аккаунта.
#[tauri::command]
pub fn ready_remove(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let db = state.db.0.lock().unwrap();
    let project_id: i64 = db
        .query_row("SELECT project_id FROM ready_videos WHERE id = ?1", [id], |r| r.get(0))
        .map_err(|_| "Ролик не найден".to_string())?;
    let job_state: Option<String> = db.query_row("SELECT state FROM publish_jobs WHERE ready_id = ?1", [id], |r| r.get(0)).ok();
    if matches!(job_state.as_deref(), Some("sending") | Some("done")) {
        return Err("Этот ролик уже выкладывается или выложен: убрать его нельзя".into());
    }
    db.execute("DELETE FROM ready_videos WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
    drop(db);
    publish_queue::changed(&app, project_id);
    Ok(())
}
