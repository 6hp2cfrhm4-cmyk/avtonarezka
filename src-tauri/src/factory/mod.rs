pub mod machine;
pub(crate) mod media;
pub mod monitor;
pub mod profiles;
pub mod qc;
pub mod resources;
pub mod rights;

// Пачка: ссылки и файлы по очереди. Скачать (ссылка) → добавить в проекты → нарезать (как одно видео)
// → черновики → ждём одобрения человека → финал. Уже нарезанный файл (`precut`) идёт без поиска моментов.
// Ошибка одного элемента не останавливает пачку. Одобрение дальше подхватывает раннер сам.

use std::{collections::HashMap, time::Duration};

use crate::host::{AppHandle, Emitter, Manager, State};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::{sync::watch, task::JoinHandle};

use crate::{
    analysis_cmds,
    analyze::pipeline::Options,
    commands::{self, Project},
    drafts,
    state::AppState,
    youtube::select,
    youtube_cmds::{self, StartArgs},
};

type CmdResult<T> = Result<T, String>;

const QUEUED: &str = "queued";
const DOWNLOADING: &str = "fetching";
const IMPORTING: &str = "fetching";
const ANALYZING: &str = "scouting";
const DRAFTING: &str = "drafting";
const AWAITING: &str = "awaiting_approval";
const RENDERING: &str = "rendering_final";
const DONE: &str = "completed";
const FAILED: &str = "failed";

/// Состояние пачки в памяти: работает ли раннер и что сейчас скачивается или анализируется.
#[derive(Default)]
pub struct BatchState {
    /// `Some` — раннер работает. Сигнал остановки.
    stop: std::sync::Mutex<Option<watch::Sender<bool>>>,
    /// Элемент пачки → id задачи загрузки (их может быть две: текущая и опережающая).
    jobs: std::sync::Mutex<HashMap<i64, String>>,
    /// Проект, который сейчас анализируется в пачке.
    analysis: std::sync::Mutex<Option<i64>>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub id: i64,
    pub kind: String,
    pub source: String,
    pub title: String,
    pub precut: bool,
    pub stage: String,
    /// Для «готово» здесь может стоять заметка (сколько черновиков не собрались), для «ошибки» — причина.
    pub error: Option<String>,
    pub project_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub priority: i64,
    pub position: i64,
    pub paused: bool,
    /// `local` — обрабатывается на этом узле; иначе имя узла, куда задача передана.
    pub owner_node: String,
}

/// Элемент, который добавляет окно.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewItem {
    /// `link` — ссылка YouTube, `file` — путь к видео.
    pub kind: String,
    pub source: String,
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_preset")]
    pub preset: String,
    #[serde(default)]
    pub precut: bool,
    #[serde(default)]
    pub channel_id: Option<i64>,
    #[serde(default)]
    pub priority: i64,
}

fn default_preset() -> String {
    "1080".into()
}

// ---------------------------------------------------------------- база

pub(crate) fn add(db: &Connection, items: &[NewItem]) -> CmdResult<usize> {
    let db = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let next: i64 = db
        .query_row(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM factory_items",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    for (i, it) in items.iter().enumerate() {
        insert_item(&db, it, None, next + i as i64)?;
    }
    db.commit().map_err(|e| e.to_string())?;
    Ok(items.len())
}

/// Вызывается внутри транзакции add или монитора: новая запись и очередь фиксируются вместе.
pub(crate) fn insert_item(
    db: &Connection,
    it: &NewItem,
    source_item: Option<i64>,
    position: i64,
) -> CmdResult<i64> {
    let snapshot = match it.channel_id {
        Some(id) => profiles::snapshot(db, id)?,
        None => profiles::legacy_snapshot(db)?,
    };
    db.execute("INSERT INTO factory_items(position,kind,source,title,preset,precut,stage,channel_id,priority,profile_json,source_item_id) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,?8,?9,?10)",params![position,it.kind,it.source.trim(),it.title.trim(),it.preset,it.precut,it.channel_id,it.priority,snapshot,source_item]).map_err(|e|e.to_string())?;
    let id = db.last_insert_rowid();
    db.execute("INSERT INTO events(item_id,channel_id,kind,stage,message) VALUES(?1,?2,'stage','queued','Источник добавлен в очередь')",params![id,it.channel_id]).map_err(|e|e.to_string())?;
    Ok(id)
}

pub(crate) fn list(db: &Connection) -> rusqlite::Result<Vec<ItemDto>> {
    let mut st = db.prepare("SELECT id, kind, source, title, precut, stage, error, project_id,channel_id,priority,position,paused,owner_node FROM factory_items ORDER BY priority DESC,position,id")?;
    let rows = st.query_map([], |r| {
        Ok(ItemDto {
            id: r.get(0)?,
            kind: r.get(1)?,
            source: r.get(2)?,
            title: r.get(3)?,
            precut: r.get::<_, i64>(4)? != 0,
            stage: r.get(5)?,
            error: r.get(6)?,
            project_id: r.get(7)?,
            channel_id: r.get(8)?,
            priority: r.get(9)?,
            position: r.get(10)?,
            paused: r.get(11)?,
            owner_node: r.get(12)?,
        })
    })?;
    rows.collect()
}

pub(crate) fn set_stage(
    db: &Connection,
    id: i64,
    stage: &str,
    error: Option<&str>,
) -> rusqlite::Result<()> {
    if !machine::STAGES.contains(&stage) && stage != "needs_check" {
        return Err(rusqlite::Error::InvalidParameterName(stage.into()));
    }
    let tx = db.unchecked_transaction()?;
    let (previous, previous_error): (String, Option<String>) = tx.query_row(
        "SELECT stage,error FROM factory_items WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !machine::transition(&previous, stage) {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "Недопустимый переход {previous} → {stage}"
        )));
    }
    tx.execute(
        "UPDATE factory_items SET stage=?2,error=?3,updated_at=unixepoch() WHERE id=?1",
        params![id, stage, error],
    )?;
    if previous != stage || previous_error.as_deref() != error {
        tx.execute("INSERT INTO events(item_id,channel_id,kind,stage,message) SELECT id,channel_id,?2,?3,?4 FROM factory_items WHERE id=?1",
            params![id,if error.is_some(){"attention"}else{"stage"},stage,error.unwrap_or(stage)])?;
    }
    tx.commit()
}

pub(crate) fn recover_interrupted(db: &Connection) {
    let stages: Vec<(i64, String)> = db
        .prepare("SELECT id,stage FROM factory_items")
        .and_then(|mut s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default();
    for (id, stage) in stages {
        let next = machine::recovery(&stage);
        if next != stage || ["drafting", "rendering_final"].contains(&stage.as_str()) {
            let _ = set_stage(
                db,
                id,
                next,
                Some("Продолжаю с сохранённого этапа после закрытия приложения"),
            );
        }
    }
    // Только прерванные черновики фабрики: ручные ошибки не скрываем и не перезапускаем бесконечно.
    let _=db.execute("UPDATE drafts SET status='queued',error=NULL WHERE status='failed' AND error='Рендер прерван: приложение было закрыто' AND moment_id IN (SELECT m.id FROM moments m JOIN factory_items f ON f.project_id=m.project_id WHERE f.stage='drafting')",[]);
}
fn paused(db: &Connection) -> bool {
    drafts::kv_get(db, "factory_paused").as_deref() == Some("1")
}
fn should_wait(db: &Connection, id: i64) -> bool {
    paused(db)||db.query_row("SELECT f.paused OR COALESCE(c.enabled,1)=0 OR COALESCE(c.rights_paused,0)=1 FROM factory_items f LEFT JOIN channels c ON c.id=f.channel_id WHERE f.id=?1",[id],|r|r.get::<_,bool>(0)).unwrap_or(true)
}
fn profile(db: &Connection, id: i64) -> Option<profiles::Snapshot> {
    db.query_row(
        "SELECT profile_json FROM factory_items WHERE id=?1",
        [id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .and_then(|j| serde_json::from_str(&j).ok())
}

/// Следующий элемент, который раннер должен взять: самый ранний из ждущих или уже скачивающихся.
fn next_item(db: &Connection) -> rusqlite::Result<Option<(i64, NewItem, Option<i64>)>> {
    let mut st = db.prepare(
        "SELECT id, kind, source, title, preset, precut, project_id, channel_id, priority FROM factory_items
         WHERE owner_node='local' AND stage IN ('queued', 'fetching') AND paused=0 AND (channel_id IS NULL OR EXISTS(SELECT 1 FROM channels c WHERE c.id=channel_id AND c.enabled=1 AND c.rights_paused=0)) ORDER BY priority DESC,position,id LIMIT 1",
    )?;
    let mut rows = st.query([])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    let item = NewItem {
        kind: r.get(1)?,
        source: r.get(2)?,
        title: r.get(3)?,
        preset: r.get(4)?,
        precut: r.get::<_, i64>(5)? != 0,
        channel_id: r.get(7)?,
        priority: r.get(8)?,
    };
    Ok(Some((r.get(0)?, item, r.get(6)?)))
}

/// Элементы, которые ждут одобрения или уже рендерят финал: раннер не выходит, пока они есть.
fn pending(db: &Connection) -> rusqlite::Result<Vec<(i64, i64)>> {
    let mut st = db.prepare("SELECT id, project_id FROM factory_items WHERE owner_node='local' AND stage IN ('drafting','qc','awaiting_selection','awaiting_approval','auto_approved','rendering_final') AND project_id IS NOT NULL AND paused=0 AND (channel_id IS NULL OR EXISTS(SELECT 1 FROM channels c WHERE c.id=channel_id AND c.enabled=1 AND c.rights_paused=0))")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

fn has_queued(db: &Connection) -> rusqlite::Result<bool> {
    db.query_row("SELECT EXISTS (SELECT 1 FROM factory_items WHERE owner_node='local' AND stage IN ('queued', 'fetching') AND paused=0 AND (channel_id IS NULL OR EXISTS(SELECT 1 FROM channels c WHERE c.id=channel_id AND c.enabled=1 AND c.rights_paused=0)))", [], |r| r.get(0))
}

// ---------------------------------------------------------------- решения (чистые, проверяются тестами)

/// Сколько черновиков проекта в каком состоянии.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Tally {
    /// Превью готово, ждёт одобрения.
    pub ready: i64,
    /// Одобрено, финал ещё не запущен.
    pub approved: i64,
    /// Что-то ещё делается: превью или финал в очереди и в работе.
    pub busy: i64,
    /// Не собрались (превью или финал).
    pub failed: i64,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// Ждём человека или рендер.
    Wait,
    /// Есть одобренные черновики: запускаем финал.
    StartFinals,
    /// Все черновики собраны или отклонены.
    Finish,
}

/// Одобрение запускает финал автоматически. Элемент закрываем, только когда ни один черновик не ждёт,
/// не рендерится и не упал: упавший ждёт, пока его починят («Повторить») или отклонят в «Черновиках».
pub(crate) fn step_after(t: &Tally) -> Step {
    if t.approved > 0 {
        Step::StartFinals
    } else if t.ready > 0 || t.busy > 0 || t.failed > 0 {
        Step::Wait
    } else {
        Step::Finish
    }
}

fn tally(db: &Connection, project_id: i64) -> rusqlite::Result<Tally> {
    db.query_row(
        "SELECT COALESCE(SUM(d.status = 'ready'), 0), COALESCE(SUM(d.status = 'approved'), 0),
                COALESCE(SUM(d.status IN ('queued', 'rendering', 'final_queued', 'final_rendering')), 0),
                COALESCE(SUM(d.status IN ('failed', 'final_failed')), 0)
         FROM drafts d JOIN moments m ON m.id = d.moment_id WHERE m.project_id = ?1",
        [project_id],
        |r| Ok(Tally { ready: r.get(0)?, approved: r.get(1)?, busy: r.get(2)?, failed: r.get(3)? }),
    )
}

// ---------------------------------------------------------------- раннер

/// Включает раннер, если он ещё не работает. Вызывается после добавления и после перезапуска.
pub(crate) fn start(app: &AppHandle) {
    let state = app.state::<AppState>();
    if paused(&state.db.0.lock().unwrap()) {
        return;
    }
    let (tx, rx) = watch::channel(false);
    {
        let mut stop = state.batch.stop.lock().unwrap();
        if stop.is_some() {
            return;
        }
        *stop = Some(tx);
    }
    let app = app.clone();
    crate::host::spawn(async move {
        run(app.clone(), rx).await;
        let _ = app.emit("batch-changed", ());
        let _ = app.emit("factory-changed", ());
    });
}

/// После перезапуска: если остались элементы, которые ждут одобрения или рендерятся, раннер снова следит за ними.
pub(crate) fn resume(app: &AppHandle) {
    let waiting = {
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        resources::load(&db).resume_on_start
            && !paused(&db)
            && (pending(&db).map(|p| !p.is_empty()).unwrap_or(false)
                || has_queued(&db).unwrap_or(false))
    };
    if waiting {
        start(app);
    }
}

async fn run(app: AppHandle, mut stop: watch::Receiver<bool>) {
    // опережающие загрузки: элемент id → задача, которая его скачивает
    let mut prefetch: HashMap<i64, JoinHandle<CmdResult<Project>>> = HashMap::new();
    loop {
        if *stop.borrow() {
            break;
        }
        settle(&app).await;
        let next = {
            let state = app.state::<AppState>();
            let db = state.db.0.lock().unwrap();
            next_item(&db).ok().flatten()
        };
        match next {
            Some((id, item, project_id)) => {
                process(&app, id, item, project_id, &mut prefetch).await
            }
            None => {
                // больше нечего скачивать: выходим, если никто не ждёт одобрения; иначе смотрим за ним
                let state = app.state::<AppState>();
                let mut guard = state.batch.stop.lock().unwrap();
                let idle = {
                    let db = state.db.0.lock().unwrap();
                    pending(&db).map(|p| p.is_empty()).unwrap_or(true)
                        && !has_queued(&db).unwrap_or(false)
                };
                if idle {
                    *guard = None;
                    return;
                }
                drop(guard);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                    _ = stop.changed() => {}
                }
            }
        }
    }
    *app.state::<AppState>().batch.stop.lock().unwrap() = None;
}

/// Один проход по ждущим элементам: одобрено — финал, всё собрано — закрываем.
async fn settle(app: &AppHandle) {
    let state = app.state::<AppState>();
    let waiting = pending(&state.db.0.lock().unwrap()).unwrap_or_default();
    for (id, project_id) in waiting {
        if paused(&state.db.0.lock().unwrap()) {
            return;
        }
        let snap = profile(&state.db.0.lock().unwrap(), id);
        // После аварии возобновляем незавершённые превью той же очередью рендера.
        let stage = state
            .db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT stage FROM factory_items WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_default();
        if stage == DRAFTING {
            if let Err(e) = drafts::generate_and_render(app, project_id) {
                update(app, id, FAILED, Some(&e));
                continue;
            }
        }
        let ds = drafts::list(&state.db.0.lock().unwrap(), project_id).unwrap_or_default();
        if stage == "awaiting_selection" {
            if ds.is_empty() {
                continue;
            }
            update(app, id, DRAFTING, None);
        }
        let busy = ds
            .iter()
            .any(|d| ["queued", "rendering"].contains(&d.status.as_str()));
        if !busy {
            for d in ds.iter().filter(|d| d.status == "ready") {
                let old = qc_reports(&state.db.0.lock().unwrap(), id).remove(&d.id.to_string());
                let path = d.proxy_path.as_deref().unwrap_or("");
                let stamp = qc::file_stamp(std::path::Path::new(path));
                let report = if let Some(r) = old.filter(|r| {
                    r.path == path && r.fingerprint == stamp && r.version == qc::VERSION
                }) {
                    r
                } else {
                    update(app, id, "qc", None);
                    let subtitles =
                        has_words(&state.db.0.lock().unwrap(), project_id, d.start, d.end);
                    let max_duration = snap
                        .as_ref()
                        .map(|s| {
                            s.channel
                                .targets
                                .iter()
                                .map(|t| match t.platform.as_str() {
                                    "telegram" => 3600.0,
                                    "tiktok" => 600.0,
                                    _ => 180.0,
                                })
                                .fold(180.0, f64::min)
                        })
                        .unwrap_or(180.0);
                    let rules = qc::Rules {
                        max_duration,
                        subtitles_required: d.config.style.subtitles.enabled,
                        subtitles_present: subtitles,
                        face_required: d.config.style.layout
                            != crate::render::Layout::GameplayStack,
                        face_present: d.config.face_check.as_ref().is_some_and(|c| c.present()),
                        final_size: false,
                        content_height: Some(
                            if d.config.style.layout == crate::render::Layout::ReactionPip
                                && !d.config.style.reaction.background
                            {
                                1.0
                            } else {
                                d.config.style.split
                            },
                        ),
                    };
                    let r = qc::check(std::path::Path::new(path), &rules).await;
                    let db = state.db.0.lock().unwrap();
                    let mut reports = qc_reports(&db, id);
                    reports.insert(d.id.to_string(), r.clone());
                    let _ = db.execute(
                        "UPDATE factory_items SET qc_json=?2 WHERE id=?1",
                        params![id, serde_json::to_string(&reports).unwrap_or_default()],
                    );
                    r
                };
                if should_wait(&state.db.0.lock().unwrap(), id) {
                    update(app, id, AWAITING, None);
                    return;
                }
                if snap.as_ref().is_some_and(|s| profiles::auto_allowed(&s.channel)) && report.passed() {
                    let db = state.db.0.lock().unwrap();
                    let _=db.execute("UPDATE drafts SET status='approved',updated_at=unixepoch() WHERE id=?1 AND status='ready'",[d.id]);
                    drop(db);
                    update(app, id, "auto_approved", None);
                }
            }
        }
        let t = tally(&state.db.0.lock().unwrap(), project_id).unwrap_or_default();
        match step_after(&t) {
            Step::Wait => {
                if !busy && t.ready > 0 {
                    let reasons = qc_reports(&state.db.0.lock().unwrap(), id)
                        .values()
                        .flat_map(|r| r.issues.clone())
                        .collect::<Vec<_>>();
                    let reason = reasons.join("; ");
                    update(
                        app,
                        id,
                        AWAITING,
                        if reason.is_empty() {
                            None
                        } else {
                            Some(reason.as_str())
                        },
                    );
                } else if !busy && t.failed > 0 {
                    update(
                        app,
                        id,
                        AWAITING,
                        Some("Есть ошибки рендера. Открой Черновики, повтори или отклони ролик"),
                    );
                }
            }
            Step::StartFinals => match drafts::start_finals(app, project_id) {
                Ok(_) => update(app, id, RENDERING, None),
                Err(e) => update(app, id, FAILED, Some(&e)),
            },
            Step::Finish => {
                let has_finals = ds.iter().any(|d| d.status == "final_done");
                update(
                    app,
                    id,
                    if snap.is_some() && has_finals {
                        "ready_to_schedule"
                    } else {
                        DONE
                    },
                    None,
                );
                if has_finals
                    && snap.as_ref().is_some_and(|s| {
                        s.channel.mode != "manual" && !s.channel.targets.is_empty()
                    })
                {
                    crate::publish::autofill::finish(app, id, project_id).await;
                }
            }
        }
    }
}

fn qc_reports(db: &Connection, id: i64) -> HashMap<String, qc::Report> {
    db.query_row("SELECT qc_json FROM factory_items WHERE id=?1", [id], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|v| serde_json::from_str(&v).ok())
    .unwrap_or_default()
}
fn has_words(db: &Connection, project: i64, start: f64, end: f64) -> bool {
    db.query_row(
        "SELECT words_json FROM analysis WHERE project_id=?1",
        [project],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
    .is_some_and(|ws| {
        ws.iter().any(|w| {
            w["start"].as_f64().is_some_and(|v| v < end)
                && w["end"].as_f64().is_some_and(|v| v > start)
        })
    })
}
fn has_accepted(db: &Connection, project: i64) -> CmdResult<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM moments WHERE project_id=?1 AND status='accepted')",
        [project],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}
fn select_moments(
    db: &Connection,
    project: i64,
    snap: &profiles::Snapshot,
    precut: bool,
) -> CmdResult<()> {
    // Повтор с уже созданными черновиками не меняет пользовательские решения.
    if db.query_row("SELECT EXISTS(SELECT 1 FROM drafts d JOIN moments m ON m.id=d.moment_id WHERE m.project_id=?1)",[project],|r|r.get::<_,bool>(0)).map_err(|e|e.to_string())? {return Ok(());}
    let mut st = db
        .prepare("SELECT id,score,start FROM moments WHERE project_id=?1 ORDER BY start")
        .map_err(|e| e.to_string())?;
    let scores = st
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let fixed = precut
        || snap
            .channel
            .analysis
            .clone()
            .into_options()
            .fixed_seconds
            .is_some()
        || db
            .query_row("SELECT tight FROM projects WHERE id=?1", [project], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap_or(false);
    let threshold = if snap.channel.mode == "auto" {
        snap.channel.score_threshold
    } else {
        0.0
    };
    let ids = machine::selected(&scores, threshold, snap.channel.max_clips, fixed);
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE moments SET status='candidate' WHERE project_id=?1",
        [project],
    )
    .map_err(|e| e.to_string())?;
    for id in ids {
        tx.execute("UPDATE moments SET status='accepted' WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn update(app: &AppHandle, id: i64, stage: &str, error: Option<&str>) {
    let state = app.state::<AppState>();
    if let Err(e) = set_stage(&state.db.0.lock().unwrap(), id, stage, error) {
        tracing::error!(
            item_id = id,
            stage,
            "Не удалось сохранить этап фабрики: {e}"
        );
    }
    if error.is_some() {
        crate::notifications::attention(
            app,
            format!("factory:{id}:{stage}"),
            "Фабрике требуется действие",
            "Открой «Фабрика» и журнал «Что происходит», чтобы проверить причину остановки.",
        );
    }
    let _ = app.emit("batch-changed", ());
    let _ = app.emit("factory-changed", ());
}

fn set_project(app: &AppHandle, id: i64, project: &Project) -> CmdResult<Project> {
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    let pid = attach_project(&db, id, project.id)?;
    let project = db
        .query_row(
            &format!(
                "SELECT {} FROM projects WHERE id=?1",
                commands::PROJECT_COLS
            ),
            [pid],
            commands::project_from_row,
        )
        .map_err(|e| e.to_string())?;
    drop(db);
    let _ = app.emit("batch-changed", ());
    let _ = app.emit("factory-changed", ());
    Ok(project)
}
/// Один файл в двух каналах получает независимые моменты/черновики, без копирования самого видео.
fn attach_project(db: &Connection, id: i64, source_project: i64) -> CmdResult<i64> {
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let (current, profile): (Option<i64>, Option<String>) = tx
        .query_row(
            "SELECT project_id,profile_json FROM factory_items WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if let Some(current) = current {
        return Ok(current);
    }
    let pid = if profile.is_some() {
        tx.execute("INSERT INTO projects(path,hash,title,duration,width,height,fps,has_audio,tight) SELECT path,'factory:'||?1||':'||hash,title,duration,width,height,fps,has_audio,tight FROM projects WHERE id=?2",params![id,source_project]).map_err(|e|e.to_string())?;
        tx.last_insert_rowid()
    } else {
        source_project
    };
    tx.execute("UPDATE factory_items SET project_id=?2,title=CASE WHEN title='' THEN (SELECT title FROM projects WHERE id=?2) ELSE title END,updated_at=unixepoch() WHERE id=?1",params![id,pid]).map_err(|e|e.to_string())?;
    tx.execute("INSERT OR IGNORE INTO analysis_sources(project_id,chat_json,clips_json,chat_attempted,chat_error) SELECT ?1,chat_json,clips_json,chat_attempted,chat_error FROM analysis_sources WHERE project_id=?2",params![pid,source_project]).map_err(|e|e.to_string())?;
    tx.execute(
        "UPDATE source_files SET project_id=?2 WHERE item_id=?1",
        params![id, pid],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(pid)
}

/// Делает один элемент: скачать или импортировать → нарезать (или полный кадр) → черновики.
async fn process(
    app: &AppHandle,
    id: i64,
    item: NewItem,
    project_id: Option<i64>,
    prefetch: &mut HashMap<i64, JoinHandle<CmdResult<Project>>>,
) {
    let outcome = async {
        let snapshot=profile(&app.state::<AppState>().db.0.lock().unwrap(),id);
        let project = match project_id {
            Some(pid) => pid,
            None => {
                let project = match prefetch.remove(&id) {
                    Some(handle) => handle.await.map_err(|e| e.to_string())??,
                    None if item.kind == "link" => {
                        update(app, id, DOWNLOADING, None);
                        download(app, id, &item).await?
                    }
                    None => {
                        update(app, id, IMPORTING, None);
                        commands::import_video(app.state(), item.source.clone()).await?
                    }
                };
                set_project(app,id,&project)?.id
            }
        };

        prefetch_next(app, prefetch);

        if item.precut {
            // уже нарезано: один момент на весь файл, без поиска моментов
            full_moment(&app.state::<AppState>().db.0.lock().unwrap(), project)?;
        } else {
            // повтор после сбоя не должен заново стирать моменты: вместе с ними уйдут и черновики, которые человек уже видел
            let analysed = has_moments(&app.state::<AppState>().db.0.lock().unwrap(), project)?;
            if !analysed {
                update(app, id, ANALYZING, None);
                collect_chat(app,id,project,&item,snapshot.as_ref()).await;
                collect_viewer_clips(app,id,project,&item,snapshot.as_ref()).await;
                analyze(app, project,snapshot.as_ref().map(|s|s.channel.analysis.clone())).await?;
            }
        }

        if should_wait(&app.state::<AppState>().db.0.lock().unwrap(),id) {
            update(app,id,QUEUED,None);return Ok(());
        }
        update(app,id,"selecting",None);
        if snapshot.as_ref().is_some_and(|s|s.channel.mode=="manual") {
            update(app,id,"awaiting_selection",Some("Открой источник в Студии и выбери моменты"));return Ok(());
        }
        if let Some(ref snap)=snapshot {
            let state=app.state::<AppState>();let db=state.db.0.lock().unwrap();
            select_moments(&db,project,snap,item.precut)?;
        } else {
            app.state::<AppState>().db.0.lock().unwrap().execute("UPDATE moments SET status='accepted' WHERE project_id=?1 AND status='candidate'",[project]).map_err(|e|e.to_string())?;
        }
        if !has_accepted(&app.state::<AppState>().db.0.lock().unwrap(),project)? {
            update(app,id,"awaiting_selection",Some("Нет моментов выше порога. Можно выбрать вручную в Студии"));return Ok(());
        }
        update(app, id, DRAFTING, None);
        drafts::generate_and_render(app, project)?;
        Ok::<(), String>(())
    }
    .await;

    if let Err(message) = outcome {
        let stopped = app
            .state::<AppState>()
            .batch
            .stop
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|tx| *tx.borrow());
        let message = if stopped {
            "Остановлено".to_string()
        } else {
            message
        };
        update(app, id, FAILED, Some(&message));
    }
}

/// Анализ уже был (хоть один момент). Удаление моментов каскадом стирает черновики, поэтому повтор его не запускает.
fn has_moments(db: &Connection, project_id: i64) -> CmdResult<bool> {
    db.query_row(
        "SELECT EXISTS (SELECT 1 FROM moments WHERE project_id = ?1)",
        [project_id],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// Уже нарезанный файл: один принятый момент на всё видео. При повторе момент не дублируется.
fn full_moment(db: &Connection, project_id: i64) -> CmdResult<()> {
    db.execute(
        "INSERT INTO moments (project_id, start, \"end\", title, status)
         SELECT id, 0, duration, title, 'accepted' FROM projects
         WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM moments WHERE project_id = ?1)",
        [project_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Скачивание ссылки в пачке: та же загрузка, что у кнопки, но результат возвращается прямо.
async fn collect_viewer_clips(
    app: &AppHandle,
    id: i64,
    project: i64,
    item: &NewItem,
    snapshot: Option<&profiles::Snapshot>,
) {
    if !snapshot.is_some_and(|s| s.channel.monitor.viewer_clips) {
        return;
    }
    if crate::analysis_inputs::load(&app.state::<AppState>().db.0.lock().unwrap(), project)
        .clips
        .is_some()
    {
        return;
    }
    let Ok(source) = crate::sources::classify_link(&item.source) else {
        return;
    };
    if source.kind != "twitch" {
        return;
    }
    let Some(vod) = source
        .url
        .split('/')
        .next_back()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    else {
        return;
    };
    match crate::analysis_inputs::fetch_clips(app, project, vod).await {
        Ok(data) => {
            let _ = crate::analysis_inputs::save_clips(
                &app.state::<AppState>().db.0.lock().unwrap(),
                project,
                &data,
            );
        }
        Err(error) => {
            let _ = app.state::<AppState>().db.0.lock().unwrap().execute(
                "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'warning','scouting',?2)",
                params![id, format!("Клипы зрителей пропущены: {error}")],
            );
        }
    }
}

async fn collect_chat(
    app: &AppHandle,
    id: i64,
    project: i64,
    item: &NewItem,
    snapshot: Option<&profiles::Snapshot>,
) {
    let Some(snapshot) = snapshot.filter(|s| s.channel.monitor.download_chat) else {
        return;
    };
    let state = app.state::<AppState>();
    let attempted: bool = state
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT chat_attempted FROM analysis_sources WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if attempted {
        return;
    }
    let dir = state
        .data_dir
        .join("sources")
        .join(id.to_string())
        .join("chat");
    let kind = crate::sources::classify_link(&item.source)
        .map(|s| s.kind)
        .unwrap_or_else(|_| snapshot.channel.source_kind.clone());
    let result=async {
        let _slot=state.youtube_slots.acquire().await.map_err(|e|e.to_string())?;
        let path=crate::sources::chat::download(&kind,&item.source,&snapshot.channel.monitor,&dir).await?;
        state.db.0.lock().unwrap().execute("INSERT OR IGNORE INTO source_files(path,channel_id,project_id,item_id,kind) VALUES(?1,?2,?3,?4,'chat')",params![path.to_string_lossy(),snapshot.channel.id,project,id]).map_err(|e|e.to_string())?;
        let duration:f64=state.db.0.lock().unwrap().query_row("SELECT duration FROM projects WHERE id=?1",[project],|r|r.get(0)).map_err(|e|e.to_string())?;
        let data=tokio::task::spawn_blocking(move||crate::analyze::chat::load(&path,duration)).await.map_err(|e|e.to_string())?.map_err(|e|format!("{e:#}"))?;
        crate::analysis_inputs::save_chat(&state.db.0.lock().unwrap(),project,&data)?;
        Ok::<_,String>(data.messages)
    }.await;
    let db = state.db.0.lock().unwrap();
    match result {
        Ok(count) => {
            let _ = db.execute(
                "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'info','scouting',?2)",
                params![id, format!("Чат: {count} сообщений для скаута")],
            );
        }
        Err(error) => {
            let _=db.execute("INSERT INTO analysis_sources(project_id,chat_attempted,chat_error) VALUES(?1,1,?2) ON CONFLICT(project_id) DO UPDATE SET chat_attempted=1,chat_error=excluded.chat_error",params![project,error]);
            let _ = db.execute(
                "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'warning','scouting',?2)",
                params![id, format!("Чат пропущен: {error}")],
            );
        }
    }
}

async fn download(app: &AppHandle, id: i64, item: &NewItem) -> CmdResult<Project> {
    if item.channel_id.is_some() || crate::sources::classify_link(&item.source)?.kind != "youtube" {
        return media::download(app, id, item).await;
    }
    let args = StartArgs {
        url: item.source.clone(),
        preset: item.preset.clone(),
        title: item.title.clone(),
        channel: String::new(),
        thumb: None,
    };
    let state = app.state::<AppState>();
    let jobs = &state.batch.jobs;
    let project = youtube_cmds::download_for_batch(app, args, |job| {
        jobs.lock().unwrap().insert(id, job.to_string());
    })
    .await;
    jobs.lock().unwrap().remove(&id);
    project
}

/// Пока анализируется текущий элемент, скачиваем следующую ссылку (сеть и видеокарта не мешают друг другу).
fn prefetch_next(app: &AppHandle, prefetch: &mut HashMap<i64, JoinHandle<CmdResult<Project>>>) {
    if !prefetch.is_empty() {
        return;
    }
    let next = {
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        db.query_row(
            "SELECT id, kind, source, title, preset, precut, channel_id, priority FROM factory_items WHERE stage = 'queued' AND kind = 'link' AND project_id IS NULL AND paused=0 AND (channel_id IS NULL OR EXISTS(SELECT 1 FROM channels c WHERE c.id=channel_id AND c.enabled=1 AND c.rights_paused=0)) ORDER BY priority DESC,position,id LIMIT 1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, NewItem { kind: r.get(1)?, source: r.get(2)?, title: r.get(3)?, preset: r.get(4)?, precut: r.get::<_, i64>(5)? != 0,channel_id:r.get(6)?,priority:r.get(7)? })),
        )
        .ok()
    };
    let Some((id, item)) = next else { return };
    update(app, id, DOWNLOADING, None);
    let app = app.clone();
    prefetch.insert(
        id,
        tokio::spawn(async move {
            let result = download(&app, id, &item).await;
            match result {
                Ok(p) => set_project(&app, id, &p),
                Err(e) => {
                    update(&app, id, FAILED, Some(&e));
                    Err(e)
                }
            }
        }),
    );
}

/// Анализ точно как у одного видео: те же настройки по умолчанию, что у кнопки «Анализ». Все найденные моменты принимаются.
async fn analyze(
    app: &AppHandle,
    project_id: i64,
    options: Option<analysis_cmds::UiOptions>,
) -> CmdResult<()> {
    let (input, rx) = {
        let state = app.state::<AppState>();
        analysis_cmds::begin(&state, project_id)?
    };
    *app.state::<AppState>().batch.analysis.lock().unwrap() = Some(project_id);
    let opt: Options = options
        .unwrap_or_else(|| {
            analysis_cmds::load_options(&app.state::<AppState>().db.0.lock().unwrap())
        })
        .into_options();
    let result = analysis_cmds::execute(app.clone(), input, rx, opt).await;
    *app.state::<AppState>().batch.analysis.lock().unwrap() = None;
    result?;
    Ok(())
}

// ---------------------------------------------------------------- команды

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_list(state: State<'_, AppState>) -> CmdResult<Vec<ItemDto>> {
    list(&state.db.0.lock().unwrap()).map_err(|e| e.to_string())
}

/// Добавляет ссылки и файлы в конец пачки. Проверяем сразу, чтобы ошибка была видна на месте, а не через час.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_add(
    app: AppHandle,
    state: State<'_, AppState>,
    items: Vec<NewItem>,
) -> CmdResult<usize> {
    if items.is_empty() {
        return Err("Нет ни одной ссылки или файла".into());
    }
    for it in &items {
        match it.kind.as_str() {
            "link" => {
                if crate::sources::classify_link(&it.source)?.collection {
                    return Err(
                        "Канал или плейлист добавляется через профиль в разделе «Каналы»".into(),
                    );
                }
                select::height_limit(&it.preset)?;
            }
            "file" => {
                if !std::path::Path::new(&it.source).is_file() {
                    return Err(format!("Файл не найден: {}", it.source));
                }
            }
            other => return Err(format!("неизвестный вид элемента: {other}")),
        }
    }
    let n = add(&state.db.0.lock().unwrap(), &items).map_err(|e| e.to_string())?;
    let _ = app.emit("batch-changed", ());
    let _ = app.emit("factory-changed", ());
    Ok(n)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_start(app: AppHandle) {
    let state = app.state::<AppState>();
    let _ = drafts::kv_set(&state.db.0.lock().unwrap(), "factory_paused", "0");
    state
        .monitor
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    state.monitor.wake.notify_one();
    if let Some(tx) = state.batch.stop.lock().unwrap().as_ref() {
        let _ = tx.send(false);
    }
    start(&app);
}

/// Пауза на безопасной границе: загрузка/анализ завершаются, следующая стадия ждёт.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_cancel(state: State<'_, AppState>) {
    let _ = drafts::kv_set(&state.db.0.lock().unwrap(), "factory_paused", "1");
    if let Some(tx) = state.batch.stop.lock().unwrap().as_ref() {
        let _ = tx.send(true);
    }
}

/// Убирает элемент, который не сейчас в работе.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_remove(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let n = state
        .db
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM factory_items WHERE id = ?1 AND stage IN ('queued','failed','completed','ready_to_schedule','awaiting_selection')", [id])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("Этот элемент сейчас в работе: сначала останови пачку".into());
    }
    let _ = app.emit("batch-changed", ());
    let _ = app.emit("factory-changed", ());
    Ok(())
}

/// Ставит упавший элемент обратно в очередь. Уже добавленное видео не добавляется второй раз.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_retry(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE factory_items SET stage = 'queued', error = NULL, updated_at = unixepoch() WHERE id = ?1 AND stage = 'failed'", [id])
        .map_err(|e| e.to_string())?;
    start(&app);
    let _ = app.emit("batch-changed", ());
    let _ = app.emit("factory-changed", ());
    Ok(())
}

/// Одобряет готовые черновики всех роликов пачки. Финал запустится сам, раннер это заметит.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn batch_approve_ready(app: AppHandle, state: State<'_, AppState>) -> CmdResult<usize> {
    let db = state.db.0.lock().unwrap();
    let projects: Vec<i64> = pending(&db)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    let mut n = 0;
    for pid in projects {
        n += drafts::approve_ready(&db, pid).map_err(|e| e.to_string())?;
    }
    drop(db);
    let _ = app.emit("batch-changed", ());
    let _ = app.emit("factory-changed", ());
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute("INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (1, '/v', 'h', 'v', 100, 1, 1, 30, 1)", []).unwrap();
        c
    }

    fn fixture_stage(
        db: &Connection,
        id: i64,
        stage: &str,
        error: Option<&str>,
    ) -> rusqlite::Result<()> {
        db.execute(
            "UPDATE factory_items SET stage=?2,error=?3 WHERE id=?1",
            params![id, stage, error],
        )?;
        Ok(())
    }

    fn link(source: &str) -> NewItem {
        NewItem {
            kind: "link".into(),
            source: source.into(),
            title: String::new(),
            preset: "1080".into(),
            precut: false,
            channel_id: None,
            priority: 0,
        }
    }

    #[test]
    fn factory_jobs_isolate_the_same_file_and_retry_reuses_the_project() {
        let c = db();
        let channel = profiles::save_channel(
            &c,
            profiles::Channel {
                name: "Канал".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut item = link("/same.mp4");
        item.kind = "file".into();
        item.channel_id = Some(channel.id);
        add(&c, &[item.clone(), item]).unwrap();
        let a = attach_project(&c, 1, 1).unwrap();
        let b = attach_project(&c, 2, 1).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, 1);
        assert_eq!(attach_project(&c, 1, 1).unwrap(), a);
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM projects", [], |r| r.get(0))
                .unwrap(),
            3
        );
        c.execute(
            "INSERT INTO moments(project_id,start,\"end\",status) VALUES(?1,0,20,'accepted')",
            [a],
        )
        .unwrap();
        assert!(!has_moments(&c, b).unwrap());
    }
    #[test]
    fn priority_pause_and_disabled_channels_control_the_next_pick() {
        let c = db();
        let channel = profiles::save_channel(
            &c,
            profiles::Channel {
                name: "Канал".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut high = link("high");
        high.priority = 10;
        high.channel_id = Some(channel.id);
        add(&c, &[link("normal"), high]).unwrap();
        assert_eq!(next_item(&c).unwrap().unwrap().0, 2);
        c.execute("UPDATE factory_items SET paused=1 WHERE id=2", [])
            .unwrap();
        assert_eq!(next_item(&c).unwrap().unwrap().0, 1);
        c.execute("UPDATE factory_items SET paused=0 WHERE id=2", [])
            .unwrap();
        c.execute("UPDATE channels SET enabled=0 WHERE id=?1", [channel.id])
            .unwrap();
        assert_eq!(next_item(&c).unwrap().unwrap().0, 1);
    }
    #[test]
    fn channel_policy_caps_accepted_moments_and_preserves_drafts_on_retry() {
        let c = db();
        let channel = profiles::save_channel(
            &c,
            profiles::Channel {
                name: "Auto".into(),
                mode: "auto".into(),
                score_threshold: 0.7,
                max_clips: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let snap: profiles::Snapshot =
            serde_json::from_str(&profiles::snapshot(&c, channel.id).unwrap()).unwrap();
        c.execute_batch("INSERT INTO moments(project_id,start,\"end\",score,status) VALUES(1,0,20,0.2,'accepted'),(1,30,50,0.9,'candidate'),(1,60,80,0.8,'candidate'),(1,80,99,0.75,'candidate');").unwrap();
        select_moments(&c, 1, &snap, false).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>(
                "SELECT count(*) FROM moments WHERE status='accepted'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            2
        );
        drafts::generate(&c, 1).unwrap();
        c.execute("UPDATE moments SET status='rejected' WHERE start=30", [])
            .unwrap();
        select_moments(&c, 1, &snap, false).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>(
                "SELECT count(*) FROM moments WHERE status='accepted'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn pause_and_channel_disable_stop_at_the_same_checkpoint_and_survive_restart() {
        let c = db();
        let channel = profiles::save_channel(
            &c,
            profiles::Channel {
                name: "Пауза".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut item = link("x");
        item.channel_id = Some(channel.id);
        add(&c, &[item]).unwrap();
        assert!(!should_wait(&c, 1));
        c.execute("UPDATE factory_items SET paused=1 WHERE id=1", [])
            .unwrap();
        assert!(should_wait(&c, 1));
        recover_interrupted(&c);
        assert!(should_wait(&c, 1));
        c.execute("UPDATE factory_items SET paused=0 WHERE id=1", [])
            .unwrap();
        c.execute("UPDATE channels SET enabled=0 WHERE id=?1", [channel.id])
            .unwrap();
        assert!(should_wait(&c, 1));
        c.execute("UPDATE channels SET enabled=1 WHERE id=?1", [channel.id])
            .unwrap();
        drafts::kv_set(&c, "factory_paused", "1").unwrap();
        assert!(should_wait(&c, 1));
        drafts::kv_set(&c, "factory_paused", "0").unwrap();
        assert!(!should_wait(&c, 1));
    }

    #[test]
    fn stage_change_is_atomic_and_rejects_a_jump_to_publication() {
        let c = db();
        add(&c, &[link("test")]).unwrap();
        assert!(set_stage(&c, 1, "published", None).is_err());
        assert_eq!(list(&c).unwrap()[0].stage, QUEUED);
        set_stage(&c, 1, "fetching", None).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM events WHERE item_id=1", [], |r| r
                .get(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn items_keep_their_order_and_the_runner_takes_the_earliest_waiting_one() {
        let c = db();
        add(&c, &[link("a"), link("b")]).unwrap();
        add(&c, &[link("c")]).unwrap();
        let (id, item, _) = next_item(&c).unwrap().unwrap();
        assert_eq!((id, item.source.as_str()), (1, "a"));
        fixture_stage(&c, 1, DONE, None).unwrap();
        assert_eq!(next_item(&c).unwrap().unwrap().1.source, "b");
    }

    #[test]
    fn a_draft_that_is_ready_waits_for_a_human_and_an_approved_one_starts_the_final() {
        assert_eq!(
            step_after(&Tally {
                ready: 2,
                ..Default::default()
            }),
            Step::Wait
        );
        assert_eq!(
            step_after(&Tally {
                ready: 1,
                approved: 1,
                ..Default::default()
            }),
            Step::StartFinals
        );
        assert_eq!(
            step_after(&Tally {
                busy: 1,
                ..Default::default()
            }),
            Step::Wait
        );
    }

    #[test]
    fn the_item_closes_only_when_no_draft_is_left_waiting_rendering_or_broken() {
        assert_eq!(step_after(&Tally::default()), Step::Finish);
        assert_eq!(
            step_after(&Tally {
                failed: 3,
                ..Default::default()
            }),
            Step::Wait,
            "упавшие ждут починки или отклонения"
        );
    }

    #[test]
    fn tally_counts_each_draft_state_once_and_only_for_its_project() {
        let c = db();
        c.execute("INSERT INTO moments (id, project_id, start, \"end\", status) VALUES (1, 1, 0, 10, 'accepted'), (2, 1, 10, 20, 'accepted')", []).unwrap();
        c.execute(
            "INSERT INTO drafts (moment_id, status) VALUES (1, 'ready'), (2, 'final_failed')",
            [],
        )
        .unwrap();
        assert_eq!(
            tally(&c, 1).unwrap(),
            Tally {
                ready: 1,
                approved: 0,
                busy: 0,
                failed: 1
            }
        );
        assert_eq!(tally(&c, 2).unwrap(), Tally::default());
    }

    #[test]
    fn a_restart_requeues_interrupted_steps_and_keeps_the_waiting_ones() {
        let c = db();
        add(&c, &[link("a"), link("b"), link("c")]).unwrap();
        fixture_stage(&c, 1, DOWNLOADING, None).unwrap();
        fixture_stage(&c, 2, AWAITING, None).unwrap();
        fixture_stage(&c, 3, RENDERING, None).unwrap();
        recover_interrupted(&c);
        let stages: Vec<String> = list(&c).unwrap().into_iter().map(|i| i.stage).collect();
        assert_eq!(stages, vec![QUEUED, AWAITING, RENDERING]);
    }

    #[test]
    fn a_precut_item_keeps_its_flag_and_its_project_once_known() {
        let c = db();
        add(
            &c,
            &[NewItem {
                kind: "file".into(),
                source: "/x.mp4".into(),
                title: String::new(),
                preset: "1080".into(),
                precut: true,
                channel_id: None,
                priority: 0,
            }],
        )
        .unwrap();
        c.execute("UPDATE factory_items SET project_id = 1 WHERE id = 1", [])
            .unwrap();
        let item = list(&c).unwrap().remove(0);
        assert!(item.precut);
        assert_eq!(item.project_id, Some(1));
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDto {
    pub id: i64,
    pub item_id: Option<i64>,
    pub kind: String,
    pub stage: Option<String>,
    pub message: String,
    pub created_at: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub item_id: i64,
    pub channel_name: String,
    pub draft: drafts::DraftDto,
    pub qc: Option<qc::Report>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryState {
    pub items: Vec<ItemDto>,
    pub events: Vec<EventDto>,
    pub inbox: Vec<InboxItem>,
    pub paused: bool,
    pub running: bool,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn factory_state(state: State<'_, AppState>) -> CmdResult<FactoryState> {
    let running = state.batch.stop.lock().unwrap().is_some();
    let db = state.db.0.lock().unwrap();
    let items = list(&db).map_err(|e| e.to_string())?;
    let mut st=db.prepare("SELECT id,item_id,kind,stage,message,created_at FROM events ORDER BY id DESC LIMIT 100").map_err(|e|e.to_string())?;
    let events = st
        .query_map([], |r| {
            Ok(EventDto {
                id: r.get(0)?,
                item_id: r.get(1)?,
                kind: r.get(2)?,
                stage: r.get(3)?,
                message: r.get(4)?,
                created_at: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut inbox = vec![];
    for it in &items {
        if !["drafting", "qc", "awaiting_approval", "rendering_final"].contains(&it.stage.as_str())
        {
            continue;
        }
        if let Some(pid) = it.project_id {
            let channel_name = profile(&db, it.id)
                .map(|s| s.channel.name)
                .unwrap_or_else(|| "Пачка из Студии".into());
            let reports = qc_reports(&db, it.id);
            for draft in drafts::list(&db, pid)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|d| ["ready", "failed", "final_failed"].contains(&d.status.as_str()))
            {
                inbox.push(InboxItem {
                    item_id: it.id,
                    channel_name: channel_name.clone(),
                    qc: reports.get(&draft.id.to_string()).cloned(),
                    draft,
                });
            }
        }
    }
    Ok(FactoryState {
        items,
        events,
        inbox,
        paused: paused(&db),
        running,
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn factory_item_update(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    priority: i64,
    position: i64,
    paused: bool,
) -> CmdResult<()> {
    if position < 0 || !(-100..=100).contains(&priority) {
        return Err("Порядок должен быть ≥0, приоритет — от −100 до 100".into());
    }
    let db = state.db.0.lock().unwrap();
    let was_paused: bool = db
        .query_row("SELECT paused FROM factory_items WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if db.execute("UPDATE factory_items SET priority=?2,position=?3,paused=?4,updated_at=unixepoch() WHERE id=?1",params![id,priority,position,paused]).map_err(|e|e.to_string())?==0{return Err("Источник не найден".into());}
    db.execute(
        "INSERT INTO events(item_id,kind,message) VALUES (?1,'control',?2)",
        params![
            id,
            if paused {
                "Источник поставлен на паузу"
            } else {
                "Изменён порядок или источник возобновлён"
            }
        ],
    )
    .map_err(|e| e.to_string())?;
    drop(db);
    if was_paused && !paused {
        start(&app);
    }
    let _ = app.emit("factory-changed", ());
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn factory_decide(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<i64>,
    approve: bool,
) -> CmdResult<usize> {
    let db = state.db.0.lock().unwrap();
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut n = 0;
    for id in ids {
        let status = if approve { "approved" } else { "rejected" };
        n+=tx.execute("UPDATE drafts SET status=?2,updated_at=unixepoch() WHERE id=?1 AND (status='ready' OR (?3=0 AND status IN ('failed','final_failed'))) AND moment_id IN (SELECT m.id FROM moments m JOIN factory_items f ON f.project_id=m.project_id)",params![id,status,approve]).map_err(|e|e.to_string())?;
    }
    tx.execute(
        "INSERT INTO events(kind,message) VALUES ('approval',?1)",
        [format!(
            "{} роликов: {n}",
            if approve {
                "Одобрено"
            } else {
                "Отклонено"
            }
        )],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    drop(db);
    start(&app);
    let _ = app.emit("factory-changed", ());
    Ok(n)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEntry {
    draft_id: i64,
    project_id: i64,
    title: String,
    channel_name: String,
    state: String,
    not_before: Option<i64>,
    final_path: Option<String>,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn factory_calendar(state: State<'_, AppState>) -> CmdResult<Vec<CalendarEntry>> {
    let db = state.db.0.lock().unwrap();
    let mut st=db.prepare("SELECT d.id,m.project_id,m.title,COALESCE(c.name,p.title),COALESCE(j.state,'ready_to_schedule'),j.not_before,d.final_path FROM drafts d JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id LEFT JOIN factory_items f ON f.project_id=p.id LEFT JOIN channels c ON c.id=f.channel_id LEFT JOIN publications j ON j.draft_id=d.id AND j.legacy=1 WHERE d.status='final_done' ORDER BY COALESCE(j.not_before,0),d.id").map_err(|e|e.to_string())?;
    let entries = st
        .query_map([], |r| {
            Ok(CalendarEntry {
                draft_id: r.get(0)?,
                project_id: r.get(1)?,
                title: r.get(2)?,
                channel_name: r.get(3)?,
                state: r.get(4)?,
                not_before: r.get::<_, Option<i64>>(5)?.filter(|v| *v > 0),
                final_path: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    entries.collect::<Result<_, _>>().map_err(|e| e.to_string())
}
