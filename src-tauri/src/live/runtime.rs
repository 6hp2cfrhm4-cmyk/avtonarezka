//! Live в работе: реестр сессий, связь движка с базой и очередью фабрики, автозапуск по отмеченным эфирам, команды окна.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use rusqlite::{params, Connection};
use serde::Serialize;
use tokio::sync::{mpsc, watch};

use super::{
    chat,
    engine::{self, ChatTarget, Cmd, Engine, Event, Input},
};
use crate::{
    discovery::store as trends,
    factory::{self, NewItem},
    host::{AppHandle, Emitter, Manager, State},
    state::AppState,
    workflows::{runtime::is_enabled, Flow, NodeKind},
};

type CmdResult<T> = Result<T, String>;

/// Правило G5: одновременно не больше двух эфиров.
pub const MAX_SESSIONS: usize = 2;
const MIN_FREE_MB: u64 = 3 * 1024;
const TWITCH_IRC: (&str, u16) = ("irc.chat.twitch.tv", 6667);

struct Handle {
    stop: watch::Sender<bool>,
    cmd: mpsc::UnboundedSender<Cmd>,
    manual: bool,
}
fn registry() -> &'static Mutex<HashMap<i64, Handle>> {
    static R: OnceLock<Mutex<HashMap<i64, Handle>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn start(app: &AppHandle) {
    {
        // сессии, оборванные закрытием программы, не остаются «идущими»
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        let _ = db.execute("UPDATE live_sessions SET state='stopped',ended_at=unixepoch(),note='остановлено перезапуском' WHERE state IN ('starting','live')", []);
    }
    let app = app.clone();
    crate::host::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            tick(&app).await;
        }
    });
}

async fn tick(app: &AppHandle) {
    let (enabled, watch): (bool, Vec<(i64, String, String)>) = {
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        let enabled = is_enabled(&db, Flow::Live);
        let watch = trends::list(&db, "watch").unwrap_or_default().into_iter().filter(|c| c.kind == "live").map(|c| (c.id, c.url, c.title)).collect();
        (enabled, watch)
    };
    if !enabled {
        // сцена выключила поток: автосессии заканчиваем, ручные идут до остановки
        for h in registry().lock().unwrap().values().filter(|h| !h.manual) {
            let _ = h.stop.send(true);
        }
        return;
    }
    for (cand, url, title) in watch {
        if registry().lock().unwrap().len() >= MAX_SESSIONS {
            break;
        }
        if let Err(e) = begin(app, &url, &title, Some(cand), false).await {
            let state = app.state::<AppState>();
            let db = state.db.0.lock().unwrap();
            let _ = db.execute("INSERT INTO events(kind,stage,message) VALUES('attention','live',?1)", [format!("Эфир {url}: {e}")]);
            // не повторяем бесконечно: решение об эфире снимается, кандидат считается рассмотренным
            let _ = db.execute("UPDATE discovery_candidates SET state='rejected',decided_at=unixepoch() WHERE id=?1", [cand]);
        }
    }
}

/// Запускает сессию. Возвращает её номер.
pub async fn begin(app: &AppHandle, url: &str, title: &str, candidate: Option<i64>, manual: bool) -> CmdResult<i64> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "Нужна ссылка на эфир (https://…)")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Нужна ссылка на эфир (https://…)".into());
    }
    let state = app.state::<AppState>();
    let data_dir = state.data_dir.clone();
    // защита от перегруза: число эфиров, место, давление на сервере
    if registry().lock().unwrap().len() >= MAX_SESSIONS {
        return Err(format!("Уже идут {MAX_SESSIONS} эфира: больше узел не потянет"));
    }
    let info = crate::node::server::sysinfo(&data_dir);
    if info["disk_free_mb"].as_u64().is_some_and(|m| m < MIN_FREE_MB) {
        return Err("Мало места на диске для записи эфира (нужно хотя бы 3 ГБ)".into());
    }
    if crate::workflows::runtime::this_node() == NodeKind::Server && info["psi"]["cpu"].as_f64().is_some_and(|p| p > 10.0) {
        return Err("Сервер под нагрузкой: новый эфир не берём".into());
    }
    let irc = chat::twitch_channel(url);
    let platform = match parsed.host_str().unwrap_or("") {
        h if h.ends_with("twitch.tv") => "twitch",
        h if h.ends_with("kick.com") => "kick",
        h if h.contains("youtube") || h == "youtu.be" => "youtube",
        _ => "other",
    };
    let id = {
        let db = state.db.0.lock().unwrap();
        if db.query_row("SELECT EXISTS(SELECT 1 FROM live_sessions WHERE url=?1 AND state IN ('starting','live'))", [url], |r| r.get::<_, bool>(0)).unwrap_or(false) {
            return Err("За этим эфиром уже следим".into());
        }
        db.execute(
            "INSERT INTO live_sessions(platform,url,channel,title,manual,chat) VALUES(?1,?2,?3,?4,?5,?6)",
            params![platform, url, irc.clone().unwrap_or_default(), title, manual, irc.is_some()],
        )
        .map_err(|e| e.to_string())?;
        let id = db.last_insert_rowid();
        if let Some(c) = candidate {
            let _ = db.execute("UPDATE discovery_candidates SET state='accepted',decided_at=unixepoch() WHERE id=?1 AND state='watch'", [c]);
        }
        id
    };
    let (stop_tx, stop_rx) = watch::channel(false);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    registry().lock().unwrap().insert(id, Handle { stop: stop_tx, cmd: cmd_tx, manual });
    let app2 = app.clone();
    let url = url.to_string();
    crate::host::spawn(async move {
        let result = drive(&app2, id, &url, irc, data_dir, cmd_rx, stop_rx).await;
        registry().lock().unwrap().remove(&id);
        let state = app2.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        match result {
            Ok(why) => {
                let st = if why == "остановлено" { "stopped" } else { "ended" };
                let _ = db.execute("UPDATE live_sessions SET state=?2,ended_at=unixepoch(),note=?3 WHERE id=?1 AND state IN ('starting','live')", params![id, st, why]);
            }
            Err(e) => {
                let _ = db.execute("UPDATE live_sessions SET state='failed',ended_at=unixepoch(),note=?2 WHERE id=?1", params![id, e]);
            }
        }
        drop(db);
        let _ = app2.emit("live-changed", ());
    });
    let _ = app.emit("live-changed", ());
    Ok(id)
}

/// Адрес потока из yt-dlp: по возможности не выше 720p, с видео и звуком.
/// `-g` обычно печатает адрес выбранного варианта HLS. Если вернулся мастер-плейлист,
/// запись берёт первые видео и звук без дополнительного выбора разрешения.
async fn resolve(url: &str) -> Result<String, String> {
    let ytdlp = crate::youtube::engine::find_ytdlp().ok_or("yt-dlp не найден")?;
    let out = tokio::time::timeout(
        Duration::from_secs(45),
        crate::platform::command_at(&ytdlp).args(["-g", "-f", "720p60/720p/best[height<=720]/best", "--no-playlist", "--no-cache-dir", "--js-runtimes", "deno", "--js-runtimes", "node", url]).stdin(std::process::Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| "yt-dlp не ответил за 45 с")?
    .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().map(str::trim).find(|l| l.starts_with("http"));
    match (out.status.success(), first) {
        (true, Some(u)) => Ok(u.to_string()),
        _ => Err(String::from_utf8_lossy(&out.stderr).lines().rev().find(|l| l.contains("ERROR")).unwrap_or("Эфир не идёт или недоступен").trim().to_string()),
    }
}

async fn drive(app: &AppHandle, id: i64, url: &str, irc: Option<String>, data_dir: PathBuf, cmds: mpsc::UnboundedReceiver<Cmd>, stop: watch::Receiver<bool>) -> Result<String, String> {
    let input = resolve(url).await?;
    let root = data_dir.join("live");
    let mut e = Engine::new(Input::Url(input), root.join(format!("ring-{id}")), root.join("clips"), irc.map(|channel| ChatTarget { host: TWITCH_IRC.0.into(), port: TWITCH_IRC.1, channel }));
    e.params.chat = e.chat.is_some();
    if let Some(f) = crate::platform::find_tool("ffmpeg") {
        e.ffmpeg = f.to_string_lossy().into_owned();
    }
    let ring_dir = e.dir.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run = tokio::spawn(engine::run(e, tx, cmds, stop));
    let mut why = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Started => {
                let state = app.state::<AppState>();
                let _ = state.db.0.lock().unwrap().execute("UPDATE live_sessions SET state='live' WHERE id=?1", [id]);
                let _ = app.emit("live-changed", ());
            }
            Event::Restarted { reason } => {
                let state = app.state::<AppState>();
                let _ = state.db.0.lock().unwrap().execute("UPDATE live_sessions SET note=?2 WHERE id=?1", params![id, reason]);
                let _ = app.emit("live-changed", ());
            }
            Event::Signal { t, z, msgs, authors, audio_db } => {
                let _ = app.emit("live-signal", serde_json::json!({"sessionId": id, "t": t, "z": z, "msgs": msgs, "authors": authors, "audioDb": audio_db}));
            }
            Event::Moment { trigger, file, manual } => {
                let state = app.state::<AppState>();
                let db = state.db.0.lock().unwrap();
                match register_moment(&db, id, &trigger, &file, manual) {
                    Ok(_) => {}
                    Err(e) => {
                        let _ = db.execute("INSERT INTO events(kind,stage,message) VALUES('attention','live',?1)", [format!("Момент не поставлен в очередь: {e}")]);
                    }
                }
                drop(db);
                let _ = app.emit("live-changed", ());
                factory::start(app);
            }
            Event::CutFailed { trigger, error } => {
                let state = app.state::<AppState>();
                let _ = state.db.0.lock().unwrap().execute(
                    "INSERT INTO live_moments(session_id,start_t,end_t,peak_t,score,state,error) VALUES(?1,?2,?3,?4,?5,'failed',?6)",
                    params![id, trigger.start, trigger.end, trigger.peak_t, trigger.score, error],
                );
                let _ = app.emit("live-changed", ());
            }
            Event::Ended(w) => why = w,
        }
    }
    let _ = run.await;
    let _ = std::fs::remove_dir_all(ring_dir); // кольцо после эфира не нужно
    Ok(why)
}

/// Момент → строка в журнале и задача фабрики (канал-песочница, режим проверки, приоритет выше фона).
pub fn register_moment(db: &Connection, session: i64, tr: &super::detector::Trigger, file: &std::path::Path, manual: bool) -> Result<i64, String> {
    let (channel, platform): (String, String) = db.query_row("SELECT channel,platform FROM live_sessions WHERE id=?1", [session], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
    let sandbox = trends::sandbox_channel(db)?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let position: i64 = tx.query_row("SELECT COALESCE(MAX(position),0)+1 FROM factory_items", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let who = if channel.is_empty() { platform } else { channel };
    let item = factory::insert_item(
        &tx,
        &NewItem { kind: "file".into(), source: file.to_string_lossy().into_owned(), title: format!("Эфир {who}: {}момент", if manual { "ручной " } else { "" }), preset: "1080".into(), precut: false, channel_id: Some(sandbox.id), priority: 10 },
        None,
        position,
    )?;
    tx.execute(
        "INSERT INTO live_moments(session_id,start_t,end_t,peak_t,score,manual,state,file,item_id) VALUES(?1,?2,?3,?4,?5,?6,'cut',?7,?8)",
        params![session, tr.start, tr.end, tr.peak_t, tr.score, manual, file.to_string_lossy(), item],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("UPDATE live_sessions SET moments=moments+1 WHERE id=?1", [session]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(item)
}

// ------------------------------------------------------------------ команды окна

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub id: i64,
    pub platform: String,
    pub url: String,
    pub channel: String,
    pub title: String,
    pub state: String,
    pub manual: bool,
    pub chat: bool,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub note: String,
    pub moments: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentDto {
    pub id: i64,
    pub session_id: i64,
    pub score: f64,
    pub manual: bool,
    pub state: String,
    pub item_id: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub length: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    pub sessions: Vec<SessionDto>,
    pub moments: Vec<MomentDto>,
    pub flow_enabled: bool,
    pub max_sessions: usize,
    pub disk_free_mb: Option<u64>,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn live_state(state: State<'_, AppState>) -> CmdResult<LiveState> {
    let db = state.db.0.lock().unwrap();
    let e = |e: rusqlite::Error| e.to_string();
    let sessions = db
        .prepare("SELECT id,platform,url,channel,title,state,manual,chat,started_at,ended_at,note,moments FROM live_sessions ORDER BY id DESC LIMIT 30")
        .map_err(e)?
        .query_map([], |r| Ok(SessionDto { id: r.get(0)?, platform: r.get(1)?, url: r.get(2)?, channel: r.get(3)?, title: r.get(4)?, state: r.get(5)?, manual: r.get(6)?, chat: r.get(7)?, started_at: r.get(8)?, ended_at: r.get(9)?, note: r.get(10)?, moments: r.get(11)? }))
        .map_err(e)?
        .flatten()
        .collect();
    let moments = db
        .prepare("SELECT id,session_id,score,manual,state,item_id,error,created_at,end_t-start_t FROM live_moments ORDER BY id DESC LIMIT 60")
        .map_err(e)?
        .query_map([], |r| Ok(MomentDto { id: r.get(0)?, session_id: r.get(1)?, score: r.get(2)?, manual: r.get(3)?, state: r.get(4)?, item_id: r.get(5)?, error: r.get(6)?, created_at: r.get(7)?, length: r.get(8)? }))
        .map_err(e)?
        .flatten()
        .collect();
    Ok(LiveState { sessions, moments, flow_enabled: is_enabled(&db, Flow::Live), max_sessions: MAX_SESSIONS, disk_free_mb: crate::node::server::sysinfo(&state.data_dir)["disk_free_mb"].as_u64() })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn live_start(app: AppHandle, url: String) -> CmdResult<i64> {
    begin(&app, url.trim(), "", None, true).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn live_stop(id: i64) -> CmdResult<()> {
    registry().lock().unwrap().get(&id).ok_or("Эфир уже не идёт")?.stop.send(true).map_err(|e| e.to_string())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn live_cut_now(id: i64, seconds: Option<f64>) -> CmdResult<()> {
    let back = seconds.unwrap_or(45.0).clamp(10.0, 90.0);
    registry().lock().unwrap().get(&id).ok_or("Эфир уже не идёт")?.cmd.send(Cmd::CutNow { back_s: back }).map_err(|e| e.to_string())
}

/// «Игнорировать»: момент помечается, а его задача, если ещё не началась, ставится на паузу.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn live_moment_ignore(app: AppHandle, id: i64) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    let item: Option<i64> = db.query_row("SELECT item_id FROM live_moments WHERE id=?1", [id], |r| r.get(0)).map_err(|_| "Момент не найден")?;
    db.execute("UPDATE live_moments SET state='ignored' WHERE id=?1", [id]).map_err(|e| e.to_string())?;
    if let Some(i) = item {
        let _ = db.execute("UPDATE factory_items SET paused=1 WHERE id=?1 AND stage='queued'", [i]);
    }
    drop(db);
    let _ = app.emit("live-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::detector::Trigger;

    #[test]
    fn moment_becomes_queued_item_in_sandbox_with_priority() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO live_sessions(id,platform,url,channel) VALUES(1,'twitch','https://twitch.tv/x','x')", []).unwrap();
        let tr = Trigger { start: 100.0, end: 160.0, peak_t: 130.0, score: 7.5 };
        let item = register_moment(&db, 1, &tr, std::path::Path::new("/tmp/live_1.mp4"), false).unwrap();
        let (stage, kind, src, prio, ch): (String, String, String, i64, i64) = db.query_row("SELECT stage,kind,source,priority,channel_id FROM factory_items WHERE id=?1", [item], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((stage.as_str(), kind.as_str(), src.as_str(), prio), ("queued", "file", "/tmp/live_1.mp4", 10));
        assert_eq!(ch, trends::sandbox_channel(&db).unwrap().id, "без вашего одобрения не публикуется: канал в режиме проверки");
        let (n, moments): (i64, i64) = db.query_row("SELECT (SELECT COUNT(*) FROM live_moments), moments FROM live_sessions WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((n, moments), (1, 1));
    }
}
