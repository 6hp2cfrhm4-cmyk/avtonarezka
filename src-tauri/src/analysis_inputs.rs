//! Дополнительные сигналы и ограниченная по размеру тепловая карта для интерфейса.
use crate::host::{AppHandle, Manager, State};
use crate::{
    analyze::{
        chat::{self, ChatFeatures},
        clips::ClipFeatures,
        scout::ScoutReport,
        Signals,
    },
    state::AppState,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
type Result<T> = std::result::Result<T, String>;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inputs {
    pub chat: Option<ChatFeatures>,
    pub clips: Option<ClipFeatures>,
    pub chat_error: Option<String>,
}
pub fn load(db: &Connection, project: i64) -> Inputs {
    let raw: Option<(Option<String>, Option<String>)> = db
        .query_row(
            "SELECT chat_json,clips_json FROM analysis_sources WHERE project_id=?1",
            [project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (chat, clips) = raw.unwrap_or_default();
    let chat_error = db
        .query_row(
            "SELECT chat_error FROM analysis_sources WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Inputs {
        chat: chat.and_then(|v| serde_json::from_str(&v).ok()),
        clips: clips.and_then(|v| serde_json::from_str(&v).ok()),
        chat_error,
    }
}
pub(crate) fn save_chat(db: &Connection, project: i64, chat: &ChatFeatures) -> Result<()> {
    db.execute("INSERT INTO analysis_sources(project_id,chat_json,chat_attempted) VALUES(?1,?2,1) ON CONFLICT(project_id) DO UPDATE SET chat_json=excluded.chat_json,chat_attempted=1,chat_error=NULL,updated_at=unixepoch()",params![project,serde_json::to_string(chat).map_err(|e|e.to_string())?]).map(|_|()).map_err(|e|e.to_string())
}
fn duration(db: &Connection, project: i64) -> Result<f64> {
    db.query_row(
        "SELECT duration FROM projects WHERE id=?1",
        [project],
        |r| r.get(0),
    )
    .map_err(|_| "Проект не найден".into())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_inputs_get(state: State<'_, AppState>, project_id: i64) -> Inputs {
    load(&state.db.0.lock().unwrap(), project_id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn analysis_chat_import(app: AppHandle, project_id: i64, path: String) -> Result<Inputs> {
    let d = duration(&app.state::<AppState>().db.0.lock().unwrap(), project_id)?;
    let chat = tokio::task::spawn_blocking(move || chat::load(std::path::Path::new(&path), d))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))?;
    let state = app.state::<AppState>();
    let running = state.analyses.lock().unwrap();
    if running.contains_key(&project_id) {
        return Err("Дождись завершения анализа перед сменой чата".into());
    }
    let db = state.db.0.lock().unwrap();
    save_chat(&db, project_id, &chat)?;
    Ok(load(&db, project_id))
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_chat_clear(state: State<'_, AppState>, project_id: i64) -> Result<()> {
    let running = state.analyses.lock().unwrap();
    if running.contains_key(&project_id) {
        return Err("Дождись завершения анализа".into());
    }
    state.db.0.lock().unwrap().execute("UPDATE analysis_sources SET chat_json=NULL,chat_error=NULL,chat_attempted=0,updated_at=unixepoch() WHERE project_id=?1",[project_id]).map(|_|()).map_err(|e|e.to_string())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    pub duration: f64,
    pub hop: f64,
    pub series: Vec<Series>,
    pub scout: Option<ScoutReport>,
}
#[derive(Serialize)]
pub struct Series {
    pub key: &'static str,
    pub label: &'static str,
    pub values: Option<Vec<f32>>,
}
pub(crate) fn timeline(db: &Connection, project: i64) -> Result<Timeline> {
    let duration = duration(db, project)?;
    let json: Option<String> = db
        .query_row(
            "SELECT signals_json FROM analysis WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let s: Signals = json
        .map(|v| serde_json::from_str(&v))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let n = (duration / crate::analyze::HOP).ceil() as usize;
    let stride = n.div_ceil(360).max(1);
    let down = |v: Option<Vec<f32>>| {
        v.filter(|v| !v.is_empty()).map(|v| {
            v.chunks(stride)
                .map(|c| {
                    c.iter()
                        .copied()
                        .filter(|v| v.is_finite())
                        .fold(0.0f32, f32::max)
                        .clamp(0.0, 8.0)
                })
                .collect()
        })
    };
    Ok(Timeline {
        duration,
        hop: stride as f64 * crate::analyze::HOP,
        series: vec![
            Series {
                key: "loudness",
                label: "Звук",
                values: down(Some(s.loudness)),
            },
            Series {
                key: "events",
                label: "Смех и события",
                values: down(s.events),
            },
            Series {
                key: "chat",
                label: "Чат",
                values: down(s.chat),
            },
            Series {
                key: "viewer_clips",
                label: "Клипы зрителей",
                values: down(s.viewer_clips),
            },
            Series {
                key: "camera",
                label: "Активность камеры",
                values: down(s.camera),
            },
            Series {
                key: "motion",
                label: "Движение",
                values: down(Some(s.motion)),
            },
            Series {
                key: "speech",
                label: "Темп речи",
                values: down(s.speech),
            },
        ],
        scout: s.scout,
    })
}
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn analysis_timeline(state: State<'_, AppState>, project_id: i64) -> Result<Timeline> {
    timeline(&state.db.0.lock().unwrap(), project_id)
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct TwitchCredentials {
    pub(crate) client_id: String,
    pub(crate) client_secret: String,
}
pub(crate) const TWITCH_KEY: &str = "factory_twitch_app";
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TwitchStatus {
    pub configured: bool,
    pub storage: &'static str,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn twitch_signal_status(state: State<'_, AppState>) -> Result<TwitchStatus> {
    Ok(TwitchStatus {
        configured: state
            .secrets
            .get(TWITCH_KEY)
            .await
            .map_err(|e| e.to_string())?
            .is_some(),
        storage: state.secrets.backend(),
    })
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn twitch_signal_credentials(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
) -> Result<()> {
    let client_id = client_id.trim();
    let client_secret = client_secret.trim();
    if client_id.is_empty() && client_secret.is_empty() {
        return state
            .secrets
            .delete(TWITCH_KEY)
            .await
            .map_err(|e| e.to_string());
    }
    if client_id.is_empty()
        || client_secret.is_empty()
        || client_id.len() > 256
        || client_secret.len() > 1024
    {
        return Err("Укажи Client ID и Client Secret приложения Twitch".into());
    }
    let data = serde_json::to_string(&TwitchCredentials {
        client_id: client_id.into(),
        client_secret: client_secret.into(),
    })
    .map_err(|e| e.to_string())?;
    state
        .secrets
        .set(TWITCH_KEY, &data)
        .await
        .map_err(|e| e.to_string())
}
pub(crate) async fn fetch_clips(app: &AppHandle, project: i64, vod: &str) -> Result<ClipFeatures> {
    let state = app.state::<AppState>();
    let value = state
        .secrets
        .get(TWITCH_KEY)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Для клипов зрителей нужны Client ID/Secret Twitch в настройках анализа")?;
    let credentials: TwitchCredentials =
        serde_json::from_str(&value).map_err(|_| "Повреждены настройки Twitch")?;
    let duration = duration(&state.db.0.lock().unwrap(), project)?;
    crate::analyze::clips::TwitchApi::default()
        .collect(
            &credentials.client_id,
            &credentials.client_secret,
            vod,
            duration,
        )
        .await
        .map_err(|e| format!("{e:#}"))
}
pub(crate) fn save_clips(db: &Connection, project: i64, clips: &ClipFeatures) -> Result<()> {
    db.execute("INSERT INTO analysis_sources(project_id,clips_json) VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET clips_json=excluded.clips_json,updated_at=unixepoch()",params![project,serde_json::to_string(clips).map_err(|e|e.to_string())?]).map(|_|()).map_err(|e|e.to_string())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn analysis_twitch_clips(
    app: AppHandle,
    project_id: i64,
    vod_id: String,
) -> Result<Inputs> {
    let clips = fetch_clips(&app, project_id, vod_id.trim()).await?;
    let state = app.state::<AppState>();
    let running = state.analyses.lock().unwrap();
    if running.contains_key(&project_id) {
        return Err("Дождись завершения анализа перед сменой сигналов".into());
    }
    let db = state.db.0.lock().unwrap();
    save_clips(&db, project_id, &clips)?;
    Ok(load(&db, project_id))
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_clips_clear(state: State<'_, AppState>, project_id: i64) -> Result<()> {
    let running = state.analyses.lock().unwrap();
    if running.contains_key(&project_id) {
        return Err("Дождись завершения анализа".into());
    }
    state.db.0.lock().unwrap().execute("UPDATE analysis_sources SET clips_json=NULL,updated_at=unixepoch() WHERE project_id=?1",[project_id]).map(|_|()).map_err(|e|e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extra_inputs_and_old_analysis_survive_restart_and_timeline_is_bounded() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio) VALUES(1,'/v','h','v',10800,160,90,10,1)",[]).unwrap();
        let chat = crate::analyze::chat::parse(
            br#"{"comments":[{"content_offset_seconds":1200,"message":{"body":"Pog"}}]}"#,
            10800.0,
        )
        .unwrap();
        save_chat(&db, 1, &chat).unwrap();
        assert_eq!(load(&db, 1).chat.unwrap().messages, 1);
        let old = r#"{"loudness":[0,8,0],"motion":[],"events":null,"speech":null,"cuts":[]}"#;
        db.execute("INSERT INTO analysis(project_id,signals_json,words_json,sentences_json) VALUES(1,?1,'[]','[]')",[old]).unwrap();
        assert!(timeline(&db, 1).unwrap().scout.is_none());
        let sig = Signals {
            loudness: vec![4.0; 21600],
            chat: Some(chat.curve),
            camera: Some(vec![2.0; 21600]),
            ..Default::default()
        };
        db.execute(
            "UPDATE analysis SET signals_json=?1",
            [serde_json::to_string(&sig).unwrap()],
        )
        .unwrap();
        let t = timeline(&db, 1).unwrap();
        assert!(t
            .series
            .iter()
            .any(|s| s.key == "camera" && s.values.as_ref().is_some_and(|v| v[0] == 2.0)));
        assert!(t
            .series
            .iter()
            .filter_map(|s| s.values.as_ref())
            .all(|v| v.len() <= 360));
        db.execute("DELETE FROM projects WHERE id=1", []).unwrap();
        assert!(load(&db, 1).chat.is_none());
    }
}
