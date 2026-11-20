//! Локальный временной ряд метрик. Публикация и сбор статистики независимы.
pub mod collectors;
pub mod features;
pub mod feedback;
pub mod vk;
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::state::AppState;
use collectors::{Collector, Counts};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub publication_id: i64,
    pub observed_at: i64,
    pub source: String,
    #[serde(flatten)]
    pub counts: Counts,
}
pub(crate) fn save(db: &Connection, s: &Sample, now: i64) -> Result<()> {
    s.counts.validate()?;
    if !["manual", "api"].contains(&s.source.as_str()) {
        return Err("Неизвестный источник метрики".into());
    }
    let sent: Option<i64> = db
        .query_row(
            "SELECT sent_at FROM publications WHERE id=?1 AND state='done'",
            [s.publication_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?
        .flatten();
    let sent =
        sent.ok_or("Метрики доступны только для подтверждённой публикации с датой отправки")?;
    if s.observed_at < sent || s.observed_at > now + 60 {
        return Err("Дата замера должна быть после публикации и не в будущем".into());
    }
    db.execute("INSERT INTO metrics(publication_id,observed_at,source,views,likes,comments,shares)VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(publication_id,observed_at,source)DO UPDATE SET views=excluded.views,likes=excluded.likes,comments=excluded.comments,shares=excluded.shares",params![s.publication_id,s.observed_at,s.source,s.counts.views,s.counts.likes,s.counts.comments,s.counts.shares]).map_err(err)?;
    Ok(())
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: i64,
    pub draft_id: Option<i64>,
    pub title: String,
    pub platform: String,
    pub account_id: Option<i64>,
    pub account: String,
    pub channel_id: Option<i64>,
    pub channel: String,
    pub state: String,
    pub sent_at: Option<i64>,
    pub url: Option<String>,
    pub sample: Option<Sample>,
    pub error: Option<String>,
}
pub(crate) fn rows(db: &Connection, since: i64, channel: Option<i64>) -> Result<Vec<Row>> {
    let mut q=db.prepare(r#"SELECT j.id,j.draft_id,COALESCE(NULLIF(m.title,''),p.title,'Готовое видео'),j.platform,COALESCE(j.target_account_id,j.account_id),COALESCE(ac.label,'Аккаунт удалён'),f.channel_id,COALESCE(ch.name,'Студия'),j.state,j.sent_at,j.url,mt.observed_at,mt.source,mt.views,mt.likes,mt.comments,mt.shares,COALESCE(ms.error,j.error)
      FROM publications j LEFT JOIN drafts d ON d.id=j.draft_id LEFT JOIN moments m ON m.id=d.moment_id LEFT JOIN ready_videos rv ON rv.id=j.ready_id LEFT JOIN projects p ON p.id=COALESCE(m.project_id,rv.project_id)
      LEFT JOIN factory_items f ON f.id=(SELECT ff.id FROM factory_items ff WHERE ff.project_id=p.id ORDER BY ff.id DESC LIMIT 1) LEFT JOIN channels ch ON ch.id=f.channel_id
      LEFT JOIN accounts ac ON ac.id=COALESCE(j.target_account_id,j.account_id)
      LEFT JOIN metrics mt ON mt.id=(SELECT mm.id FROM metrics mm WHERE mm.publication_id=j.id ORDER BY mm.observed_at DESC,mm.source DESC,mm.id DESC LIMIT 1)
      LEFT JOIN metric_sync ms ON ms.publication_id=j.id
      WHERE COALESCE(j.sent_at,j.updated_at)>=?1 AND (?2 IS NULL OR f.channel_id=?2) ORDER BY COALESCE(j.sent_at,j.updated_at) DESC,j.id DESC"#).map_err(err)?;
    let mapped = q
        .query_map(params![since, channel], |r| {
            let id = r.get(0)?;
            let at: Option<i64> = r.get(11)?;
            Ok(Row {
                id,
                draft_id: r.get(1)?,
                title: r.get(2)?,
                platform: r.get(3)?,
                account_id: r.get(4)?,
                account: r.get(5)?,
                channel_id: r.get(6)?,
                channel: r.get(7)?,
                state: r.get(8)?,
                sent_at: r.get(9)?,
                url: r.get(10)?,
                sample: at
                    .map(|observed_at| {
                        Ok::<_, rusqlite::Error>(Sample {
                            publication_id: id,
                            observed_at,
                            source: r.get(12)?,
                            counts: Counts {
                                views: r.get(13)?,
                                likes: r.get(14)?,
                                comments: r.get(15)?,
                                shares: r.get(16)?,
                            },
                        })
                    })
                    .transpose()?,
                error: r.get(17)?,
            })
        })
        .map_err(err)?;
    mapped.map(|r| r.map_err(err)).collect()
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hour {
    pub hour: u32,
    pub published: usize,
    pub median_views: Option<f64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub rows: Vec<Row>,
    pub published: usize,
    pub measured: usize,
    pub views: Option<i64>,
    pub median_views: Option<f64>,
    pub errors: usize,
    pub attention: usize,
    pub error_rate: Option<f64>,
    pub hours: Vec<Hour>,
    pub auto_collect: bool,
    pub checked_at: Option<i64>,
}
pub(crate) fn dashboard(db: &Connection, since: i64, channel: Option<i64>) -> Result<Dashboard> {
    use chrono::Timelike;
    let rows = rows(db, since, channel)?;
    let done: Vec<_> = rows.iter().filter(|r| r.state == "done").collect();
    let views: Vec<_> = done
        .iter()
        .filter_map(|r| r.sample.as_ref()?.counts.views)
        .collect();
    let sum = if views.is_empty() {
        None
    } else {
        Some(
            views
                .iter()
                .try_fold(0i64, |a, n| a.checked_add(*n))
                .ok_or("Переполнение суммы просмотров")?,
        )
    };
    let median_views = features::median(&views.iter().map(|n| *n as f64).collect::<Vec<_>>());
    let errors = rows
        .iter()
        .filter(|r| matches!(r.state.as_str(), "failed" | "needs_check"))
        .count();
    let attention = rows
        .iter()
        .filter(|r| r.error.is_some() || matches!(r.state.as_str(), "failed" | "needs_check"))
        .count();
    let attempted = done.len() + errors;
    let hours = (0..24)
        .map(|hour| {
            let in_hour: Vec<_> = done
                .iter()
                .filter(|r| {
                    r.sent_at
                        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                        .is_some_and(|t| t.with_timezone(&chrono::Local).hour() == hour)
                })
                .collect();
            Hour {
                hour,
                published: in_hour.len(),
                median_views: features::median(
                    &in_hour
                        .iter()
                        .filter_map(|r| r.sample.as_ref()?.counts.views.map(|n| n as f64))
                        .collect::<Vec<_>>(),
                ),
            }
        })
        .collect();
    Ok(Dashboard {
        published: done.len(),
        measured: views.len(),
        views: sum,
        median_views,
        errors,
        attention,
        error_rate: (attempted > 0).then(|| errors as f64 / attempted as f64),
        hours,
        auto_collect: crate::drafts::kv_get(db, "analytics_auto").as_deref() != Some("false"),
        checked_at: db
            .query_row("SELECT MAX(checked_at) FROM metric_sync", [], |r| r.get(0))
            .map_err(err)?,
        rows,
    })
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analytics_dashboard(
    state: State<'_, AppState>,
    days: u32,
    channel_id: Option<i64>,
) -> Result<Dashboard> {
    dashboard(
        &state.db.0.lock().unwrap(),
        chrono::Utc::now().timestamp() - i64::from(days.clamp(1, 3650)) * 86400,
        channel_id,
    )
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analytics_history(state: State<'_, AppState>, publication_id: i64) -> Result<Vec<Sample>> {
    let db = state.db.0.lock().unwrap();
    let mut q=db.prepare("SELECT observed_at,source,views,likes,comments,shares FROM metrics WHERE publication_id=?1 ORDER BY observed_at DESC,source DESC LIMIT 500").map_err(err)?;
    let rows = q
        .query_map([publication_id], |r| {
            Ok(Sample {
                publication_id,
                observed_at: r.get(0)?,
                source: r.get(1)?,
                counts: Counts {
                    views: r.get(2)?,
                    likes: r.get(3)?,
                    comments: r.get(4)?,
                    shares: r.get(5)?,
                },
            })
        })
        .map_err(err)?;
    rows.map(|r| r.map_err(err)).collect()
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analytics_manual(
    app: AppHandle,
    state: State<'_, AppState>,
    publication_id: i64,
    observed_at: Option<i64>,
    counts: Counts,
) -> Result<()> {
    save(
        &state.db.0.lock().unwrap(),
        &Sample {
            publication_id,
            observed_at: observed_at.unwrap_or_else(|| chrono::Utc::now().timestamp()),
            source: "manual".into(),
            counts,
        },
        chrono::Utc::now().timestamp(),
    )?;
    reconcile_measured(&state.db.0.lock().unwrap())?;
    let _ = app.emit("analytics-changed", ());
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analytics_auto(app: AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<()> {
    crate::drafts::kv_set(
        &state.db.0.lock().unwrap(),
        "analytics_auto",
        if enabled { "true" } else { "false" },
    )
    .map_err(err)?;
    let _ = app.emit("analytics-changed", ());
    Ok(())
}
#[derive(Default, Serialize)]
pub struct SyncReport {
    pub collected: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}
#[derive(Debug)]
struct Target {
    id: i64,
    platform: String,
    account: i64,
    external: String,
    owner: String,
}
fn due(db: &Connection, now: i64) -> Result<Vec<Target>> {
    let mut q=db.prepare("SELECT j.id,j.platform,ac.id,j.external_id,ac.external_ref FROM publications j JOIN accounts ac ON ac.id=COALESCE(j.target_account_id,j.account_id) LEFT JOIN metric_sync s ON s.publication_id=j.id WHERE j.state='done' AND j.sent_at IS NOT NULL AND j.external_id IS NOT NULL AND j.platform IN ('youtube','instagram','vk') AND ac.platform=j.platform AND ac.enabled=1 AND COALESCE(s.next_at,0)<=?1 ORDER BY COALESCE(s.next_at,0),j.id LIMIT 100").map_err(err)?;
    let rows = q
        .query_map([now], |r| {
            Ok(Target {
                id: r.get(0)?,
                platform: r.get(1)?,
                account: r.get(2)?,
                external: r.get(3)?,
                owner: r.get(4)?,
            })
        })
        .map_err(err)?;
    rows.map(|r| r.map_err(err)).collect()
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn analytics_collect(app: AppHandle) -> Result<SyncReport> {
    let state = app.state::<AppState>();
    let _guard = state
        .analytics_gate
        .try_lock()
        .map_err(|_| "Сбор метрик уже выполняется")?;
    let now = chrono::Utc::now().timestamp();
    let targets = due(&state.db.0.lock().unwrap(), now)?;
    let mut report = SyncReport::default();
    for t in targets {
        let cooldown = crate::drafts::kv_get(
            &state.db.0.lock().unwrap(),
            &format!("metrics_cooldown:{}", t.account),
        )
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
        if cooldown > now {
            report.skipped += 1;
            continue;
        }
        let result = async {
            let token = if t.platform == "youtube" {
                crate::publish::youtube::accounts::access(&state, t.account)
                    .await
                    .map_err(|_| collectors::Error::Auth)?
            } else {
                let key = match t.platform.as_str() {
                    "instagram" => crate::publish::instagram::accounts::key(t.account),
                    "vk" => vk::key(t.account),
                    _ => return Err(collectors::Error::Unavailable),
                };
                state
                    .secrets
                    .get(&key)
                    .await
                    .map_err(|_| collectors::Error::Auth)?
                    .ok_or(collectors::Error::Auth)?
            };
            #[cfg(debug_assertions)]
            let endpoint = std::env::var(match t.platform.as_str() {
                "youtube" => "AVTO_TEST_GOOGLE_ENDPOINT",
                "instagram" => "AVTO_TEST_INSTAGRAM_ENDPOINT",
                "vk" => "AVTO_TEST_VK_ENDPOINT",
                _ => return Err(collectors::Error::Unavailable),
            })
            .ok();
            #[cfg(not(debug_assertions))]
            let endpoint: Option<String> = None;
            let c = Collector::new(&t.platform, token, endpoint.as_deref())?;
            match t.platform.as_str() {
                "youtube" => c.youtube(&t.external, &t.owner).await,
                "instagram" => c.instagram(&t.external, &t.owner).await,
                "vk" => c.vk(&t.external, &t.owner).await,
                _ => Err(collectors::Error::Unavailable),
            }
        }
        .await;
        let observed = chrono::Utc::now().timestamp();
        let mut db = state.db.0.lock().unwrap();
        let tx = db.transaction().map_err(err)?;
        let (error, next) = match result {
            Ok(counts) => {
                save(
                    &tx,
                    &Sample {
                        publication_id: t.id,
                        observed_at: observed,
                        source: "api".into(),
                        counts,
                    },
                    observed,
                )?;
                report.collected += 1;
                (None, observed + 3600)
            }
            Err(e) => {
                let next = observed
                    + match e {
                        collectors::Error::Rate(n) => n as i64,
                        collectors::Error::Auth => 21600,
                        _ => 3600,
                    };
                if matches!(e, collectors::Error::Rate(_) | collectors::Error::Auth) {
                    crate::drafts::kv_set(
                        &tx,
                        &format!("metrics_cooldown:{}", t.account),
                        &next.to_string(),
                    )
                    .map_err(err)?;
                }
                let message = e.to_string();
                report
                    .errors
                    .push(format!("Публикация #{}: {}", t.id, message));
                (Some(message), next)
            }
        };
        let previous_error: Option<String> = tx
            .query_row(
                "SELECT error FROM metric_sync WHERE publication_id=?1",
                [t.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?
            .flatten();
        let newly_failed = error.is_some() && error != previous_error;
        if newly_failed {
            tx.execute("INSERT INTO events(channel_id,kind,message)VALUES((SELECT f.channel_id FROM factory_items f JOIN moments m ON m.project_id=f.project_id JOIN drafts d ON d.moment_id=m.id JOIN publications j ON j.draft_id=d.id WHERE j.id=?1 LIMIT 1),'attention',?2)",params![t.id,format!("Метрики публикации #{}: {}",t.id,error.as_deref().unwrap_or_default())]).map_err(err)?;
        }
        tx.execute("INSERT INTO metric_sync(publication_id,checked_at,next_at,error)VALUES(?1,?2,?3,?4) ON CONFLICT(publication_id)DO UPDATE SET checked_at=excluded.checked_at,next_at=excluded.next_at,error=excluded.error",params![t.id,observed,next,error]).map_err(err)?;
        tx.commit().map_err(err)?;
        drop(db);
        if newly_failed {
            crate::notifications::attention(&app,format!("metrics:{}",t.account),"Не удалось получить метрики","Проверь доступ или квоту аккаунта на экране «Аналитика». Публикации продолжают храниться в календаре.");
        }
    }
    reconcile_measured(&state.db.0.lock().unwrap())?;
    let _ = app.emit("analytics-changed", ());
    let _ = app.emit("factory-changed", ());
    Ok(report)
}
fn reconcile_measured(db: &Connection) -> Result<()> {
    let mut q=db.prepare("SELECT f.id FROM factory_items f WHERE f.stage='published' AND EXISTS(SELECT 1 FROM publications j JOIN drafts d ON d.id=j.draft_id JOIN moments m ON m.id=d.moment_id WHERE m.project_id=f.project_id AND j.state='done') AND NOT EXISTS(SELECT 1 FROM publications j JOIN drafts d ON d.id=j.draft_id JOIN moments m ON m.id=d.moment_id WHERE m.project_id=f.project_id AND (j.state!='done' OR NOT EXISTS(SELECT 1 FROM metrics mt WHERE mt.publication_id=j.id)))").map_err(err)?;
    let ids = q
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(q);
    for id in ids {
        crate::factory::set_stage(db, id, "measured", None).map_err(err)?;
    }
    Ok(())
}
pub(crate) fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        loop {
            let enabled = crate::drafts::kv_get(
                &app.state::<AppState>().db.0.lock().unwrap(),
                "analytics_auto",
            )
            .as_deref()
                != Some("false");
            if enabled {
                if let Err(e) = analytics_collect(app.clone()).await {
                    tracing::warn!("Сбор метрик: {e}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(900)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'/f','h','Видео',60,1920,1080,30,1); INSERT INTO moments(id,project_id,start,\"end\",status)VALUES(1,1,0,30,'accepted');INSERT INTO drafts(id,moment_id)VALUES(1,1);INSERT INTO publications(id,draft_id,state,sent_at)VALUES(1,1,'done',1000);").unwrap();
        c
    }
    fn sample(at: i64, views: Option<i64>) -> Sample {
        Sample {
            publication_id: 1,
            observed_at: at,
            source: "manual".into(),
            counts: Counts {
                views,
                ..Default::default()
            },
        }
    }
    #[test]
    fn series_dates_nullable_counts_idempotency_and_dashboard_do_not_sum_snapshots() {
        let c = fixture();
        assert!(save(&c, &sample(999, Some(1)), 2000).is_err());
        assert!(save(&c, &sample(2100, Some(1)), 2000).is_err());
        assert!(save(&c, &sample(1001, Some(-1)), 2000).is_err());
        assert!(save(&c, &sample(1001, None), 2000).is_err());
        save(&c, &sample(1001, Some(0)), 2000).unwrap();
        let d = dashboard(&c, 0, None).unwrap();
        assert_eq!(
            (d.published, d.measured, d.views, d.median_views),
            (1, 1, Some(0), Some(0.))
        );
        save(&c, &sample(1100, Some(15)), 2000).unwrap();
        save(&c, &sample(1100, Some(18)), 2000).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM metrics", [], |r| r.get(0))
                .unwrap(),
            2
        );
        let d = dashboard(&c, 0, None).unwrap();
        assert_eq!(d.views, Some(18));
        assert_eq!(d.rows[0].sample.as_ref().unwrap().counts.likes, None);
        assert_eq!(d.error_rate, Some(0.));
        c.execute("UPDATE publications SET state='needs_check'", [])
            .unwrap();
        assert!(save(&c, &sample(1200, Some(25)), 2000).is_err());
        c.execute("DELETE FROM publications", []).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM metrics", [], |r| r.get(0))
                .unwrap(),
            0
        );
        let d = dashboard(&c, 0, None).unwrap();
        assert_eq!(d.error_rate, None);
        assert_eq!(d.views, None);
    }
    #[test]
    fn vk_sync_only_reads_confirmed_owned_enabled_publications_when_due() {
        let c = fixture();
        c.execute("INSERT INTO accounts(id,label,platform,external_ref,enabled,position)VALUES(300,'VK test','vk','-42',1,300)", []).unwrap();
        c.execute("INSERT INTO publications(draft_id,platform,legacy,target_account_id,external_id,state,sent_at)VALUES(1,'vk',0,300,'-42_17','done',1000)", []).unwrap();
        let id = c.last_insert_rowid();
        let t = due(&c, 2000).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(
            (
                t[0].id,
                t[0].account,
                t[0].platform.as_str(),
                t[0].external.as_str(),
                t[0].owner.as_str()
            ),
            (id, 300, "vk", "-42_17", "-42")
        );
        for state in ["sending", "scheduled", "needs_check", "failed"] {
            c.execute(
                "UPDATE publications SET state=?1 WHERE id=?2",
                params![state, id],
            )
            .unwrap();
            assert!(due(&c, 2000).unwrap().is_empty());
        }
        c.execute("UPDATE publications SET state='done' WHERE id=?1", [id])
            .unwrap();
        c.execute("UPDATE accounts SET enabled=0 WHERE id=300", [])
            .unwrap();
        assert!(due(&c, 2000).unwrap().is_empty());
        c.execute(
            "UPDATE accounts SET enabled=1,platform='telegram' WHERE id=300",
            [],
        )
        .unwrap();
        assert!(due(&c, 2000).unwrap().is_empty());
        c.execute("UPDATE accounts SET platform='vk' WHERE id=300", [])
            .unwrap();
        c.execute(
            "INSERT INTO metric_sync(publication_id,checked_at,next_at)VALUES(?1,1500,2100)",
            [id],
        )
        .unwrap();
        assert!(due(&c, 2000).unwrap().is_empty());
        assert_eq!(due(&c, 2100).unwrap().len(), 1);
        assert_eq!(
            c.query_row::<String, _, _>("SELECT state FROM publications WHERE id=?1", [id], |r| r
                .get(0))
                .unwrap(),
            "done"
        );
    }
    #[test]
    fn immutable_features_are_captured_by_real_generate_and_absent_signals_are_missing() {
        let c = fixture();
        c.execute("DELETE FROM publications", []).unwrap();
        c.execute("DELETE FROM drafts", []).unwrap();
        let sig = crate::analyze::Signals {
            loudness: vec![2.; 120],
            motion: vec![0.; 120],
            ..Default::default()
        };
        c.execute("INSERT INTO analysis(project_id,signals_json,words_json,sentences_json)VALUES(1,?1,'[]','[]')",[serde_json::to_string(&sig).unwrap()]).unwrap();
        crate::drafts::generate(&c, 1).unwrap();
        let before: String = c
            .query_row("SELECT features FROM moment_features", [], |r| r.get(0))
            .unwrap();
        let f: features::Features = serde_json::from_str(&before).unwrap();
        assert_eq!(f.signals["loudness"], 2.);
        assert!(!f.signals.contains_key("chat"));
        assert_eq!(f.length, 30.);
        c.execute("UPDATE analysis SET signals_json='{}'", [])
            .unwrap();
        crate::drafts::generate(&c, 1).unwrap();
        assert_eq!(
            before,
            c.query_row::<String, _, _>("SELECT features FROM moment_features", [], |r| r.get(0))
                .unwrap()
        );
    }
    #[test]
    fn measured_needs_every_publication_and_never_changes_publication_state() {
        let c = fixture();
        c.execute("INSERT INTO factory_items(position,kind,source,project_id,stage)VALUES(1,'file','/f',1,'published')",[]).unwrap();
        reconcile_measured(&c).unwrap();
        assert_eq!(
            c.query_row::<String, _, _>("SELECT stage FROM factory_items", [], |r| r.get(0))
                .unwrap(),
            "published"
        );
        save(&c, &sample(1001, Some(0)), 2000).unwrap();
        reconcile_measured(&c).unwrap();
        assert_eq!(
            c.query_row::<String, _, _>("SELECT stage FROM factory_items", [], |r| r.get(0))
                .unwrap(),
            "measured"
        );
        assert_eq!(
            c.query_row::<String, _, _>("SELECT state FROM publications", [], |r| r.get(0))
                .unwrap(),
            "done"
        );
    }
}
