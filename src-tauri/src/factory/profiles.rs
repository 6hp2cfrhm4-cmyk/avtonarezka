//! Профили и именованные настройки оформления. Тот же Defaults, что использует студия.
use crate::host::{AppHandle, Emitter, State};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    analysis_cmds::UiOptions,
    drafts::{self, Defaults},
    state::AppState,
};
type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    pub id: i64,
    pub name: String,
    pub layout: String,
    pub config: Defaults,
    pub is_default: bool,
}
pub(crate) fn preview_config(t: &Template) -> String {
    serde_json::json!({"layout":t.layout,"config":t.config}).to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub platform: String,
    pub account_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub source_kind: String,
    pub source: String,
    pub template_id: Option<i64>,
    pub mode: String,
    pub enabled: bool,
    pub analysis: UiOptions,
    pub score_threshold: f64,
    pub max_clips: usize,
    pub permission: String,
    pub credit: String,
    pub complaint_limit: u32,
    pub rights_paused: bool,
    pub active_incidents: u32,
    pub targets: Vec<Target>,
    pub campaign_ids: Vec<i64>,
    pub cam_rect: Option<serde_json::Value>,
    pub monitor: crate::sources::MonitorOptions,
    pub last_checked_at: Option<i64>,
    pub next_check_at: Option<i64>,
    pub source_error: Option<String>,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            source_kind: "file".into(),
            source: String::new(),
            template_id: None,
            mode: "review".into(),
            enabled: true,
            analysis: UiOptions::default(),
            score_threshold: 0.5,
            max_clips: 5,
            permission: "unspecified".into(),
            credit: String::new(),
            complaint_limit: 3,
            rights_paused: false,
            active_incidents: 0,
            targets: vec![],
            campaign_ids: vec![],
            cam_rect: None,
            monitor: Default::default(),
            last_checked_at: None,
            next_check_at: None,
            source_error: None,
        }
    }
}

pub(crate) fn templates(db: &Connection) -> Result<Vec<Template>> {
    let mut st = db
        .prepare(
            "SELECT id,name,layout,config,is_default FROM templates ORDER BY is_default DESC,id",
        )
        .map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })
        .map_err(err)?;
    rows.map(|r| {
        let (id, name, layout, config, is_default) = r.map_err(err)?;
        let mut config: Defaults = serde_json::from_str(&config).map_err(err)?;
        drafts::upgrade_defaults(&mut config);
        Ok(Template {
            preview_url: None,
            id,
            name,
            layout,
            config,
            is_default,
        })
    })
    .collect()
}

pub(crate) fn save_template(db: &Connection, mut t: Template) -> Result<Template> {
    t.name = t.name.trim().to_string();
    if t.name.is_empty() {
        return Err("Укажи название шаблона".into());
    }
    t.config.style.layout = match t.layout.as_str() {
        "gameplay_stack" => crate::render::Layout::GameplayStack,
        "reaction_pip" => crate::render::Layout::ReactionPip,
        "reaction_stack" => crate::render::Layout::ReactionStack,
        _ => return Err("Раскладка не поддерживается".into()),
    };
    drafts::upgrade_defaults(&mut t.config);
    let config = serde_json::to_string(&t.config).map_err(err)?;
    let tx = db.unchecked_transaction().map_err(err)?;
    if t.id == 0 {
        tx.execute(
            "INSERT INTO templates(name,layout,config) VALUES (?1,?2,?3)",
            params![t.name, t.layout, config],
        )
        .map_err(err)?;
        t.id = tx.last_insert_rowid();
        t.is_default = false;
    } else {
        if tx.execute("UPDATE templates SET name=?2,layout=?3,config=?4,updated_at=unixepoch() WHERE id=?1", params![t.id,t.name,t.layout,config]).map_err(err)? == 0 {
            return Err("Шаблон не найден".into());
        }
        t.is_default = tx
            .query_row(
                "SELECT is_default FROM templates WHERE id=?1",
                [t.id],
                |r| r.get(0),
            )
            .map_err(err)?;
        if t.is_default {
            drafts::kv_set(&tx, "draft_defaults", &config).map_err(err)?;
        }
    }
    tx.commit().map_err(err)?;
    Ok(t)
}

pub(crate) fn channels(db: &Connection) -> Result<Vec<Channel>> {
    let mut st = db.prepare("SELECT id,name,source_kind,source,template_id,mode,enabled,analysis,score_threshold,max_clips,permission,credit,targets,campaign_ids,cam_rect,monitor_options,last_checked_at,next_check_at,source_error,complaint_limit,rights_paused,(SELECT count(*) FROM channel_incidents i WHERE i.channel_id=channels.id AND i.resolved_at IS NULL) FROM channels ORDER BY id").map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok(Channel {
                id: r.get(0)?,
                name: r.get(1)?,
                source_kind: r.get(2)?,
                source: r.get(3)?,
                template_id: r.get(4)?,
                mode: r.get(5)?,
                enabled: r.get(6)?,
                analysis: serde_json::from_str::<UiOptions>(&r.get::<_, String>(7)?)
                    .unwrap_or_default()
                    .normalized(),
                score_threshold: r.get(8)?,
                max_clips: r.get::<_, i64>(9)? as usize,
                permission: r.get(10)?,
                credit: r.get(11)?,
                targets: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
                campaign_ids: serde_json::from_str(&r.get::<_, String>(13)?).unwrap_or_default(),
                cam_rect: r
                    .get::<_, Option<String>>(14)?
                    .and_then(|v| serde_json::from_str(&v).ok()),
                monitor: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
                last_checked_at: r.get(16)?,
                next_check_at: r.get(17)?,
                source_error: r.get(18)?,
                complaint_limit: r.get(19)?,
                rights_paused: r.get(20)?,
                active_incidents: r.get(21)?,
            })
        })
        .map_err(err)?;
    rows.collect::<std::result::Result<_, _>>().map_err(err)
}

pub(crate) fn save_channel(db: &Connection, mut c: Channel) -> Result<Channel> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let db = &*tx;
    c.name = c.name.trim().into();
    c.source = c.source.trim().into();
    c.credit = c.credit.trim().into();
    if c.name.is_empty() {
        return Err("Укажи название канала".into());
    }
    if !["auto", "review", "manual"].contains(&c.mode.as_str()) {
        return Err("Неизвестный режим канала".into());
    }
    if !["file", "youtube", "folder", "twitch", "kick"].contains(&c.source_kind.as_str()) {
        return Err("Неизвестный вид источника".into());
    }
    c.monitor.validate()?;
    if !(1..=100).contains(&c.complaint_limit) {
        return Err("Порог автопаузы: от 1 до 100 событий".into());
    }
    if c.monitor.enabled {
        crate::sources::classify(&c.source_kind, &c.source)?;
    }
    if !["owned", "agreed", "unspecified"].contains(&c.permission.as_str()) {
        return Err("Неизвестное разрешение на контент".into());
    }
    if !c.score_threshold.is_finite() || !(0.0..=1.0).contains(&c.score_threshold) {
        return Err("Порог оценки должен быть от 0 до 1".into());
    }
    if !(1..=60).contains(&c.max_clips) {
        return Err("Потолок роликов: от 1 до 60 на источник".into());
    }
    c.campaign_ids.sort();
    c.campaign_ids.dedup();
    for id in &c.campaign_ids {
        if !db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM campaigns WHERE id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(err)?
        {
            return Err(format!("Кампания #{id} не найдена"));
        }
    }
    for target in &c.targets {
        if !["tiktok", "youtube", "instagram", "vk", "telegram"].contains(&target.platform.as_str())
        {
            return Err("Неизвестная площадка".into());
        }
        if let Some(id) = target.account_id {
            if !db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND platform=?2)",
                    params![id, target.platform],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(err)?
            {
                return Err("Аккаунт этой площадки не найден".into());
            }
        }
    }
    c.analysis = c.analysis.normalized();
    if let Some(ref value) = c.cam_rect {
        serde_json::from_value::<crate::analyze::face::Rect>(value.clone())
            .map_err(|_| "Некорректная рамка камеры".to_string())?
            .validate()?;
    }
    let analysis_json = serde_json::to_string(&c.analysis).map_err(err)?;
    let targets_json = serde_json::to_string(&c.targets).map_err(err)?;
    let campaigns_json = serde_json::to_string(&c.campaign_ids).map_err(err)?;
    let cam_json = c.cam_rect.as_ref().map(|v| v.to_string());
    let monitor_json = serde_json::to_string(&c.monitor).map_err(err)?;
    let max_clips = c.max_clips as i64;
    let args = params![
        c.name,
        c.source_kind,
        c.source,
        c.template_id,
        c.mode,
        c.enabled,
        analysis_json,
        c.score_threshold,
        max_clips,
        c.permission,
        c.credit,
        targets_json,
        campaigns_json,
        cam_json,
        monitor_json,
        c.complaint_limit
    ];
    if c.id == 0 {
        db.execute("INSERT INTO channels(name,source_kind,source,template_id,mode,enabled,analysis,score_threshold,max_clips,permission,credit,targets,campaign_ids,cam_rect,monitor_options,complaint_limit) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)", args).map_err(err)?;
        c.id = db.last_insert_rowid();
    } else {
        // Именованный id в SQL берётся только из проверенного i64.
        let sql = format!("UPDATE channels SET next_check_at=CASE WHEN source_kind<>?2 OR source<>?3 OR monitor_options<>?15 OR (enabled=0 AND ?6=1) THEN NULL ELSE next_check_at END,name=?1,source_kind=?2,source=?3,template_id=?4,mode=?5,enabled=?6,analysis=?7,score_threshold=?8,max_clips=?9,permission=?10,credit=?11,targets=?12,campaign_ids=?13,cam_rect=?14,monitor_options=?15,complaint_limit=?16,updated_at=unixepoch() WHERE id={}",c.id);
        if db.execute(&sql, args).map_err(err)? == 0 {
            return Err("Канал не найден".into());
        }
    }
    super::rights::pause_if_needed(db, c.id)?;
    let saved = channels(db)?
        .into_iter()
        .find(|row| row.id == c.id)
        .ok_or("Канал не найден")?;
    tx.commit().map_err(err)?;
    Ok(saved)
}

/// Профиль фиксируется при постановке в очередь: правки канала не меняют уже начатую работу.
pub(crate) fn legacy_snapshot(db: &Connection) -> Result<String> {
    let template = Template {
        preview_url: None,
        id: 0,
        name: "По умолчанию".into(),
        layout: "gameplay_stack".into(),
        config: drafts::load_defaults(db),
        is_default: true,
    };
    let channel = Channel {
        name: "Пачка Студии".into(),
        analysis: crate::analysis_cmds::load_options(db),
        ..Default::default()
    };
    serde_json::to_string(&Snapshot { channel, template }).map_err(err)
}

pub(crate) fn snapshot(db: &Connection, channel_id: i64) -> Result<String> {
    let c = channels(db)?
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or("Канал не найден")?;
    if !c.enabled || c.rights_paused {
        return Err("Канал на паузе".into());
    }
    let template = templates(db)?
        .into_iter()
        .find(|t| Some(t.id) == c.template_id)
        .or_else(|| templates(db).ok()?.into_iter().find(|t| t.is_default))
        .ok_or("Шаблон не найден")?;
    serde_json::to_string(&Snapshot {
        channel: c,
        template,
    })
    .map_err(err)
}

/// Полное автоодобрение только для своего контента или по договорённости. Чужое без отметки
/// (`unspecified`) всегда проходит через ручное одобрение, даже если у канала режим auto.
pub(crate) fn auto_allowed(c: &Channel) -> bool {
    c.mode == "auto" && matches!(c.permission.as_str(), "owned" | "agreed")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Snapshot {
    pub channel: Channel,
    pub template: Template,
}

pub(crate) fn project_defaults(db: &Connection, project: i64) -> Option<Defaults> {
    let json: Option<String> = db.query_row("SELECT profile_json FROM factory_items WHERE project_id=?1 AND profile_json IS NOT NULL ORDER BY id DESC LIMIT 1",[project],|r|r.get(0)).optional().ok().flatten().flatten();
    serde_json::from_str::<Snapshot>(&json?)
        .ok()
        .map(|s| s.template.config)
}

pub(crate) fn project_camera(db: &Connection, project: i64) -> Option<crate::analyze::face::Rect> {
    let json:String=db.query_row("SELECT profile_json FROM factory_items WHERE project_id=?1 AND profile_json IS NOT NULL ORDER BY id DESC LIMIT 1",[project],|r|r.get(0)).ok()?;
    let s: Snapshot = serde_json::from_str(&json).ok()?;
    serde_json::from_value::<crate::analyze::face::Rect>(s.channel.cam_rect?)
        .ok()?
        .validate()
        .ok()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channels_list(state: State<'_, AppState>) -> Result<Vec<Channel>> {
    channels(&state.db.0.lock().unwrap())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channel_save(
    app: AppHandle,
    state: State<'_, AppState>,
    channel: Channel,
) -> Result<Channel> {
    let c = save_channel(&state.db.0.lock().unwrap(), channel)?;
    state.monitor.wake.notify_one();
    let _ = app.emit("factory-changed", ());
    Ok(c)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn channel_remove(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<()> {
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM channels WHERE id=?1", [id])
        .map_err(err)?;
    let _ = app.emit("factory-changed", ());
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn templates_list(state: State<'_, AppState>) -> Result<Vec<Template>> {
    let db = state.db.0.lock().unwrap();
    let mut list = templates(&db)?;
    for t in &mut list {
        if let Some(p) = drafts::kv_get(&db, &format!("layout_preview:{}", t.id)).filter(|_| {
            drafts::kv_get(&db, &format!("layout_preview_config:{}", t.id)).as_deref()
                == Some(preview_config(t).as_str())
        }) {
            let path = std::path::PathBuf::from(p);
            if path.starts_with(state.data_dir.join("layout-previews")) && path.is_file() {
                t.preview_url = Some(state.media.register(path));
            }
        }
    }
    Ok(list)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn template_save(
    app: AppHandle,
    state: State<'_, AppState>,
    template: Template,
) -> Result<Template> {
    let t = save_template(&state.db.0.lock().unwrap(), template)?;
    let _ = app.emit("factory-changed", ());
    Ok(t)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn template_remove(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<()> {
    let db = state.db.0.lock().unwrap();
    if db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM channels WHERE template_id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(err)?
    {
        return Err("Шаблон используется каналом".into());
    }
    if db
        .execute("DELETE FROM templates WHERE id=?1 AND is_default=0", [id])
        .map_err(err)?
        == 0
    {
        return Err("Шаблон по умолчанию нельзя удалить".into());
    }
    let _ = app.emit("factory-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reaction_template_persists_layout_and_camera_is_taken_from_frozen_profile() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        let mut t = templates(&c).unwrap().remove(0);
        t.id = 0;
        t.name = "Реакция A".into();
        t.layout = "reaction_pip".into();
        let saved = save_template(&c, t).unwrap();
        assert_eq!(
            saved.config.style.layout,
            crate::render::Layout::ReactionPip
        );
        assert_eq!(
            templates(&c)
                .unwrap()
                .into_iter()
                .find(|t| t.id == saved.id)
                .unwrap()
                .layout,
            "reaction_pip"
        );
        let camera = serde_json::json!({"x":0.08,"y":0.06,"width":0.14,"height":0.28});
        let channel = save_channel(
            &c,
            Channel {
                name: "Реакция".into(),
                template_id: Some(saved.id),
                cam_rect: Some(camera.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        c.execute("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio) VALUES(1,'/v','h','test',60,1920,1080,30,1)",[]).unwrap();
        c.execute("INSERT INTO factory_items(kind,source,position,project_id,profile_json) VALUES('file','/v',0,1,?1)",[snapshot(&c,channel.id).unwrap()]).unwrap();
        assert_eq!(
            serde_json::to_value(project_camera(&c, 1).unwrap()).unwrap(),
            camera
        );
        save_channel(
            &c,
            Channel {
                cam_rect: None,
                ..channel
            },
        )
        .unwrap();
        assert!(project_camera(&c, 1).is_some());
        assert_eq!(
            project_defaults(&c, 1).unwrap().style.layout,
            crate::render::Layout::ReactionPip
        );
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }
    #[test]
    fn camera_profile_round_trip_and_reject_invalid_keeps_previous() {
        let db = db();
        let rect = serde_json::json!({"x":0.1,"y":0.2,"width":0.25,"height":0.3});
        let c = save_channel(
            &db,
            Channel {
                name: "Камера".into(),
                cam_rect: Some(rect.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(channels(&db).unwrap()[0].cam_rect, Some(rect.clone()));
        assert!(save_channel(
            &db,
            Channel {
                cam_rect: Some(serde_json::json!({"x":0.9,"y":0.2,"width":0.4,"height":0.3})),
                ..c.clone()
            }
        )
        .is_err());
        assert_eq!(channels(&db).unwrap()[0].cam_rect, Some(rect));
        let c = save_channel(
            &db,
            Channel {
                cam_rect: None,
                ..c
            },
        )
        .unwrap();
        assert!(c.cam_rect.is_none());
    }
    #[test]
    fn profile_round_trip_validates_mode_cap_and_foreign_keys() {
        let db = db();
        let template = templates(&db).unwrap().remove(0);
        let c = save_channel(
            &db,
            Channel {
                name: " Стрим ".into(),
                template_id: Some(template.id),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (c.name.as_str(), c.mode.as_str(), c.permission.as_str()),
            ("Стрим", "review", "unspecified")
        );
        let c = save_channel(
            &db,
            Channel {
                mode: "auto".into(),
                max_clips: 3,
                ..c
            },
        )
        .unwrap();
        let got = channels(&db).unwrap().remove(0);
        assert_eq!(
            (got.id, got.max_clips, got.mode.as_str()),
            (c.id, 3, "auto")
        );
        assert!(save_channel(
            &db,
            Channel {
                max_clips: 0,
                ..c.clone()
            }
        )
        .is_err());
        assert!(save_channel(
            &db,
            Channel {
                template_id: Some(999),
                ..c.clone()
            }
        )
        .is_err());
        assert!(save_channel(
            &db,
            Channel {
                mode: "mystery".into(),
                ..c
            }
        )
        .is_err());
    }
    #[test]
    fn named_template_and_snapshot_drive_drafts_without_changing_studio_defaults() {
        let db = db();
        let mut t = templates(&db).unwrap().remove(0);
        t.id = 0;
        t.name = "Особый".into();
        t.config.style.split = 0.68;
        t.config.background_ids = vec![];
        let t = save_template(&db, t).unwrap();
        assert!(!t.is_default);
        let c = save_channel(
            &db,
            Channel {
                name: "Канал".into(),
                template_id: Some(t.id),
                ..Default::default()
            },
        )
        .unwrap();
        let before = drafts::load_defaults(&db).style.split;
        let profile = snapshot(&db, c.id).unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio) VALUES(1,'/v','h','Видео',30,1920,1080,30,1); INSERT INTO moments(project_id,start,\"end\",status) VALUES(1,0,20,'accepted');").unwrap();
        db.execute("INSERT INTO factory_items(position,kind,source,project_id,channel_id,profile_json) VALUES(1,'file','/v',1,?1,?2)",params![c.id,profile]).unwrap();
        // После постановки в очередь правим исходный шаблон. Черновик использует сохранённый снимок.
        let mut edited = t;
        edited.config.style.split = 0.4;
        save_template(&db, edited).unwrap();
        drafts::generate(&db, 1).unwrap();
        let config: String = db
            .query_row("SELECT style_json FROM drafts", [], |r| r.get(0))
            .unwrap();
        let cfg: crate::drafts::DraftConfig = serde_json::from_str(&config).unwrap();
        assert_eq!(cfg.style.split, 0.68);
        assert_eq!(drafts::load_defaults(&db).style.split, before);
        db.execute("DELETE FROM channels WHERE id=?1", [c.id])
            .unwrap();
        assert_eq!(
            project_defaults(&db, 1).unwrap().style.split,
            0.68,
            "снимок переживает удаление канала"
        );
    }
    #[test]
    fn default_template_updates_the_existing_studio_defaults() {
        let db = db();
        let mut t = templates(&db).unwrap().remove(0);
        t.config.style.split = 0.64;
        save_template(&db, t).unwrap();
        assert_eq!(drafts::load_defaults(&db).style.split, 0.64);
        assert_eq!(templates(&db).unwrap().len(), 1);
    }

    #[test]
    fn unspecified_permission_never_auto_approves() {
        let mk = |mode: &str, perm: &str| Channel { mode: mode.into(), permission: perm.into(), ..Default::default() };
        assert!(auto_allowed(&mk("auto", "owned")));
        assert!(auto_allowed(&mk("auto", "agreed")));
        assert!(!auto_allowed(&mk("auto", "unspecified")));
        assert!(!auto_allowed(&mk("review", "owned")));
    }
}
