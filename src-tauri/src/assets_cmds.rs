//! Библиотека ассетов: баннеры (видео на зелёном фоне), фоны (геймплей), музыка.
//! Файлы остаются где лежат, в базе хранятся путь и сведения о них.

use std::path::PathBuf;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{media, state::AppState};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];
const AUDIO_EXT: &[&str] = &["mp3", "wav", "m4a", "aac", "ogg", "flac", "opus"];

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Meta {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    pub thumb: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AssetDto {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub name: String,
    #[serde(flatten)]
    pub meta: Meta,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<AssetDto> {
    Ok(AssetDto {
        id: r.get(0)?,
        kind: r.get(1)?,
        path: r.get(2)?,
        name: r.get(3)?,
        meta: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
    })
}

const COLS: &str = "id, kind, path, name, meta_json";

pub(crate) fn load_asset(db: &Connection, id: i64) -> Option<AssetDto> {
    db.query_row(&format!("SELECT {COLS} FROM assets WHERE id = ?1"), [id], from_row).ok()
}

pub(crate) fn list_assets(db: &Connection) -> rusqlite::Result<Vec<AssetDto>> {
    let mut st = db.prepare(&format!("SELECT {COLS} FROM assets ORDER BY kind, created_at DESC, id DESC"))?;
    let rows = st.query_map([], from_row)?;
    rows.collect()
}

#[tauri::command]
pub fn assets_list(state: State<'_, AppState>) -> CmdResult<Vec<AssetDto>> {
    list_assets(&state.db.0.lock().unwrap()).map_err(err)
}

#[tauri::command]
pub async fn asset_add(state: State<'_, AppState>, kind: String, path: String) -> CmdResult<AssetDto> {
    add(&state, &kind, &path).await
}

pub(crate) async fn add(state: &AppState, kind: &str, path: &str) -> CmdResult<AssetDto> {
    if !["banner", "background", "music"].contains(&kind) {
        return Err(format!("неизвестный вид ассета: {kind}"));
    }
    let p = PathBuf::from(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let allowed = if kind == "music" { AUDIO_EXT } else { VIDEO_EXT };
    if !p.is_file() || !allowed.contains(&ext.as_str()) {
        return Err(if kind == "music" {
            "Для музыки подойдут mp3, wav, m4a, ogg, flac".into()
        } else {
            "Это не видеофайл: подойдут mp4, mov, mkv, webm".into()
        });
    }

    let meta = if kind == "music" {
        Meta { duration: media::probe_audio(&p).await.map_err(err)?, has_audio: true, ..Default::default() }
    } else {
        let info = media::probe(&p).await.map_err(err)?;
        let hash = media::quick_hash(&p).await.map_err(err)?;
        let thumb = state.data_dir.join("thumbs").join(format!("{hash}.jpg"));
        // кадр с баннером лучше брать ближе к середине, где он уже не прозрачный
        let at = if kind == "banner" { info.duration * 0.4 } else { info.duration.min(10.0) * 0.3 };
        let thumb = media::make_thumb(&p, &thumb, at).await.ok().map(|_| thumb.display().to_string());
        Meta { duration: info.duration, width: info.width, height: info.height, has_audio: info.has_audio, thumb }
    };

    let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Без названия".into());
    let db = state.db.0.lock().unwrap();
    db.execute(
        "INSERT INTO assets (kind, path, name, meta_json) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET kind = excluded.kind, name = excluded.name, meta_json = excluded.meta_json",
        params![kind, path, name, serde_json::to_string(&meta).unwrap_or_default()],
    )
    .map_err(err)?;
    db.query_row(&format!("SELECT {COLS} FROM assets WHERE path = ?1"), [path], from_row).map_err(err)
}

/// Убирает ассет из библиотеки (файл на диске остаётся).
#[tauri::command]
pub fn asset_delete(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.db.0.lock().unwrap().execute("DELETE FROM assets WHERE id = ?1", [id]).map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_serialises_flat_and_camel_cased() {
        let a = AssetDto {
            id: 1,
            kind: "banner".into(),
            path: "/b.mp4".into(),
            name: "b".into(),
            meta: Meta { duration: 3.0, width: 640, height: 360, has_audio: true, thumb: None },
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["duration"], 3.0);
        assert_eq!(v["hasAudio"], true);
        assert!(v.get("meta").is_none());
    }

    #[test]
    fn broken_meta_does_not_break_listing() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute("INSERT INTO assets (kind, path, name, meta_json) VALUES ('music', '/m.mp3', 'm', 'не json')", []).unwrap();
        let list = list_assets(&c).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].meta.duration, 0.0);
    }
}
