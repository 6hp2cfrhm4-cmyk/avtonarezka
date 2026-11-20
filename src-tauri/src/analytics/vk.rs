//! Пользовательское подключение только чтения и привязка уже опубликованного видео.
//! video.get подтверждает доступ к счётчикам, но не владельца токена/право загрузки.
use super::{collectors::Collector, err, Sample};
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::state::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub(crate) fn key(id: i64) -> String {
    format!("platform:vk:account:{id}:user_token")
}
fn connector(token: String) -> Result<Collector, String> {
    #[cfg(debug_assertions)]
    let endpoint = std::env::var("AVTO_TEST_VK_ENDPOINT").ok();
    #[cfg(not(debug_assertions))]
    let endpoint: Option<String> = None;
    Collector::new("vk", token, endpoint.as_deref()).map_err(|e| e.to_string())
}

#[derive(Debug, PartialEq)]
struct Video {
    id: String,
    owner: String,
}
impl Video {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let path;
        let id = if raw.starts_with("https://") {
            let u = reqwest::Url::parse(raw).map_err(|_| "Некорректная ссылка VK")?;
            if !matches!(u.host_str(), Some("vk.com" | "m.vk.com" | "vkvideo.ru"))
                || !u.username().is_empty()
                || u.password().is_some()
                || u.port().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err(
                    "Нужна прямая публичная ссылка VK без параметров и ключей доступа".into(),
                );
            }
            path = u.path().to_string();
            path.strip_prefix("/clip")
                .or_else(|| path.strip_prefix("/video"))
                .ok_or("Нужна ссылка VK вида https://vk.com/clip-123_456 или ID -123_456")?
        } else {
            raw
        };
        let (owner, video) = id
            .split_once('_')
            .ok_or("Укажи ID VK в формате -123_456 или прямую ссылку на ролик")?;
        let number = |s: &str| {
            s.parse::<i64>()
                .ok()
                .filter(|n| *n != 0 && n.to_string() == s)
        };
        if number(owner).is_none() || number(video).is_none_or(|n| n < 0) {
            return Err("ID VK: ненулевой владелец и положительный ID видео; для сообщества владелец отрицательный".into());
        }
        Ok(Self {
            id: id.into(),
            owner: owner.into(),
        })
    }
    fn url(&self) -> String {
        format!("https://vk.com/video{}", self.id)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    id: i64,
    label: String,
    owner: String,
    enabled: bool,
    has_secret: bool,
    probe_video: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    draft_id: Option<i64>,
    ready_id: Option<i64>,
    title: String,
    project: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    accounts: Vec<Profile>,
    clips: Vec<Clip>,
    secret_backend: String,
}
fn profiles(db: &Connection) -> Result<Vec<Profile>, String> {
    let mut q = db.prepare("SELECT id,label,external_ref,enabled FROM accounts WHERE platform='vk' ORDER BY position,id").map_err(err)?;
    let rows = q
        .query_map([], |r| {
            let id = r.get(0)?;
            Ok(Profile {
                id,
                label: r.get(1)?,
                owner: r.get(2)?,
                enabled: r.get(3)?,
                has_secret: false,
                probe_video: crate::drafts::kv_get(db, &format!("vk_probe:{id}"))
                    .unwrap_or_default(),
            })
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<_>>().map_err(err)
}
fn clips(db: &Connection) -> Result<Vec<Clip>, String> {
    let mut q = db.prepare("SELECT d.id,NULL,COALESCE(NULLIF(m.title,''),p.title),p.title FROM drafts d JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id WHERE d.status='final_done' AND NULLIF(trim(d.final_path),'') IS NOT NULL UNION ALL SELECT NULL,r.id,r.name,p.title FROM ready_videos r JOIN projects p ON p.id=r.project_id ORDER BY 4,3,1,2").map_err(err)?;
    let rows = q
        .query_map([], |r| {
            Ok(Clip {
                draft_id: r.get(0)?,
                ready_id: r.get(1)?,
                title: r.get(2)?,
                project: r.get(3)?,
            })
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<_>>().map_err(err)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn vk_metrics_options(state: State<'_, AppState>) -> Result<Options, String> {
    let (mut accounts, clips) = {
        let db = state.db.0.lock().unwrap();
        (profiles(&db)?, clips(&db)?)
    };
    for a in &mut accounts {
        a.has_secret = state
            .secrets
            .get(&key(a.id))
            .await
            .map_err(|_| "Не удалось прочитать токен VK в связке ключей")?
            .is_some();
    }
    Ok(Options {
        accounts,
        clips,
        secret_backend: state.secrets.backend().into(),
    })
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn vk_metrics_account_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    enabled: bool,
) -> Result<(), String> {
    let _guard = state
        .analytics_gate
        .try_lock()
        .map_err(|_| "Дождись завершения текущего сбора или подключения метрик")?;
    let n = state
        .db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET enabled=?2 WHERE id=?1 AND platform='vk'",
            params![id, enabled],
        )
        .map_err(err)?;
    if n != 1 {
        return Err("Профиль статистики VK не найден".into());
    }
    changed(&app);
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountArgs {
    pub id: Option<i64>,
    pub label: String,
    pub video: String,
    pub token: Option<String>,
    pub enabled: bool,
}
fn validate_account(db: &Connection, args: &AccountArgs, v: &Video) -> Result<(), String> {
    if args.label.trim().is_empty() || args.label.trim().chars().count() > 60 {
        return Err("Название профиля: от 1 до 60 знаков".into());
    }
    if let Some(id) = args.id {
        if account_owner(db, id)? != v.owner {
            return Err("Для другого владельца VK создай новый профиль статистики".into());
        }
    }
    let duplicate: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE platform='vk' AND external_ref=?1 AND id!=COALESCE(?2,-1))", params![v.owner,args.id], |r| r.get(0)).map_err(err)?;
    if duplicate {
        return Err("Этот владелец VK уже подключён".into());
    }
    Ok(())
}
fn account_owner(db: &Connection, id: i64) -> Result<String, String> {
    db.query_row(
        "SELECT external_ref FROM accounts WHERE id=?1 AND platform='vk'",
        [id],
        |r| r.get(0),
    )
    .map_err(|_| "Профиль статистики VK не найден".into())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn vk_metrics_account_save(app: AppHandle, args: AccountArgs) -> Result<i64, String> {
    let state = app.state::<AppState>();
    let _guard = state
        .analytics_gate
        .try_lock()
        .map_err(|_| "Сбор или подключение метрик уже выполняется")?;
    let v = Video::parse(&args.video)?;
    validate_account(&state.db.0.lock().unwrap(), &args, &v)?;
    let token = match args.token.as_ref().filter(|t| !t.is_empty()) {
        Some(t) => t.clone(),
        None => state
            .secrets
            .get(&key(args.id.ok_or("Нужен пользовательский токен VK")?))
            .await
            .map_err(|_| "Не удалось прочитать токен VK")?
            .ok_or("Введи пользовательский токен VK заново")?,
    };
    connector(token.clone())?
        .vk(&v.id, &v.owner)
        .await
        .map_err(|e| e.to_string())?;
    let id = {
        let db = state.db.0.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(err)?;
        validate_account(&tx, &args, &v)?;
        let id = if let Some(id) = args.id {
            id
        } else {
            tx.execute("INSERT INTO accounts(label,platform,external_ref,enabled,position)VALUES(?1,'vk',?2,0,(SELECT COALESCE(MAX(position),0)+1 FROM accounts))",params![args.label.trim(),v.owner]).map_err(err)?;
            tx.last_insert_rowid()
        };
        // Не оставлять сбор включённым при отказе сохранения нового секрета.
        tx.execute("UPDATE accounts SET enabled=0 WHERE id=?1", [id])
            .map_err(err)?;
        tx.commit().map_err(err)?;
        id
    };
    state.secrets.set(&key(id), &token).await.map_err(|_| {
        "Токен VK не сохранён в связке ключей. Профиль оставлен на паузе; повтори подключение"
    })?;
    {
        let db = state.db.0.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(err)?;
        if account_owner(&tx, id)? != v.owner {
            return Err("Профиль VK изменился; повтори подключение".into());
        }
        tx.execute(
            "UPDATE accounts SET label=?2,enabled=?3 WHERE id=?1",
            params![id, args.label.trim(), args.enabled],
        )
        .map_err(err)?;
        crate::drafts::kv_set(&tx, &format!("vk_probe:{id}"), &v.id).map_err(err)?;
        tx.execute(
            "DELETE FROM kv WHERE key=?1",
            [format!("metrics_cooldown:{id}")],
        )
        .map_err(err)?;
        tx.execute("UPDATE metric_sync SET next_at=0,error=NULL WHERE publication_id IN (SELECT id FROM publications WHERE platform='vk' AND target_account_id=?1)",[id]).map_err(err)?;
        tx.commit().map_err(err)?;
    }
    changed(&app);
    Ok(id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkArgs {
    pub account_id: i64,
    pub draft_id: Option<i64>,
    pub ready_id: Option<i64>,
    pub video: String,
    pub sent_at: i64,
}
// Called both before the network and in the committing transaction. Existing history
// is immutable here, including uncertain/scheduled/cancelled rows.
fn preflight(
    db: &Connection,
    args: &LinkArgs,
    video: &Video,
    now: i64,
) -> Result<Option<i64>, String> {
    if args.sent_at <= 0 || args.sent_at > now {
        return Err("Укажи фактическую дату публикации, не в будущем".into());
    }
    if args.draft_id.is_some() == args.ready_id.is_some() {
        return Err("Выбери один локальный финал".into());
    }
    if account_owner(db, args.account_id)? != video.owner {
        return Err("Владелец ролика не совпадает с профилем статистики VK".into());
    }
    let existing: Option<(i64,String,Option<String>,Option<i64>)> = db.query_row("SELECT id,state,external_id,sent_at FROM publications WHERE legacy=0 AND platform='vk' AND target_account_id=?1 AND (draft_id=?2 OR ready_id=?3)",params![args.account_id,args.draft_id,args.ready_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(err)?;
    if let Some((id, state, external, sent)) = existing {
        if state == "done" && external.as_deref() == Some(&video.id) && sent == Some(args.sent_at) {
            return Ok(Some(id));
        }
        return Err(
            "Для этого финала уже есть запись VK. Её состояние и историю нельзя заменить привязкой"
                .into(),
        );
    }
    let duplicate: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM publications WHERE platform='vk' AND target_account_id=?1 AND external_id=?2)",params![args.account_id,video.id],|r|r.get(0)).map_err(err)?;
    if duplicate {
        return Err("Этот ролик VK уже связан с другим локальным финалом".into());
    }
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM drafts WHERE id=?1 AND status='final_done' AND NULLIF(trim(final_path),'') IS NOT NULL) OR EXISTS(SELECT 1 FROM ready_videos WHERE id=?2)",params![args.draft_id,args.ready_id],|r|r.get(0)).map_err(err)?;
    if !exists {
        return Err("Локальный финал не найден или ещё не готов".into());
    }
    Ok(None)
}
fn insert_link(
    db: &Connection,
    args: &LinkArgs,
    video: &Video,
    counts: super::collectors::Counts,
    observed: i64,
) -> Result<i64, String> {
    let tx = db.unchecked_transaction().map_err(err)?;
    if let Some(id) = preflight(&tx, args, video, observed)? {
        return Ok(id);
    }
    tx.execute("INSERT INTO publications(draft_id,ready_id,platform,legacy,target_account_id,state,sent_at,external_id,url,not_before)VALUES(?1,?2,'vk',0,?3,'done',?4,?5,?6,?4)",params![args.draft_id,args.ready_id,args.account_id,args.sent_at,video.id,video.url()]).map_err(err)?;
    let id = tx.last_insert_rowid();
    super::save(
        &tx,
        &Sample {
            publication_id: id,
            observed_at: observed,
            source: "api".into(),
            counts,
        },
        observed,
    )?;
    tx.execute(
        "INSERT INTO metric_sync(publication_id,checked_at,next_at)VALUES(?1,?2,?3)",
        params![id, observed, observed + 3600],
    )
    .map_err(err)?;
    tx.execute("INSERT INTO events(kind,stage,message)VALUES('publication','measured',?1)",[format!("Публикация #{id}: уже опубликованный ролик VK связан с локальным финалом; статистика проверена")]).map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn vk_metrics_link(app: AppHandle, args: LinkArgs) -> Result<i64, String> {
    let state = app.state::<AppState>();
    let _guard = state
        .analytics_gate
        .try_lock()
        .map_err(|_| "Сбор или подключение метрик уже выполняется")?;
    let video = Video::parse(&args.video)?;
    if let Some(id) = preflight(
        &state.db.0.lock().unwrap(),
        &args,
        &video,
        chrono::Utc::now().timestamp(),
    )? {
        return Ok(id);
    }
    let token = state
        .secrets
        .get(&key(args.account_id))
        .await
        .map_err(|_| "Не удалось прочитать токен VK")?
        .ok_or("Подключи токен VK в разделе «Аккаунты»")?;
    let counts = connector(token)?
        .vk(&video.id, &video.owner)
        .await
        .map_err(|e| e.to_string())?;
    let id = insert_link(
        &state.db.0.lock().unwrap(),
        &args,
        &video,
        counts,
        chrono::Utc::now().timestamp(),
    )?;
    changed(&app);
    Ok(id)
}
fn changed(app: &AppHandle) {
    crate::publish::calendar::changed(app);
    let _ = app.emit("analytics-changed", ());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'/final','h','Проект',60,1920,1080,30,1); INSERT INTO moments(id,project_id,start,\"end\",status)VALUES(1,1,0,30,'accepted'),(2,1,30,60,'accepted'); INSERT INTO drafts(id,moment_id,status,final_path)VALUES(1,1,'final_done','/final1.mp4'),(2,2,'final_done','/final2.mp4'); INSERT INTO ready_videos(id,project_id,path,name)VALUES(1,1,'/ready.mp4','Импорт'); INSERT INTO accounts(id,label,platform,external_ref,enabled)VALUES(300,'VK','vk','-42',1),(301,'Другой','vk','51',0),(302,'Telegram','telegram','-42',1);").unwrap();
        db
    }
    fn args() -> LinkArgs {
        LinkArgs {
            account_id: 300,
            draft_id: Some(1),
            ready_id: None,
            video: "-42_17".into(),
            sent_at: 1000,
        }
    }
    fn counts() -> super::super::collectors::Counts {
        super::super::collectors::Counts {
            views: Some(157),
            likes: Some(0),
            comments: None,
            shares: Some(4),
        }
    }
    #[test]
    fn public_vk_ids_and_urls_are_strict_and_canonical() {
        for raw in [
            "-42_17",
            "https://vk.com/clip-42_17",
            "https://m.vk.com/video-42_17",
            "https://vkvideo.ru/video-42_17",
            "  -42_17  ",
        ] {
            assert_eq!(
                Video::parse(raw).unwrap(),
                Video {
                    id: "-42_17".into(),
                    owner: "-42".into()
                }
            );
        }
        assert_eq!(Video::parse("42_17").unwrap().owner, "42");
        for raw in [
            "",
            "-0_1",
            "0_1",
            "-42_0",
            "-42_-1",
            "+42_1",
            "042_1",
            "-42_01",
            "-42_17_secret",
            "https://vk.com.evil/clip-42_17",
            "http://vk.com/clip-42_17",
            "https://vk.com/clip-42_17?access_key=secret",
            "https://vk.com/clip-42_17#secret",
            "https://a@vk.com/video-42_17",
            "https://vk.com:8443/video-42_17",
            "https://vk.com/videos-42",
            "https://vk.com/video-42_17/",
            "https://vk.com/video%2D42_17",
        ] {
            assert!(Video::parse(raw).is_err(), "{raw}");
        }
    }
    #[test]
    fn vk_profile_owner_and_duplicates_are_checked_without_changing_other_accounts() {
        let db = fixture();
        let v = Video::parse("-42_17").unwrap();
        let mut a = AccountArgs {
            id: None,
            label: "VK".into(),
            video: v.id.clone(),
            token: None,
            enabled: true,
        };
        assert!(validate_account(&db, &a, &v).is_err());
        a.id = Some(300);
        assert!(validate_account(&db, &a, &v).is_ok());
        a.id = Some(301);
        assert!(validate_account(&db, &a, &v).is_err());
        a.id = Some(302);
        assert!(validate_account(&db, &a, &v).is_err());
        a.id = Some(300);
        a.label = " ".into();
        assert!(validate_account(&db, &a, &v).is_err());
        a.label = "Ж".repeat(61);
        assert!(validate_account(&db, &a, &v).is_err());
        assert_eq!(profiles(&db).unwrap().len(), 2);
        assert_eq!(clips(&db).unwrap().len(), 3);
        db.execute("UPDATE drafts SET status='ready' WHERE id=2", [])
            .unwrap();
        assert_eq!(clips(&db).unwrap().len(), 2);
    }
    #[test]
    fn vk_link_is_atomic_idempotent_preserves_null_zero_and_has_no_upload_attempt() {
        let db = fixture();
        let a = args();
        let v = Video::parse(&a.video).unwrap();
        let id = insert_link(&db, &a, &v, counts(), 2000).unwrap();
        assert_eq!(insert_link(&db, &a, &v, counts(), 3000).unwrap(), id);
        assert_eq!(
            db.query_row::<(i64, i64, Option<i64>, i64), _, _>(
                "SELECT views,likes,comments,shares FROM metrics",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            )
            .unwrap(),
            (157, 0, None, 4)
        );
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT next_at FROM metric_sync", [], |r| r.get(0))
                .unwrap(),
            5600
        );
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM publications", [], |r| r.get(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM publication_attempts", [], |r| r
                .get(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT attempts FROM publications", [], |r| r.get(0))
                .unwrap(),
            0
        );
        let mut b = args();
        b.draft_id = None;
        b.ready_id = Some(1);
        let other = Video::parse("-42_18").unwrap();
        assert!(insert_link(&db, &b, &other, Default::default(), 2000).is_err());
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM publications", [], |r| r.get(0))
                .unwrap(),
            1
        );
        insert_link(&db, &b, &other, counts(), 2000).unwrap();
        assert_eq!(super::super::dashboard(&db, 0, None).unwrap().published, 2);
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn vk_link_refuses_conflicting_history_owner_clip_dates_and_unsafe_states() {
        let db = fixture();
        let mut a = args();
        let v = Video::parse(&a.video).unwrap();
        assert!(preflight(&db, &a, &v, 999).is_err());
        a.sent_at = 0;
        assert!(preflight(&db, &a, &v, 2000).is_err());
        a.sent_at = 1000;
        a.account_id = 301;
        assert!(preflight(&db, &a, &v, 2000).is_err());
        a.account_id = 302;
        assert!(preflight(&db, &a, &v, 2000).is_err());
        a.account_id = 300;
        a.ready_id = Some(1);
        assert!(preflight(&db, &a, &v, 2000).is_err());
        a.ready_id = None;
        db.execute("UPDATE drafts SET status='ready' WHERE id=1", [])
            .unwrap();
        assert!(preflight(&db, &a, &v, 2000).is_err());
        db.execute(
            "UPDATE drafts SET status='final_done',final_path='' WHERE id=1",
            [],
        )
        .unwrap();
        assert!(preflight(&db, &a, &v, 2000).is_err());
        db.execute("UPDATE drafts SET final_path='/final.mp4' WHERE id=1", [])
            .unwrap();
        let id = insert_link(&db, &a, &v, counts(), 2000).unwrap();
        a.draft_id = Some(2);
        assert!(preflight(&db, &a, &v, 2000).is_err());
        a.draft_id = Some(1);
        a.sent_at = 1001;
        assert!(preflight(&db, &a, &v, 2000).is_err());
        a.sent_at = 1000;
        assert!(preflight(&db, &a, &Video::parse("-42_18").unwrap(), 2000).is_err());
        for status in ["scheduled", "sending", "needs_check", "failed", "cancelled"] {
            db.execute(
                "UPDATE publications SET state=?1 WHERE id=?2",
                params![status, id],
            )
            .unwrap();
            assert!(preflight(&db, &a, &v, 2000).is_err(), "{status}");
            assert!(insert_link(&db, &a, &v, counts(), 3000).is_err());
            assert_eq!(
                db.query_row::<String, _, _>(
                    "SELECT state FROM publications WHERE id=?1",
                    [id],
                    |r| r.get(0)
                )
                .unwrap(),
                status
            );
        }
    }
}
