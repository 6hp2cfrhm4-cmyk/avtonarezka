//! Права и ограничения относятся к текущему каналу, даже если оформление задания заморожено.
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::state::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub id: i64,
    pub channel_id: i64,
    pub publication_id: Option<i64>,
    pub kind: String,
    pub note: String,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewIncident {
    pub channel_id: i64,
    pub publication_id: Option<i64>,
    pub kind: String,
    pub note: String,
}

pub(crate) fn pause_if_needed(db: &Connection, channel: i64) -> Result<()> {
    let n = db.execute("UPDATE channels SET rights_paused=1,updated_at=unixepoch() WHERE id=?1 AND rights_paused=0 AND complaint_limit <= (SELECT count(*) FROM channel_incidents WHERE channel_id=?1 AND resolved_at IS NULL)",[channel]).map_err(err)?;
    if n > 0 {
        db.execute("INSERT INTO events(channel_id,kind,stage,message) VALUES(?1,'needs_action','rights_paused','Канал на паузе: достигнут порог жалоб и ограничений. Разбери события в профиле и возобнови вручную')",[channel]).map_err(err)?;
    }
    Ok(())
}
pub(crate) fn list(db: &Connection, channel: i64) -> Result<Vec<Incident>> {
    let mut st = db.prepare("SELECT id,channel_id,publication_id,kind,note,created_at,resolved_at FROM channel_incidents WHERE channel_id=?1 ORDER BY id DESC LIMIT 100").map_err(err)?;
    let rows = st
        .query_map([channel], |r| {
            Ok(Incident {
                id: r.get(0)?,
                channel_id: r.get(1)?,
                publication_id: r.get(2)?,
                kind: r.get(3)?,
                note: r.get(4)?,
                created_at: r.get(5)?,
                resolved_at: r.get(6)?,
            })
        })
        .map_err(err)?;
    rows.collect::<std::result::Result<_, _>>().map_err(err)
}
pub(crate) fn record(db: &Connection, args: NewIncident) -> Result<i64> {
    let note = args.note.trim();
    if !["complaint", "restriction"].contains(&args.kind.as_str()) {
        return Err("Выбери жалобу или ограничение".into());
    }
    if note.is_empty() || note.chars().count() > 1000 {
        return Err("Описание события: от 1 до 1000 знаков".into());
    }
    let tx = db.unchecked_transaction().map_err(err)?;
    if let Some(id) = args.publication_id {
        let belongs: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM publications p LEFT JOIN drafts d ON d.id=p.draft_id LEFT JOIN moments m ON m.id=d.moment_id LEFT JOIN ready_videos r ON r.id=p.ready_id JOIN factory_items f ON f.project_id=COALESCE(m.project_id,r.project_id) WHERE p.id=?1 AND f.channel_id=?2)",params![id,args.channel_id],|r|r.get(0)).map_err(err)?;
        if !belongs {
            return Err("Публикация не относится к этому каналу".into());
        }
    }
    tx.execute(
        "INSERT INTO channel_incidents(channel_id,publication_id,kind,note) VALUES(?1,?2,?3,?4)",
        params![args.channel_id, args.publication_id, args.kind, note],
    )
    .map_err(err)?;
    let id = tx.last_insert_rowid();
    tx.execute("INSERT INTO events(channel_id,kind,stage,message) VALUES(?1,'rights','needs_attention',?2)",params![args.channel_id,format!("{} #{id}: {note}",if args.kind=="complaint" {"Жалоба"} else {"Ограничение"})]).map_err(err)?;
    pause_if_needed(&tx, args.channel_id)?;
    tx.commit().map_err(err)?;
    Ok(id)
}
pub(crate) fn resolve(db: &Connection, id: i64) -> Result<()> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let channel: i64 = tx
        .query_row(
            "SELECT channel_id FROM channel_incidents WHERE id=?1 AND resolved_at IS NULL",
            [id],
            |r| r.get(0),
        )
        .map_err(|_| "Неразобранное событие не найдено")?;
    tx.execute(
        "UPDATE channel_incidents SET resolved_at=unixepoch() WHERE id=?1",
        [id],
    )
    .map_err(err)?;
    tx.execute(
        "INSERT INTO events(channel_id,kind,message) VALUES(?1,'rights',?2)",
        params![
            channel,
            format!("Событие #{id} разобрано. Сохранённая пауза снимается вручную")
        ],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}
pub(crate) fn resume(db: &Connection, channel: i64) -> Result<()> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let n = tx.execute("UPDATE channels SET rights_paused=0,next_check_at=NULL,updated_at=unixepoch() WHERE id=?1 AND rights_paused=1 AND complaint_limit > (SELECT count(*) FROM channel_incidents WHERE channel_id=?1 AND resolved_at IS NULL)",[channel]).map_err(err)?;
    if n != 1 {
        return Err(
            "Сначала разбери события: их число должно стать меньше порога автопаузы".into(),
        );
    }
    tx.execute("INSERT INTO events(channel_id,kind,message) VALUES(?1,'rights','Канал возобновлён вручную после разбора жалоб и ограничений')",[channel]).map_err(err)?;
    tx.commit().map_err(err)
}
pub(crate) fn project_block(db: &Connection, project: i64) -> Result<Option<String>> {
    db.query_row("SELECT CASE WHEN c.rights_paused=1 THEN 'Канал «'||c.name||'» на паузе из-за жалоб и ограничений' ELSE 'Канал «'||c.name||'» выключен' END FROM factory_items f JOIN channels c ON c.id=f.channel_id WHERE f.project_id=?1 AND (c.enabled=0 OR c.rights_paused=1) LIMIT 1",[project],|r|r.get(0)).optional().map_err(err)
}
pub(crate) fn require_project(db: &Connection, project: i64) -> Result<()> {
    if let Some(reason) = project_block(db, project)? {
        return Err(reason);
    }
    Ok(())
}
pub(crate) fn require_clip(db: &Connection, clip: i64) -> Result<()> {
    let project: i64 = if clip > 0 {
        db.query_row(
            "SELECT m.project_id FROM drafts d JOIN moments m ON m.id=d.moment_id WHERE d.id=?1",
            [clip],
            |r| r.get(0),
        )
    } else {
        db.query_row(
            "SELECT project_id FROM ready_videos WHERE id=?1",
            [clip.checked_neg().ok_or("Неверный номер ролика")?],
            |r| r.get(0),
        )
    }
    .map_err(err)?;
    require_project(db, project)
}
pub(crate) fn publication_block(db: &Connection, id: i64) -> Result<Option<String>> {
    let project: i64 = db.query_row("SELECT COALESCE(m.project_id,r.project_id) FROM publications p LEFT JOIN drafts d ON d.id=p.draft_id LEFT JOIN moments m ON m.id=d.moment_id LEFT JOIN ready_videos r ON r.id=p.ready_id WHERE p.id=?1",[id],|r|r.get(0)).map_err(err)?;
    project_block(db, project)
}
fn changed(app: &AppHandle) {
    app.state::<AppState>().monitor.wake.notify_one();
    let _ = app.emit("factory-changed", ());
    let _ = app.emit("publications-changed", ());
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channel_incidents(state: State<'_, AppState>, channel_id: i64) -> Result<Vec<Incident>> {
    list(&state.db.0.lock().unwrap(), channel_id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channel_incident_add(
    app: AppHandle,
    state: State<'_, AppState>,
    args: NewIncident,
) -> Result<i64> {
    let id = record(&state.db.0.lock().unwrap(), args)?;
    changed(&app);
    Ok(id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channel_incident_resolve(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<()> {
    resolve(&state.db.0.lock().unwrap(), id)?;
    changed(&app);
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channel_rights_resume(
    app: AppHandle,
    state: State<'_, AppState>,
    channel_id: i64,
) -> Result<()> {
    resume(&state.db.0.lock().unwrap(), channel_id)?;
    changed(&app);
    // Возобновляет прежнюю фабрику только если она уже была запущена и не стоит на глобальной паузе.
    super::start(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        factory::profiles::{self, Channel},
        publish::calendar,
        publish_queue::Action,
    };
    fn fixture() -> (Connection, Channel) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrate(&db).unwrap();
        let c = profiles::save_channel(
            &db,
            Channel {
                name: "Автор".into(),
                complaint_limit: 2,
                ..Default::default()
            },
        )
        .unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'v','h','Клип',30,640,360,25,1),(2,'w','g','Другой',30,640,360,25,1);
        INSERT INTO moments(id,project_id,start,\"end\")VALUES(1,1,0,30),(2,2,0,30);INSERT INTO drafts(id,moment_id,status,final_path)VALUES(1,1,'final_done','final-1.mp4'),(2,2,'final_done','final-2.mp4');
        INSERT INTO accounts(id,label,platform,external_ref)VALUES(2,'Канал','telegram','@channel');
        INSERT INTO publications(id,draft_id,platform,legacy,target_account_id,state,not_before)VALUES(1,1,'telegram',0,2,'scheduled',0),(2,2,'telegram',0,2,'scheduled',0);").unwrap();
        db.execute("INSERT INTO factory_items(position,kind,source,project_id,channel_id) VALUES(0,'file','v',1,?1)",[c.id]).unwrap();
        (db, c)
    }
    fn add(db: &Connection, c: i64, kind: &str, p: Option<i64>) -> i64 {
        record(
            db,
            NewIncident {
                channel_id: c,
                publication_id: p,
                kind: kind.into(),
                note: "Ограничение на ролик".into(),
            },
        )
        .unwrap()
    }
    #[test]
    fn threshold_holds_only_affected_channel_until_explicit_resume_without_losing_jobs() {
        let (db, c) = fixture();
        let first = add(&db, c.id, "complaint", Some(1));
        assert!(project_block(&db, 1).unwrap().is_none());
        assert_eq!(
            calendar::next_due(&db, 10).unwrap(),
            Action::Send { job: 1, account: 2 }
        );
        add(&db, c.id, "restriction", None);
        assert!(profiles::channels(&db).unwrap()[0].rights_paused);
        assert!(super::super::should_wait(&db, 1));
        assert!(!super::super::has_queued(&db).unwrap());
        assert!(crate::factory::monitor::ingest(&db, &c, &[]).is_err());
        assert!(profiles::snapshot(&db, c.id).is_err());
        assert!(require_clip(&db, 1).is_err());
        assert_eq!(
            calendar::next_due(&db, 10).unwrap(),
            Action::Send { job: 2, account: 2 },
            "другой канал продолжает работу"
        );
        assert!(calendar::list(&db).unwrap()[0]
            .hold_reason
            .as_deref()
            .unwrap()
            .contains("жалоб"));
        assert!(resume(&db, c.id).is_err());
        resolve(&db, first).unwrap();
        assert!(
            profiles::channels(&db).unwrap()[0].rights_paused,
            "разбор не снимает паузу"
        );
        resume(&db, c.id).unwrap();
        assert_eq!(
            calendar::next_due(&db, 10).unwrap(),
            Action::Send { job: 1, account: 2 }
        );
        assert_eq!(
            db.query_row::<i64, _, _>(
                "SELECT count(*) FROM publications WHERE state='scheduled'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(list(&db, c.id).unwrap().len(), 2);
        assert!(resolve(&db, first).is_err());
    }
    #[test]
    fn changing_threshold_is_atomic_and_client_cannot_reset_pause_or_counts() {
        let (db, c) = fixture();
        add(&db, c.id, "complaint", None);
        let saved = profiles::save_channel(
            &db,
            Channel {
                complaint_limit: 1,
                rights_paused: false,
                active_incidents: 0,
                ..c
            },
        )
        .unwrap();
        assert!(saved.rights_paused);
        assert_eq!(saved.active_incidents, 1);
        let saved = profiles::save_channel(
            &db,
            Channel {
                rights_paused: false,
                active_incidents: 0,
                ..saved
            },
        )
        .unwrap();
        assert!(saved.rights_paused);
        assert_eq!(
            db.query_row::<i64, _, _>(
                "SELECT count(*) FROM events WHERE stage='rights_paused'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            1
        );
        assert!(profiles::save_channel(
            &db,
            Channel {
                complaint_limit: 0,
                ..saved
            }
        )
        .is_err());
        assert_eq!(profiles::channels(&db).unwrap()[0].complaint_limit, 1);
    }
    #[test]
    fn invalid_or_foreign_publication_never_changes_incidents_and_disabled_channel_holds() {
        let (db, c) = fixture();
        for (kind, note, pub_id) in [
            ("unknown", "Описание", None),
            ("complaint", " ", None),
            ("complaint", "Чужой ролик", Some(2)),
        ] {
            assert!(record(
                &db,
                NewIncident {
                    channel_id: c.id,
                    kind: kind.into(),
                    note: note.into(),
                    publication_id: pub_id
                }
            )
            .is_err());
        }
        assert!(list(&db, c.id).unwrap().is_empty());
        db.execute("UPDATE channels SET enabled=0 WHERE id=?1", [c.id])
            .unwrap();
        assert!(publication_block(&db, 1)
            .unwrap()
            .unwrap()
            .contains("выключен"));
        assert_eq!(
            calendar::next_due(&db, 10).unwrap(),
            Action::Send { job: 2, account: 2 }
        );
        db.execute(
            "INSERT INTO ready_videos(id,project_id,path,name)VALUES(1,1,'r','Готовый')",
            [],
        )
        .unwrap();
        assert!(require_clip(&db, -1).is_err());
        assert!(!db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
}
