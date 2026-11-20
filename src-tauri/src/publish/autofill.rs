//! Готовые финалы занимают слоты целей профиля. Никаких сетевых отправок.
use super::{calendar, schedule::next_slot};
use crate::host::{AppHandle, Manager};
use crate::{
    factory::{
        self,
        profiles::{Snapshot, Target},
    },
    state::AppState,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub scheduled: usize,
    pub existing: usize,
    pub blocked: Vec<String>,
}
/// При равном ближайшем слоте продолжает круг с давно не использованного аккаунта.
fn choose(candidates: &[(i64, i64, i64)]) -> Option<i64> {
    candidates
        .iter()
        .min_by_key(|(id, slot, last)| (*slot, *last, *id))
        .map(|c| c.0)
}
/// Состояние источника определяется всеми финалами и целями его сохранённого профиля.
/// Частичная публикация не завершает источник. Отмены никогда не создаются заново.
pub(crate) fn reconcile(db: &Connection) -> Result<(), String> {
    let mut st=db.prepare("SELECT id,project_id,profile_json,stage FROM factory_items WHERE project_id IS NOT NULL AND stage IN ('ready_to_schedule','captioning','scheduled','publishing','needs_check')").map_err(err)?;
    let items = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(st);
    for (id, project, raw, mut current) in items {
        let Some(profile) = raw.and_then(|s| serde_json::from_str::<Snapshot>(&s).ok()) else {
            continue;
        };
        if profile.channel.targets.is_empty() {
            continue;
        }
        let mut st=db.prepare("SELECT d.id FROM drafts d JOIN moments m ON m.id=d.moment_id WHERE m.project_id=?1 AND d.status='final_done'").map_err(err)?;
        let drafts = st
            .query_map([project], |r| r.get::<_, i64>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        drop(st);
        if drafts.is_empty() {
            continue;
        }
        let mut groups: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
        for t in &profile.channel.targets {
            let ids = groups.entry(&t.platform).or_default();
            if let Some(id) = t.account_id {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        let mut states = vec![];
        for draft in drafts {
            for (platform, accounts) in &groups {
                let wanted = if accounts.is_empty() {
                    vec![None]
                } else {
                    accounts.iter().copied().map(Some).collect()
                };
                for account in wanted {
                    let state:Option<String>=db.query_row("SELECT state FROM publications WHERE legacy=0 AND draft_id=?1 AND platform=?2 AND (?3 IS NULL OR target_account_id=?3) ORDER BY CASE state WHEN 'done' THEN 0 WHEN 'needs_check' THEN 1 WHEN 'sending' THEN 2 WHEN 'scheduled' THEN 3 ELSE 4 END,id LIMIT 1",params![draft,platform,account],|r|r.get(0)).optional().map_err(err)?;
                    states.push(state);
                }
            }
        }
        let attention = if states.iter().any(|s| s.as_deref() == Some("needs_check")) {
            Some(("needs_check", "Проверь результат отправки в Календаре"))
        } else if states
            .iter()
            .any(|s| matches!(s.as_deref(), Some("failed" | "cancelled")))
        {
            Some((
                current.as_str(),
                "Есть ошибочные или отменённые публикации. Открой Календарь",
            ))
        } else if states.iter().any(Option::is_none) {
            Some((
                current.as_str(),
                "Не все цели профиля назначены. Проверь автозаполнение в Календаре",
            ))
        } else {
            None
        };
        let next = attention
            .map(|(s, _)| s)
            .unwrap_or_else(|| {
                if states.iter().all(|s| s.as_deref() == Some("done")) {
                    "published"
                } else if states
                    .iter()
                    .any(|s| matches!(s.as_deref(), Some("sending" | "done")))
                {
                    "publishing"
                } else if current == "publishing" {
                    "publishing"
                } else {
                    "scheduled"
                }
            })
            .to_string();
        let reason = attention.map(|(_, s)| s);
        if ["ready_to_schedule", "captioning"].contains(&current.as_str()) && next != current {
            factory::set_stage(db, id, "scheduled", None).map_err(err)?;
            current = "scheduled".into();
        }
        if current == "scheduled"
            && ["publishing", "published", "needs_check"].contains(&next.as_str())
        {
            factory::set_stage(db, id, "publishing", None).map_err(err)?;
        }
        factory::set_stage(db, id, &next, reason).map_err(err)?;
    }
    Ok(())
}
fn existing(
    db: &Connection,
    draft: i64,
    platform: &str,
    account: Option<i64>,
) -> Result<bool, String> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM publications WHERE draft_id=?1 AND platform=?2 AND legacy=0 AND (?3 IS NULL OR target_account_id=?3))",params![draft,platform,account],|r|r.get(0)).map_err(err)
}
fn target_accounts(
    db: &Connection,
    targets: &[Target],
    draft: i64,
    time: i64,
    report: &mut Report,
) -> Vec<i64> {
    let all = match calendar::account_list(db) {
        Ok(v) => v,
        Err(e) => {
            report.blocked.push(e);
            return vec![];
        }
    };
    let mut groups: BTreeMap<&str, Vec<Option<i64>>> = BTreeMap::new();
    for t in targets {
        groups.entry(&t.platform).or_default().push(t.account_id);
    }
    let mut selected = vec![];
    for (platform, ids) in groups {
        if !["telegram", "youtube", "instagram", "tiktok"].contains(&platform) {
            report.blocked.push(format!(
                "Ролик #{draft}: {platform} ещё не подключён к общему календарю"
            ));
            continue;
        }
        let explicit: HashSet<i64> = ids.iter().filter_map(|id| *id).collect();
        // Ротация применяется только когда не выбраны конкретные аккаунты этой площадки.
        let wanted: Vec<Option<i64>> = if explicit.is_empty() {
            vec![None]
        } else {
            let mut v: Vec<_> = explicit.into_iter().map(Some).collect();
            v.sort();
            v
        };
        for wanted in wanted {
            match existing(db, draft, platform, wanted) {
                Ok(true) => {
                    report.existing += 1;
                    continue;
                }
                Err(e) => {
                    report.blocked.push(e);
                    continue;
                }
                _ => {}
            }
            let candidates:Vec<_>=all.iter().filter(|a|a.enabled&&a.platform==platform&&wanted.is_none_or(|id|id==a.id)).filter_map(|a|{
                let reserved=calendar::reserved(db,a.id,None).ok()?;
                let slot=next_slot(&a.policy,time,calendar::offset(time),&reserved).ok()?;
                let last:i64=db.query_row("SELECT COALESCE(MAX(id),0) FROM publications WHERE COALESCE(target_account_id,account_id,wanted_account_id)=?1",[a.id],|r|r.get(0)).ok()?;
                Some((a.id,slot,last))
            }).collect();
            if let Some(id) = choose(&candidates) {
                selected.push(id);
            } else {
                report.blocked.push(format!(
                    "Ролик #{draft}: нет включённого аккаунта {platform} со свободным слотом"
                ));
            }
        }
    }
    selected
}
#[cfg(test)]
pub(crate) fn fill(db: &Connection, project: Option<i64>, time: i64) -> Result<Report, String> {
    let mut st=db.prepare("SELECT f.id,f.project_id,f.profile_json,d.id FROM factory_items f JOIN channels c ON c.id=f.channel_id JOIN moments m ON m.project_id=f.project_id JOIN drafts d ON d.moment_id=m.id WHERE c.enabled=1 AND f.paused=0 AND d.status='final_done' AND (?1 IS NULL OR f.project_id=?1) ORDER BY f.priority DESC,f.position,d.id").map_err(err)?;
    let rows = st
        .query_map([project], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(st);
    let mut report = Report::default();
    for (_item, _project, raw, draft) in rows {
        let Some(profile) = raw.and_then(|v| serde_json::from_str::<Snapshot>(&v).ok()) else {
            report
                .blocked
                .push(format!("Ролик #{draft}: снимок профиля недоступен"));
            continue;
        };
        let accounts = target_accounts(db, &profile.channel.targets, draft, time, &mut report);
        for account in accounts {
            match calendar::schedule(db, draft, account, None, time) {
                Ok(_) => report.scheduled += 1,
                Err(e) => report.blocked.push(format!("Ролик #{draft}: {e}")),
            }
        }
    }
    report.blocked.sort();
    report.blocked.dedup();
    Ok(report)
}
/// Вызывается на устойчивой границе после финалов. Ручной режим ждёт кнопки календаря.
pub(crate) async fn finish(app: &AppHandle, item: i64, project: i64) {
    factory::update(app, item, "captioning", None);
    let result = fill_with_captions(app, Some(project)).await;
    match result {
        Ok(r) if r.blocked.is_empty() && r.scheduled + r.existing > 0 => {
            factory::update(app, item, "scheduled", None)
        }
        Ok(r) => {
            let reason = r.blocked.join("; ");
            factory::update(
                app,
                item,
                "ready_to_schedule",
                (!reason.is_empty()).then_some(reason.as_str()),
            );
        }
        Err(e) => factory::update(app, item, "ready_to_schedule", Some(&e)),
    }
    calendar::changed(app);
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn publication_autofill(app: AppHandle) -> Result<Report, String> {
    let report = fill_with_captions(&app, None).await?;
    calendar::changed(&app);
    Ok(report)
}

async fn fill_with_captions(app: &AppHandle, project: Option<i64>) -> Result<Report, String> {
    let state = app.state::<AppState>();
    let mut report = Report::default();
    let rows = {
        let db = state.db.0.lock().unwrap();
        let mut st=db.prepare("SELECT d.id,f.profile_json,f.channel_id,f.id FROM factory_items f JOIN channels c ON c.id=f.channel_id JOIN moments m ON m.project_id=f.project_id JOIN drafts d ON d.moment_id=m.id WHERE c.enabled=1 AND f.paused=0 AND d.status='final_done' AND (?1 IS NULL OR f.project_id=?1) ORDER BY f.priority DESC,f.position,d.id").map_err(err)?;
        let rows = st
            .query_map([project], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for (draft, raw, channel, item) in rows {
        let Some(profile) = raw.and_then(|s| serde_json::from_str::<Snapshot>(&s).ok()) else {
            report
                .blocked
                .push(format!("Ролик #{draft}: нет снимка профиля"));
            continue;
        };
        let accounts = target_accounts(
            &state.db.0.lock().unwrap(),
            &profile.channel.targets,
            draft,
            crate::tiktok::api::now_unix(),
            &mut report,
        );
        for account in accounts {
            if project.is_some()
                && crate::drafts::kv_get(&state.db.0.lock().unwrap(), "factory_paused").as_deref()
                    == Some("1")
            {
                report
                    .blocked
                    .push("Фабрика на паузе: подписи и слоты сохранены".into());
                return Ok(report);
            }

            let result = {
                let db = state.db.0.lock().unwrap();
                let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM channels c JOIN factory_items f ON f.channel_id=c.id WHERE c.id=?1 AND f.id=?2 AND c.enabled=1 AND c.rights_paused=0 AND f.paused=0)",params![channel,item],|r|r.get(0)).map_err(err)?;
                if !active
                    || (project.is_some()
                        && crate::drafts::kv_get(&db, "factory_paused").as_deref() == Some("1"))
                {
                    Err("Канал, источник или фабрика на паузе".into())
                } else {
                    calendar::schedule(&db, draft, account, None, crate::tiktok::api::now_unix())
                }
            };
            match result {
                Ok(_) => report.scheduled += 1,
                Err(e) => report.blocked.push(format!("Ролик #{draft}: {e}")),
            }
        }
    }
    report.blocked.sort();
    report.blocked.dedup();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        db: Connection,
        path: std::path::PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn fixture() -> Fixture {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let path = std::env::temp_dir().join(format!(
            "avto-autofill-{}-{}.mp4",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(&path, b"fixture").unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'a','h','Запись',60,640,360,25,1);INSERT INTO channels(id,name)VALUES(1,'Источник');INSERT INTO moments(id,project_id,start,\"end\",transcript)VALUES(1,1,0,30,'Речь'),(2,1,30,60,'Ещё речь');INSERT INTO drafts(id,moment_id,status)VALUES(1,1,'final_done'),(2,2,'final_done');INSERT INTO factory_items(id,position,kind,source,project_id,channel_id,stage)VALUES(1,1,'file','a',1,1,'ready_to_schedule');INSERT INTO accounts(id,label,platform,external_ref,enabled,schedule_json)VALUES(2,'Первый','telegram','@first',1,'{\"pauseMin\":30,\"dailyLimit\":5,\"windowStart\":0,\"windowEnd\":1440}'),(3,'Второй','telegram','@second',1,'{\"pauseMin\":30,\"dailyLimit\":5,\"windowStart\":0,\"windowEnd\":1440}');").unwrap();
        db.execute("UPDATE drafts SET final_path=?1", [path.to_str().unwrap()])
            .unwrap();
        let mut profile: Snapshot =
            serde_json::from_str(&factory::profiles::legacy_snapshot(&db).unwrap()).unwrap();
        profile.channel.targets = vec![Target {
            platform: "telegram".into(),
            account_id: None,
        }];
        db.execute(
            "UPDATE factory_items SET profile_json=?1",
            [serde_json::to_string(&profile).unwrap()],
        )
        .unwrap();
        Fixture { db, path }
    }
    #[test]
    fn rotation_prefers_free_slot_then_oldest_account() {
        assert_eq!(choose(&[(1, 20, 0), (2, 10, 99)]), Some(2));
        assert_eq!(choose(&[(1, 10, 9), (2, 10, 1)]), Some(2));
        assert_eq!(choose(&[]), None);
    }
    #[test]
    fn tiktok_autofill_checks_caption_rotates_accounts_and_completes_factory() {
        let f = fixture();
        f.db.execute(
            "UPDATE accounts SET platform='tiktok' WHERE id IN (2,3)",
            [],
        )
        .unwrap();
        f.db.execute("UPDATE accounts SET enabled=0 WHERE id=1", [])
            .unwrap();
        let raw: String =
            f.db.query_row("SELECT profile_json FROM factory_items", [], |r| r.get(0))
                .unwrap();
        let mut profile: Snapshot = serde_json::from_str(&raw).unwrap();
        profile.channel.targets = vec![Target {
            platform: "tiktok".into(),
            account_id: None,
        }];
        f.db.execute(
            "UPDATE factory_items SET profile_json=?1",
            [serde_json::to_string(&profile).unwrap()],
        )
        .unwrap();
        let r = fill(&f.db, None, 1000).unwrap();
        assert_eq!(
            (r.scheduled, r.blocked.len()),
            (2, 0),
            "отсутствие текста не блокирует календарь"
        );
        let mut st =
            f.db.prepare("SELECT target_account_id,length(caption) FROM publications ORDER BY id")
                .unwrap();
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![(2, 0), (3, 0)]);
        assert_eq!(fill(&f.db, None, 1000).unwrap().existing, 2);
        f.db.execute("UPDATE publications SET state='done'", [])
            .unwrap();
        reconcile(&f.db).unwrap();
        assert_eq!(
            f.db.query_row::<String, _, _>("SELECT stage FROM factory_items", [], |r| r.get(0))
                .unwrap(),
            "published"
        );
    }
    #[test]
    fn two_clips_rotate_and_repeat_preserves_slots_captions_and_cancellation() {
        let f = fixture();
        let r = fill(&f.db, None, 1000).unwrap();
        assert_eq!((r.scheduled, r.existing, r.blocked.len()), (2, 0, 0));
        let accounts: Vec<i64> =
            f.db.prepare("SELECT target_account_id FROM publications ORDER BY id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
        assert_eq!(accounts, vec![2, 3]);
        f.db.execute(
            "UPDATE publications SET state='cancelled',caption='Сохранённая подпись' WHERE id=1",
            [],
        )
        .unwrap();
        let r = fill(&f.db, None, 90000).unwrap();
        assert_eq!((r.scheduled, r.existing), (0, 2));
        let row: (String, String, i64) =
            f.db.query_row(
                "SELECT state,caption,not_before FROM publications WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("cancelled".into(), "Сохранённая подпись".into(), 1000)
        );
    }
    #[test]
    fn explicit_multiple_targets_and_blocked_platform_are_reported_without_duplication() {
        let f = fixture();
        let raw: String =
            f.db.query_row("SELECT profile_json FROM factory_items", [], |r| r.get(0))
                .unwrap();
        let mut p: Snapshot = serde_json::from_str(&raw).unwrap();
        p.channel.targets = vec![
            Target {
                platform: "telegram".into(),
                account_id: Some(2),
            },
            Target {
                platform: "telegram".into(),
                account_id: Some(3),
            },
            Target {
                platform: "vk".into(),
                account_id: None,
            },
        ];
        f.db.execute(
            "UPDATE factory_items SET profile_json=?1",
            [serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
        let r = fill(&f.db, None, 1000).unwrap();
        assert_eq!((r.scheduled, r.blocked.len()), (4, 2));
        let r = fill(&f.db, None, 1000).unwrap();
        assert_eq!((r.scheduled, r.existing), (0, 4));
        f.db.execute("UPDATE channels SET enabled=0", []).unwrap();
        let r = fill(&f.db, None, 1000).unwrap();
        assert_eq!((r.scheduled, r.existing, r.blocked.len()), (0, 0, 0));
    }
    #[test]
    fn factory_tracks_all_targets_uncertainty_and_confirmed_receipts_without_repeat_events() {
        let f = fixture();
        fill(&f.db, None, 1000).unwrap();
        reconcile(&f.db).unwrap();
        let stage = || {
            f.db.query_row("SELECT stage FROM factory_items WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(stage(), "scheduled");
        f.db.execute("UPDATE publications SET state='done' WHERE id=1", [])
            .unwrap();
        reconcile(&f.db).unwrap();
        assert_eq!(stage(), "publishing");
        f.db.execute("UPDATE publications SET state='needs_check' WHERE id=2", [])
            .unwrap();
        reconcile(&f.db).unwrap();
        assert_eq!(stage(), "needs_check");
        let count: i64 =
            f.db.query_row("SELECT count(*) FROM events", [], |r| r.get(0))
                .unwrap();
        reconcile(&f.db).unwrap();
        assert_eq!(
            f.db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            count
        );
        calendar::resolve(&f.db, 2, "mark_done", 2000).unwrap();
        reconcile(&f.db).unwrap();
        assert_eq!(stage(), "published");
        assert_eq!(fill(&f.db, None, 2000).unwrap().scheduled, 0);
    }
    #[test]
    fn missing_target_prevents_factory_completion_and_paused_items_are_not_autofilled() {
        let f = fixture();
        f.db.execute("UPDATE factory_items SET paused=1", [])
            .unwrap();
        assert_eq!(fill(&f.db, None, 1000).unwrap().scheduled, 0);
        f.db.execute("UPDATE factory_items SET paused=0", [])
            .unwrap();
        fill(&f.db, None, 1000).unwrap();
        f.db.execute("UPDATE publications SET state='done'", [])
            .unwrap();
        let raw: String =
            f.db.query_row("SELECT profile_json FROM factory_items", [], |r| r.get(0))
                .unwrap();
        let mut profile: Snapshot = serde_json::from_str(&raw).unwrap();
        profile.channel.targets.push(Target {
            platform: "vk".into(),
            account_id: None,
        });
        f.db.execute(
            "UPDATE factory_items SET profile_json=?1",
            [serde_json::to_string(&profile).unwrap()],
        )
        .unwrap();
        reconcile(&f.db).unwrap();
        let row: (String, String) =
            f.db.query_row("SELECT stage,error FROM factory_items", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(row.0, "ready_to_schedule");
        assert!(row.1.contains("Не все цели"));
    }
}
