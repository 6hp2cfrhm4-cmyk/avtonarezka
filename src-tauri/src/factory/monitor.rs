//! Монитор работает в Rust пока открыто приложение; каждый опрос и enqueue идемпотентны.
use super::{
    profiles::{self, Channel},
    NewItem,
};
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::{
    sources::{self, FolderSource, Source, SourceItem, YtDlpSource},
    state::AppState,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct MonitorState {
    active: Mutex<HashMap<i64, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    pub armed: AtomicBool,
    pub wake: tokio::sync::Notify,
}
impl MonitorState {
    pub fn new(armed: bool) -> Self {
        Self {
            active: Default::default(),
            armed: AtomicBool::new(armed),
            wake: Default::default(),
        }
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PollResult {
    pub found: usize,
    pub queued: usize,
    pub skipped: usize,
    pub seen: usize,
}

fn filter_reason(c: &Channel, item: &SourceItem, regex: Option<&regex::Regex>) -> Option<String> {
    if regex.is_some_and(|re| !re.is_match(&item.title)) {
        return Some("Название не прошло фильтр".into());
    }
    if c.monitor.min_duration.is_some() || c.monitor.max_duration.is_some() {
        let Some(d) = item.duration else {
            return Some("Длительность неизвестна: проверить фильтр нельзя".into());
        };
        if c.monitor.min_duration.is_some_and(|min| d < min) {
            return Some("Короче минимальной длительности".into());
        }
        if c.monitor.max_duration.is_some_and(|max| d > max) {
            return Some("Длиннее максимальной длительности".into());
        }
    }
    None
}

/// Дедуп, сохранение пропущенных записей и вставка в ту же очередь — одна транзакция.
pub(crate) fn ingest(
    db: &Connection,
    channel: &Channel,
    items: &[SourceItem],
) -> Result<PollResult, String> {
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let current = profiles::channels(&tx)?
        .into_iter()
        .find(|c| c.id == channel.id)
        .ok_or("Канал удалён во время опроса")?;
    if !current.enabled
        || current.rights_paused
        || current.source_kind != channel.source_kind
        || current.source != channel.source
        || current.monitor != channel.monitor
    {
        return Err("Профиль источника изменён во время опроса; повтори проверку".into());
    }
    let regex = current.monitor.filter()?;
    let mut result = PollResult {
        found: items.len(),
        ..Default::default()
    };
    let mut position: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(position),0)+1 FROM factory_items",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    for item in items {
        let old: Option<(i64, String)> = tx
            .query_row(
                "SELECT id,state FROM source_items WHERE channel_id=?1 AND external_id=?2",
                params![channel.id, item.external_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if old.as_ref().is_some_and(|(_, state)| state == "queued") {
            result.seen += 1;
            continue;
        }
        let reason = filter_reason(&current, item, regex.as_ref());
        tx.execute("INSERT INTO source_items(channel_id,external_id,source,title,duration,published_at,state,filter_reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(channel_id,external_id) DO UPDATE SET source=excluded.source,title=excluded.title,duration=excluded.duration,published_at=excluded.published_at,state=excluded.state,filter_reason=excluded.filter_reason",params![channel.id,item.external_id,item.source,item.title,item.duration,item.published_at,if reason.is_some(){"skipped"}else{"discovered"},reason]).map_err(|e|e.to_string())?;
        let source_id: i64 = tx
            .query_row(
                "SELECT id FROM source_items WHERE channel_id=?1 AND external_id=?2",
                params![channel.id, item.external_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if reason.is_some() {
            result.skipped += 1;
            continue;
        }
        let it = NewItem {
            kind: if ["folder", "file"].contains(&channel.source_kind.as_str()) {
                "file"
            } else {
                "link"
            }
            .into(),
            source: item.source.clone(),
            title: item.title.clone(),
            preset: "1080".into(),
            precut: false,
            channel_id: Some(channel.id),
            priority: 0,
        };
        super::insert_item(&tx, &it, Some(source_id), position)?;
        position += 1;
        tx.execute(
            "UPDATE source_items SET state='queued',filter_reason=NULL WHERE id=?1",
            [source_id],
        )
        .map_err(|e| e.to_string())?;
        result.queued += 1;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

async fn poll(app: &AppHandle, id: i64) -> Result<PollResult, String> {
    let state = app.state::<AppState>();
    let gate = state
        .monitor
        .active
        .lock()
        .unwrap()
        .entry(id)
        .or_default()
        .clone();
    let _guard = gate.lock().await;
    let mut checked = None;
    let result = async {
        let c = profiles::channels(&state.db.0.lock().unwrap())?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or("Канал не найден")?;
        if !c.enabled || c.rights_paused {
            return Err("Канал выключен".into());
        }
        checked = Some(c.clone());
        c.monitor.validate()?;
        let source = sources::classify(&c.source_kind, &c.source)?;
        let items = if ["folder", "file"].contains(&c.source_kind.as_str()) {
            FolderSource
                .list_new(&source, c.last_checked_at, &c.monitor)
                .await?
        } else {
            // Монитор и загрузчики используют общий потолок: максимум две сетевые операции источников.
            let _slot = state
                .youtube_slots
                .acquire()
                .await
                .map_err(|e| e.to_string())?;
            YtDlpSource::detected(&c.source_kind)?
                .list_new(&source, c.last_checked_at, &c.monitor)
                .await?
        };
        ingest(&state.db.0.lock().unwrap(), &c, &items)
    }
    .await;
    {
        let db = state.db.0.lock().unwrap();
        let current = profiles::channels(&db)?.into_iter().find(|c| c.id == id);
        let unchanged = checked
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(a, b)| {
                a.source_kind == b.source_kind
                    && a.source == b.source
                    && a.monitor == b.monitor
                    && b.enabled
                    && !b.rights_paused
            });
        if unchanged {
            let time = now();
            let error = result.as_ref().err().cloned();
            let previous: Option<String> = db
                .query_row("SELECT source_error FROM channels WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .ok()
                .flatten();
            if let Err(e)=db.execute("UPDATE channels SET last_checked_at=?2,next_check_at=?2+COALESCE(json_extract(monitor_options,'$.intervalMinutes'),15)*60,source_error=?3 WHERE id=?1",params![id,time,error]){tracing::error!("Не сохранить результат мониторинга: {e}");}
            let message = match &result {
                Ok(r) => format!(
                    "Проверка источника: найдено {}, в очередь {}, по фильтру {}, уже видели {}",
                    r.found, r.queued, r.skipped, r.seen
                ),
                Err(e) => format!("Проверка источника: {e}"),
            };
            // Успешные пустые опросы не раздувают журнал; повторяющаяся ошибка видна в профиле.
            if result.as_ref().is_ok_and(|r| r.queued > 0) || error != previous {
                let _=db.execute("INSERT INTO events(channel_id,kind,message) SELECT id,?2,?3 FROM channels WHERE id=?1",params![id,if error.is_some(){"attention"}else{"source"},message]);
            }
        }
    }
    let _ = app.emit("factory-changed", ());
    let _ = app.emit("batch-changed", ());
    if result.as_ref().is_ok_and(|r| r.queued > 0) {
        super::start(app);
    }
    result
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        loop {
            let state = app.state::<AppState>();
            if state.monitor.armed.load(Ordering::SeqCst)
                && !super::paused(&state.db.0.lock().unwrap())
            {
                let channels = profiles::channels(&state.db.0.lock().unwrap()).unwrap_or_default();
                for c in channels.into_iter().filter(|c| {
                    c.enabled
                        && !c.rights_paused
                        && c.monitor.enabled
                        && c.next_check_at.is_none_or(|t| t <= now())
                }) {
                    if super::paused(&state.db.0.lock().unwrap()) {
                        break;
                    }
                    if let Err(e) = poll(&app, c.id).await {
                        tracing::warn!(channel_id = c.id, "Монитор: {e}");
                    }
                }
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(15))=>{},_=state.monitor.wake.notified()=>{}}
        }
    });
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn source_check(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Result<PollResult, String> {
    state.monitor.armed.store(true, Ordering::SeqCst);
    poll(&app, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingest_filters_deduplicates_keeps_snapshot_and_can_reconsider_skipped() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let mut c = Channel {
            name: "test".into(),
            source_kind: "youtube".into(),
            source: "youtube.com/@test".into(),
            ..Default::default()
        };
        c.monitor.min_duration = Some(60.0);
        c.monitor.max_duration = Some(600.0);
        c.monitor.title_regex = "(?i)стрим".into();
        let c = profiles::save_channel(&db, c).unwrap();
        let items = vec![
            ("one", "Стрим", 120.0),
            ("two", "Стрим short", 10.0),
            ("three", "Другое", 300.0),
        ]
        .into_iter()
        .map(|(id, title, duration)| SourceItem {
            external_id: id.into(),
            source: format!("https://youtu.be/{id}"),
            title: title.into(),
            duration: Some(duration),
            published_at: None,
        })
        .collect::<Vec<_>>();
        let r = ingest(&db, &c, &items).unwrap();
        assert_eq!((r.queued, r.skipped, r.seen), (1, 2, 0));
        let r = ingest(&db, &c, &items).unwrap();
        assert_eq!((r.queued, r.skipped, r.seen), (0, 2, 1));
        let mut c = c;
        c.monitor.title_regex = String::new();
        let c = profiles::save_channel(&db, c).unwrap();
        assert_eq!(ingest(&db, &c, &items).unwrap().queued, 1);
        assert_eq!(super::super::list(&db).unwrap().len(), 2);
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM source_items", [], |r| r.get(0))
                .unwrap(),
            3
        );
        let snap = super::super::profile(&db, 1).unwrap();
        assert_eq!(snap.channel.monitor.title_regex, "(?i)стрим");
        db.execute("DELETE FROM factory_items WHERE id=1", [])
            .unwrap();
        assert_eq!(
            ingest(&db, &c, &items).unwrap().queued,
            0,
            "удаление из очереди не забывает внешний id"
        );
    }
    #[test]
    fn stale_poll_does_not_enqueue_for_reconfigured_channel() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let old = profiles::save_channel(
            &db,
            Channel {
                name: "x".into(),
                source_kind: "folder".into(),
                source: "/old".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut next = old.clone();
        next.source = "/new".into();
        profiles::save_channel(&db, next).unwrap();
        assert!(ingest(&db, &old, &[]).is_err());
        assert!(super::super::list(&db).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_ytdlp_three_recordings_repeated_poll_and_duration_filter() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("avto-monitor-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("yt-dlp");
        std::fs::write(&script,"#!/bin/sh\ncase \" $* \" in *' --flat-playlist '*) printf '%s\\n' '{\"entries\":[{\"id\":\"abcdefghijk\",\"duration\":120},{\"id\":\"lmnopqrstuv\",\"duration\":30},{\"id\":\"01234567890\",\"duration\":240}]}' ;; *) exit 2 ;; esac\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = YtDlpSource {
            kind: "youtube".into(),
            executable: script,
        };
        let source = driver.classify("youtube.com/@test").unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let mut c = Channel {
            name: "fake list".into(),
            source_kind: "youtube".into(),
            source: source.url.clone(),
            ..Default::default()
        };
        c.monitor.min_duration = Some(60.0);
        let c = profiles::save_channel(&db, c).unwrap();
        for (queued, seen) in [(2, 0), (0, 2)] {
            let list = driver.list_new(&source, None, &c.monitor).await.unwrap();
            let result = ingest(&db, &c, &list).unwrap();
            assert_eq!(
                (result.found, result.queued, result.skipped, result.seen),
                (3, queued, 1, seen)
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
