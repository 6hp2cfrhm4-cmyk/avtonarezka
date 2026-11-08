//! Очередь автовыкладки в TikTok: каждому готовому ролику своё описание от модели, дальше ролики выходят по одному
//! через браузер с паузой между ними и суточным лимитом.
//!
//! Очередь живёт в базе (`publish_jobs`), а работает, пока открыто приложение. После перезапуска очередь не стартует сама:
//! выкладка без присмотра начинается только по новому согласию человека.

use std::{collections::HashSet, path::{Path, PathBuf}, time::Duration};

use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, Row};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

use crate::{
    accounts,
    analyze::{
        describe::{self, MAX_LEN, MAX_TAGS, MIN_LEN},
        llm::{self, LlmClient},
        pipeline::Options,
    },
    drafts::{get as get_draft, kv_get, kv_set, render_pool_copy},
    models::ModelPaths,
    state::AppState,
    tiktok::{
        api::{len_utf16, make_caption, normalize_hashtags, now_unix},
        browser::{find_chromium, run_browser_post, AfterClick, BrowserPost, CAPTION_LIMIT},
    },
    tiktok_cmds::{begin_post, begin_ready_post, finish_post, latest_posts, mark_sent, PostDone, PostDto, PostFailed, PostProgress},
};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

const K_PAUSE: &str = "pub_pause_min";
const K_LIMIT: &str = "pub_daily_limit";
const K_WINDOW_START: &str = "pub_window_start";
const K_WINDOW_END: &str = "pub_window_end";
const DAY: i64 = 86_400;
/// Минут в сутках: верхняя граница окна публикации.
const DAY_MIN: i64 = 1440;

// ---------------------------------------------------------------- настройки

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueSettings {
    /// Пауза между роликами, минут.
    pub pause_min: i64,
    /// Сколько роликов не больше в местные сутки на каждый аккаунт.
    pub daily_limit: i64,
    /// Окно публикации: минуты от местной полуночи. Одинаковые начало и конец — без ограничения; начало больше конца — через полночь.
    pub window_start: i64,
    pub window_end: i64,
}

pub(crate) fn load_settings(db: &Connection) -> QueueSettings {
    QueueSettings {
        pause_min: kv_get(db, K_PAUSE).and_then(|v| v.parse().ok()).map_or(45, |v: i64| v.clamp(1, 1440)),
        daily_limit: kv_get(db, K_LIMIT).and_then(|v| v.parse().ok()).map_or(5, |v: i64| v.clamp(1, 50)),
        window_start: kv_get(db, K_WINDOW_START).and_then(|v| v.parse().ok()).map_or(0, |v: i64| v.clamp(0, DAY_MIN)),
        window_end: kv_get(db, K_WINDOW_END).and_then(|v| v.parse().ok()).map_or(DAY_MIN, |v: i64| v.clamp(0, DAY_MIN)),
    }
}

// ---------------------------------------------------------------- задачи

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobDto {
    pub id: i64,
    /// Черновик, если задача про ролик из пачки; у готового ролика из папки его нет.
    pub draft_id: Option<i64>,
    /// Готовый ролик из папки (`ready_videos`).
    pub ready_id: Option<i64>,
    /// Имя файла для готового ролика; у черновика пусто.
    pub name: String,
    /// Аккаунт, который человек выбрал для этого ролика; `None` — по кругу.
    pub wanted_account: Option<i64>,
    pub caption: String,
    pub hashtags: Vec<String>,
    /// `drafting`, `ready`, `queued`, `sending`, `done`, `failed`, `needs_check`.
    pub state: String,
    pub error: Option<String>,
    pub attempts: i64,
    pub not_before: i64,
    /// Длина подписи с хэштегами в единицах UTF-16, как её соберёт `make_caption`.
    pub len: usize,
    pub updated_at: i64,
}

const JOB_SELECT: &str = "SELECT j.id, j.draft_id, j.caption, j.hashtags, j.state, j.error, j.attempts, j.not_before, j.updated_at,
    j.ready_id, COALESCE(r.name, ''), j.wanted_account_id
    FROM publish_jobs j LEFT JOIN ready_videos r ON r.id = j.ready_id";

fn job_from_row(r: &Row) -> rusqlite::Result<JobDto> {
    let caption: String = r.get(2)?;
    let hashtags: Vec<String> = serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default();
    Ok(JobDto {
        id: r.get(0)?,
        draft_id: r.get(1)?,
        len: len_utf16(&make_caption(&caption, &hashtags, usize::MAX)),
        caption,
        hashtags,
        state: r.get(4)?,
        error: r.get(5)?,
        attempts: r.get(6)?,
        not_before: r.get(7)?,
        updated_at: r.get(8)?,
        ready_id: r.get(9)?,
        name: r.get(10)?,
        wanted_account: r.get(11)?,
    })
}

fn job_by_id(db: &Connection, id: i64) -> rusqlite::Result<JobDto> {
    db.query_row(&format!("{JOB_SELECT} WHERE j.id = ?1"), [id], job_from_row)
}

/// Задачи проекта: сначала ролики из пачки по частям, потом готовые ролики из папки.
pub(crate) fn jobs_for_project(db: &Connection, project_id: i64) -> rusqlite::Result<Vec<JobDto>> {
    let mut st = db.prepare(&format!(
        "{JOB_SELECT} LEFT JOIN drafts d ON d.id = j.draft_id LEFT JOIN moments m ON m.id = d.moment_id
         WHERE COALESCE(m.project_id, r.project_id) = ?1 ORDER BY COALESCE(d.part, 1000000), j.id"
    ))?;
    let rows = st.query_map([project_id], job_from_row)?;
    rows.collect()
}

/// Правило для любого текста, который пойдёт в очередь, будь он от модели, вставленный или поправленный руками:
/// текст не пустой, хэштегов от одного до `MAX_TAGS`, подпись с хэштегами влезает в страницу TikTok.
pub(crate) fn validate_job(caption: &str, hashtags: &[String]) -> Result<(Vec<String>, usize), String> {
    if caption.trim().is_empty() {
        return Err("Описание пустое".into());
    }
    let tags = normalize_hashtags(hashtags);
    if tags.is_empty() {
        return Err("Нужен хотя бы один хэштег".into());
    }
    if tags.len() > MAX_TAGS {
        return Err(format!("Хэштегов {}, а можно не больше {MAX_TAGS}", tags.len()));
    }
    let len = len_utf16(&make_caption(caption, &tags, usize::MAX));
    if len > CAPTION_LIMIT {
        return Err(format!("Подпись с хэштегами {len} знаков, а страница TikTok принимает не больше {CAPTION_LIMIT}"));
    }
    Ok((tags, len))
}

/// Описание от модели обязано занять `MIN_LEN`–`MAX_LEN` знаков: это цель генерации, а не лимит страницы.
/// Свой текст этому окну не подчиняется (см. [`validate_job`]).
pub(crate) fn validate_generated(caption: &str, hashtags: &[String]) -> Result<(Vec<String>, usize), String> {
    let (tags, len) = validate_job(caption, hashtags)?;
    if !(MIN_LEN..=MAX_LEN).contains(&len) {
        return Err(format!("Длина подписи с хэштегами {len}, а нужно {MIN_LEN}–{MAX_LEN} символов"));
    }
    Ok((tags, len))
}

/// Ролик, выложенный вручную через диалог: повторно описывать и выкладывать его очередью незачем.
fn was_published(p: &PostDto) -> bool {
    p.status == "done" && ["Нажата кнопка", "Опубликовано", "Выложено"].iter().any(|s| p.detail.starts_with(s))
}

pub(crate) struct Todo {
    pub draft_id: i64,
    pub title: String,
    pub transcript: String,
}

/// Готовые ролики проекта без описания становятся задачами `drafting`. Упавшие описания пишутся заново.
pub(crate) fn create_drafting_jobs(db: &Connection, project_id: i64) -> rusqlite::Result<Vec<Todo>> {
    let published: HashSet<i64> = latest_posts(db, project_id)?.iter().filter(|p| was_published(p)).map(|p| p.draft_id).collect();
    let mut st = db.prepare(
        "SELECT d.id, m.title, m.transcript FROM drafts d JOIN moments m ON m.id = d.moment_id
         WHERE m.project_id = ?1 AND d.status = 'final_done'
           AND NOT EXISTS (SELECT 1 FROM publish_jobs j WHERE j.draft_id = d.id AND j.state != 'failed')
         ORDER BY d.part, d.id",
    )?;
    let todo: Vec<Todo> = st
        .query_map([project_id], |r| Ok(Todo { draft_id: r.get(0)?, title: r.get(1)?, transcript: r.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let todo: Vec<Todo> = todo.into_iter().filter(|t| !published.contains(&t.draft_id)).collect();
    for t in &todo {
        db.execute(
            "INSERT INTO publish_jobs (draft_id, state) VALUES (?1, 'drafting')
             ON CONFLICT(draft_id) DO UPDATE SET state = 'drafting', caption = '', hashtags = '[]', error = NULL, updated_at = unixepoch()",
            [t.draft_id],
        )?;
    }
    Ok(todo)
}

fn store_ready(db: &Connection, draft_id: i64, caption: &str, hashtags: &[String]) -> rusqlite::Result<usize> {
    db.execute(
        "UPDATE publish_jobs SET caption = ?2, hashtags = ?3, state = 'ready', error = NULL, updated_at = unixepoch() WHERE draft_id = ?1 AND state = 'drafting'",
        params![draft_id, caption.trim(), serde_json::to_string(hashtags).unwrap_or_else(|_| "[]".into())],
    )
}

fn store_text_failed(db: &Connection, draft_id: i64, message: &str) {
    let _ = db.execute("UPDATE publish_jobs SET state = 'failed', error = ?2, updated_at = unixepoch() WHERE draft_id = ?1 AND state = 'drafting'", params![draft_id, message]);
}

/// Правка человека: только пока задача не ушла в работу. После правки задача снова готова к выкладке.
pub(crate) fn update_job(db: &Connection, id: i64, caption: &str, hashtags: &[String]) -> Result<JobDto, String> {
    let (tags, _) = validate_job(caption, hashtags)?;
    let state: String = db.query_row("SELECT state FROM publish_jobs WHERE id = ?1", [id], |r| r.get(0)).map_err(|_| "Задача не найдена".to_string())?;
    if !["ready", "failed"].contains(&state.as_str()) {
        return Err("Сейчас править нельзя: ролик в очереди, выкладывается или уже выложен".into());
    }
    db.execute(
        "UPDATE publish_jobs SET caption = ?2, hashtags = ?3, state = 'ready', error = NULL, updated_at = unixepoch() WHERE id = ?1",
        params![id, caption.trim(), serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into())],
    )
    .map_err(err)?;
    job_by_id(db, id).map_err(err)
}

/// Один свой текст на все готовые ролики проекта, без модели. Берутся ролики, которым можно назначить текст: у них нет задачи
/// либо она ещё не пошла в работу (готова или упала, текст в ней заменяется). Уже в очереди, выкладываемые, выложенные,
/// ждущие проверки и пишущиеся моделью не трогаем, как и ролики, выложенные вручную. Возвращает, скольким назначен текст.
pub(crate) fn apply_text(db: &Connection, project_id: i64, caption: &str, hashtags: &[String]) -> Result<usize, String> {
    let (tags, _) = validate_job(caption, hashtags)?;
    let published: HashSet<i64> = latest_posts(db, project_id).map_err(err)?.iter().filter(|p| was_published(p)).map(|p| p.draft_id).collect();
    let mut st = db
        .prepare(
            "SELECT d.id FROM drafts d JOIN moments m ON m.id = d.moment_id
             WHERE m.project_id = ?1 AND d.status = 'final_done'
               AND NOT EXISTS (SELECT 1 FROM publish_jobs j WHERE j.draft_id = d.id AND j.state IN ('drafting', 'queued', 'sending', 'done', 'needs_check'))
             ORDER BY d.part, d.id",
        )
        .map_err(err)?;
    let drafts: Vec<i64> = st.query_map([project_id], |r| r.get(0)).map_err(err)?.collect::<rusqlite::Result<_>>().map_err(err)?;
    let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into());
    let mut n = 0;
    for id in drafts.into_iter().filter(|d| !published.contains(d)) {
        db.execute(
            "INSERT INTO publish_jobs (draft_id, caption, hashtags, state) VALUES (?1, ?2, ?3, 'ready')
             ON CONFLICT(draft_id) DO UPDATE SET caption = excluded.caption, hashtags = excluded.hashtags, state = 'ready', error = NULL, updated_at = unixepoch()",
            params![id, caption.trim(), tags_json],
        )
        .map_err(err)?;
        n += 1;
    }
    // готовые ролики из папки: у них нет черновика, поэтому отдельный список
    let readies: Vec<i64> = {
        let mut st = db
            .prepare(
                "SELECT r.id FROM ready_videos r
                 WHERE r.project_id = ?1
                   AND NOT EXISTS (SELECT 1 FROM publish_jobs j WHERE j.ready_id = r.id AND j.state IN ('queued', 'sending', 'done', 'needs_check'))
                 ORDER BY r.id",
            )
            .map_err(err)?;
        let rows: Vec<i64> = st.query_map([project_id], |r| r.get(0)).map_err(err)?.collect::<rusqlite::Result<_>>().map_err(err)?;
        rows
    };
    for id in readies {
        db.execute(
            "INSERT INTO publish_jobs (ready_id, caption, hashtags, state) VALUES (?1, ?2, ?3, 'ready')
             ON CONFLICT(ready_id) DO UPDATE SET caption = excluded.caption, hashtags = excluded.hashtags, state = 'ready', error = NULL, updated_at = unixepoch()",
            params![id, caption.trim(), tags_json],
        )
        .map_err(err)?;
        n += 1;
    }
    Ok(n)
}

/// Что делать с задачей, которая встала: `retry`, `mark_done` («он вышел») или `remove`.
pub(crate) fn resolve_job(db: &Connection, id: i64, action: &str) -> Result<(), String> {
    let job = job_by_id(db, id).map_err(|_| "Задача не найдена".to_string())?;
    match action {
        "retry" if ["failed", "needs_check"].contains(&job.state.as_str()) => {
            db.execute("UPDATE publish_jobs SET state = 'queued', not_before = 0, error = NULL, updated_at = unixepoch() WHERE id = ?1", [id]).map_err(err)?;
        }
        "mark_done" if ["failed", "needs_check"].contains(&job.state.as_str()) => {
            db.execute("UPDATE publish_jobs SET state = 'done', sent_at = ?2, error = NULL, updated_at = unixepoch() WHERE id = ?1", params![id, now_unix()]).map_err(err)?;
            let account: Option<i64> = db.query_row("SELECT account_id FROM publish_jobs WHERE id = ?1", [id], |r| r.get(0)).ok().flatten();
            // чтобы на карточке ролика было видно, что он выложен, и повторный запуск его не тронул
            let post = match (job.draft_id, job.ready_id) {
                (Some(draft), _) => begin_post(db, draft, "browser", "browser"),
                (None, Some(ready)) => begin_ready_post(db, ready, "browser", "browser"),
                (None, None) => return Err("Задача без ролика".into()),
            };
            if let Ok(post) = post {
                finish_post(db, post, &Ok(("Выложено (отмечено вручную)".to_string(), None)));
                // «он вышел» тоже входит в суточный лимит того аккаунта, с которого шла выкладка
                mark_sent(db, post, account);
            }
        }
        "remove" if job.state != "sending" => {
            db.execute("DELETE FROM publish_jobs WHERE id = ?1", [id]).map_err(err)?;
        }
        "retry" | "mark_done" | "remove" => return Err(format!("Так сделать нельзя, пока задача в состоянии «{}»", job.state)),
        _ => return Err("Неизвестное действие".into()),
    }
    Ok(())
}

/// Готовые к выкладке становятся очередью. Возвращает, сколько задач в очереди.
pub(crate) fn start_jobs(db: &Connection, project_id: i64) -> Result<usize, String> {
    let jobs = jobs_for_project(db, project_id).map_err(err)?;
    for j in jobs.iter().filter(|j| j.state == "ready") {
        validate_job(&j.caption, &j.hashtags).map_err(|e| format!("Описание ролика №{} не подходит: {e}. Исправь его перед запуском", j.id))?;
    }
    db.execute(
        // паузу после прошлой выкладки не сбрасываем: иначе «стоп → старт» выпускает следующий ролик сразу
        "UPDATE publish_jobs SET state = 'queued', error = NULL, updated_at = unixepoch()
         WHERE state = 'ready' AND (
             draft_id IN (SELECT d.id FROM drafts d JOIN moments m ON m.id = d.moment_id WHERE m.project_id = ?1)
             OR ready_id IN (SELECT id FROM ready_videos WHERE project_id = ?1))",
        [project_id],
    )
    .map_err(err)?;
    let queued = jobs_for_project(db, project_id).map_err(err)?.iter().filter(|j| j.state == "queued").count();
    if queued == 0 {
        return Err("Нет роликов с готовым описанием: сначала составь описания".into());
    }
    Ok(queued)
}

/// Приложение закрыли посреди работы. Незавершённая выкладка требует проверки человеком (файл мог уйти),
/// недописанное описание пишется заново, а очередь после перезапуска ждёт нового согласия.
pub(crate) fn recover_interrupted_jobs(db: &Connection) {
    for sql in [
        "UPDATE publish_jobs SET state = 'needs_check', error = 'Выкладка прервана: приложение было закрыто. Проверь в TikTok Studio, вышел ли ролик, прежде чем отправлять ещё раз.' WHERE state = 'sending'",
        "UPDATE publish_jobs SET state = 'failed', error = 'Описание не дописано: приложение было закрыто. Нажми «Составить описания» ещё раз.' WHERE state = 'drafting'",
        "UPDATE publish_jobs SET state = 'ready' WHERE state = 'queued'",
    ] {
        let _ = db.execute(sql, []);
    }
}

// ---------------------------------------------------------------- расписание

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Выложить эту задачу с этого аккаунта прямо сейчас.
    Send { job: i64, account: i64 },
    /// Первая в очереди ждёт паузы до этого времени (unix).
    WaitUntil(i64),
    /// Сейчас вне окна публикации: ждём, когда окно откроется (unix).
    OutsideWindow(i64),
    /// У каждого аккаунта выбран суточный лимит: ждём полуночи (unix).
    LimitReached(i64),
    /// В очереди пусто.
    Idle,
    /// Ни одного включённого аккаунта: выкладывать не с чего.
    NoAccounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedJob {
    pub id: i64,
    pub not_before: i64,
    /// Аккаунт, выбранный вручную; `None` — по кругу.
    pub wanted: Option<i64>,
}

/// Аккаунт в круге и его выкладки за текущие местные сутки (более ранние в счёт не идут).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub id: i64,
    pub sent: Vec<i64>,
}

/// Всё, что нужно планировщику в этот момент. Время и часовой пояс приходят снаружи, поэтому тесты не ждут
/// и не зависят от часов машины; после сна компьютера очередь просто видит новое время.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Сейчас, unix-секунды.
    pub now: i64,
    /// Смещение местного времени от UTC, секунды.
    pub offset: i64,
    /// Окно публикации, минуты от местной полуночи.
    pub window_start: i64,
    pub window_end: i64,
    pub daily_limit: i64,
    /// Включённые аккаунты в порядке круга.
    pub accounts: Vec<Turn>,
    /// Аккаунт последней выкладки: следующим идёт тот, что стоит после него.
    pub last: Option<i64>,
}

impl Plan {
    /// Начало местных суток (unix).
    fn day_start(&self) -> i64 {
        day_start(self.now, self.offset)
    }

    /// Местная минута суток.
    fn minute(&self) -> i64 {
        (self.now + self.offset).rem_euclid(DAY) / 60
    }

    fn in_window(&self) -> bool {
        let (start, end, minute) = (self.window_start, self.window_end, self.minute());
        if start == end {
            true
        } else if start < end {
            (start..end).contains(&minute)
        } else {
            minute >= start || minute < end
        }
    }
}

/// Начало местных суток (unix) для момента `now` при смещении `offset`.
fn day_start(now: i64, offset: i64) -> i64 {
    now - (now + offset).rem_euclid(DAY)
}

/// Смещение местного времени от UTC на момент `now`, секунды.
fn local_offset(now: i64) -> i64 {
    Local.timestamp_opt(now, 0).single().map_or(0, |t| t.offset().local_minus_utc() as i64)
}

/// Что делать очереди сейчас. `queued` — задачи в порядке выкладки. Сначала окно публикации, потом аккаунт, потом пауза.
/// Аккаунт не занят, пока у него не выбран суточный лимит; за выбор аккаунта отвечает `pick_account`.
pub fn next_action(plan: &Plan, queued: &[QueuedJob]) -> Action {
    let Some(first) = queued.first() else { return Action::Idle };
    if plan.accounts.is_empty() {
        return Action::NoAccounts;
    }
    if !plan.in_window() {
        return Action::OutsideWindow(next_open(plan));
    }
    // выбранный вручную аккаунт: ролик ждёт своего аккаунта, а не уходит на другой (кроме отключённого аккаунта)
    let account = match first.wanted.filter(|w| plan.accounts.iter().any(|t| t.id == *w)) {
        Some(wanted) if under_limit(plan, wanted) => wanted,
        Some(_) => return Action::LimitReached((plan.day_start() + DAY).max(first.not_before)),
        None => match pick_account(plan) {
            Some(account) => account,
            None => return Action::LimitReached((plan.day_start() + DAY).max(first.not_before)),
        },
    };
    if first.not_before > plan.now {
        return Action::WaitUntil(first.not_before);
    }
    Action::Send { job: first.id, account }
}

/// У аккаунта за местные сутки ещё нет полного лимита выкладок.
fn under_limit(plan: &Plan, account: i64) -> bool {
    let day = plan.day_start();
    let limit = plan.daily_limit.max(1) as usize;
    plan.accounts.iter().find(|a| a.id == account).is_some_and(|a| a.sent.iter().filter(|t| **t >= day).count() < limit)
}

/// Когда окно откроется в следующий раз: сегодня или завтра.
fn next_open(plan: &Plan) -> i64 {
    let at = plan.day_start() + plan.window_start * 60;
    if at > plan.now { at } else { at + DAY }
}

/// Аккаунт для следующей выкладки: по кругу после выложившего последним, пропуская тех, у кого лимит на сутки выбран.
fn pick_account(plan: &Plan) -> Option<i64> {
    let n = plan.accounts.len();
    let first = plan.last.and_then(|last| plan.accounts.iter().position(|a| a.id == last)).map_or(0, |i| i + 1);
    let (day, limit) = (plan.day_start(), plan.daily_limit.max(1) as usize);
    (0..n)
        .map(|k| &plan.accounts[(first + k) % n])
        .find(|a| a.sent.iter().filter(|t| **t >= day).count() < limit)
        .map(|a| a.id)
}

/// Состояние для планировщика: настройки, включённые аккаунты с выкладками за сутки и аккаунт последней выкладки.
pub(crate) fn make_plan(db: &Connection, now: i64) -> Plan {
    let settings = load_settings(db);
    let offset = local_offset(now);
    let mut accounts: Vec<Turn> = accounts::enabled_ids(db).unwrap_or_default().into_iter().map(|id| Turn { id, sent: vec![] }).collect();
    for (account, at) in accounts::sent_since(db, day_start(now, offset)).unwrap_or_default() {
        if let Some(turn) = accounts.iter_mut().find(|t| t.id == account) {
            turn.sent.push(at);
        }
    }
    Plan {
        now,
        offset,
        window_start: settings.window_start,
        window_end: settings.window_end,
        daily_limit: settings.daily_limit,
        accounts,
        last: accounts::last_sender(db),
    }
}

fn queued_jobs(db: &Connection, project_id: i64) -> Vec<QueuedJob> {
    let Ok(mut st) = db.prepare(
        "SELECT j.id, j.not_before, j.wanted_account_id FROM publish_jobs j
         LEFT JOIN drafts d ON d.id = j.draft_id LEFT JOIN moments m ON m.id = d.moment_id LEFT JOIN ready_videos r ON r.id = j.ready_id
         WHERE COALESCE(m.project_id, r.project_id) = ?1 AND j.state = 'queued' ORDER BY COALESCE(d.part, 1000000), j.id",
    ) else {
        return vec![];
    };
    st.query_map([project_id], |r| Ok(QueuedJob { id: r.get(0)?, not_before: r.get(1)?, wanted: r.get(2)? }))
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- события

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UpdateEvent {
    project_id: i64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TextEvent {
    draft_id: i64,
    message: String,
}

pub(crate) fn changed(app: &AppHandle, project_id: i64) {
    let _ = app.emit("queue-update", UpdateEvent { project_id });
}

// ---------------------------------------------------------------- описания

async fn generate_all(app: &AppHandle, project_id: i64, todo: Vec<Todo>, cancel: watch::Receiver<bool>) {
    let state = app.state::<AppState>();
    let models = ModelPaths::in_dir(&state.models_dir());
    let opt = Options::default();
    let mut gpu = opt.llm_gpu_layers;
    let fail_rest = |from: usize, message: &str| {
        let db = state.db.0.lock().unwrap();
        for t in &todo[from..] {
            store_text_failed(&db, t.draft_id, message);
        }
    };

    let started = match LlmClient::start(&models.llm, opt.threads, gpu).await {
        Err(_) if gpu > 0 => {
            gpu = 0;
            LlmClient::start(&models.llm, opt.threads, 0).await
        }
        r => r,
    };
    let mut client = match started {
        Ok(c) => Some(c),
        Err(e) => {
            fail_rest(0, &format!("Языковая модель не запустилась: {e:#}"));
            return;
        }
    };

    // сходство проверяем и с описаниями, которые уже есть у проекта
    let mut prior: Vec<String> = jobs_for_project(&state.db.0.lock().unwrap(), project_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|j| ["ready", "queued", "sending", "done"].contains(&j.state.as_str()) && !j.caption.is_empty())
        .map(|j| j.caption)
        .collect();

    for (i, t) in todo.iter().enumerate() {
        if *cancel.borrow() {
            fail_rest(i, "Остановлено: описание не написано. Нажми «Составить описания» ещё раз.");
            break;
        }
        let mut seed = 1000u32.wrapping_add((t.draft_id as u32).wrapping_mul(31));
        let (mut tries, mut fell_back) = (0, false);
        let outcome: Result<describe::Description, String> = loop {
            let Some(c) = client.as_mut() else { break Err("Языковая модель остановилась".into()) };
            let (app2, did) = (app.clone(), t.draft_id);
            let r = describe::describe(c, &t.title, &t.transcript, seed, &mut move |m| {
                let _ = app2.emit("queue-text", TextEvent { draft_id: did, message: m.to_string() });
            })
            .await;
            match r {
                Ok(d) => {
                    // ролики из одного видео похожи, и модель с одним зерном может написать почти то же самое
                    if tries < 2 && prior.iter().any(|p| describe::similarity(p, &d.caption) > 0.5) {
                        tries += 1;
                        seed = seed.wrapping_add(101);
                        continue;
                    }
                    break Ok(d);
                }
                Err(e) => {
                    // видеокарта могла не потянуть запрос, а воркер мог упасть: поднимаем заново, сперва на процессоре
                    if let Some(c) = client.take() {
                        c.shutdown().await;
                    }
                    if gpu > 0 && !fell_back {
                        fell_back = true;
                        gpu = 0;
                        client = LlmClient::start(&models.llm, opt.threads, 0).await.ok();
                        continue;
                    }
                    client = LlmClient::start(&models.llm, opt.threads, gpu).await.ok();
                    break Err(format!("{e:#}"));
                }
            }
        };
        {
            let db = state.db.0.lock().unwrap();
            match outcome.and_then(|d| validate_generated(&d.caption, &d.hashtags).map(|(tags, _)| (d.caption, tags))) {
                Ok((caption, tags)) => {
                    prior.push(caption.clone());
                    let _ = store_ready(&db, t.draft_id, &caption, &tags);
                }
                Err(e) => store_text_failed(&db, t.draft_id, &e),
            }
        }
        changed(app, project_id);
    }
    if let Some(c) = client {
        c.shutdown().await;
    }
}

#[tauri::command]
pub async fn queue_prepare(app: AppHandle, state: State<'_, AppState>, project_id: i64) -> CmdResult<()> {
    let models = ModelPaths::in_dir(&state.models_dir());
    if !models.llm.is_file() {
        return Err(format!("Языковая модель ещё не скачана ({}). Дождись загрузки моделей или нажми «Скачать модели»", models.llm.display()));
    }
    if llm::find_worker().is_none() {
        return Err("Не найден llm-worker рядом с программой".into());
    }
    let (tx, rx) = watch::channel(false);
    {
        let mut g = state.drafting.lock().unwrap();
        if g.is_some() {
            return Err("Описания уже пишутся".into());
        }
        *g = Some(tx);
    }
    let todo = match create_drafting_jobs(&state.db.0.lock().unwrap(), project_id) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => {
            *state.drafting.lock().unwrap() = None;
            return Err("У всех готовых роликов уже есть описание, либо готовых роликов нет".into());
        }
        Err(e) => {
            *state.drafting.lock().unwrap() = None;
            return Err(err(e));
        }
    };
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        generate_all(&app2, project_id, todo, rx).await;
        *app2.state::<AppState>().drafting.lock().unwrap() = None;
        changed(&app2, project_id);
    });
    changed(&app, project_id);
    Ok(())
}

/// «Свой текст на все»: модель не запускается, одно описание и хэштеги получают все готовые ролики, которым это можно.
#[tauri::command]
pub fn queue_apply_text(app: AppHandle, state: State<'_, AppState>, project_id: i64, caption: String, hashtags: Vec<String>) -> CmdResult<usize> {
    if state.drafting.lock().unwrap().is_some() {
        return Err("Подожди, пока модель допишет описания, или останови её".into());
    }
    let n = apply_text(&state.db.0.lock().unwrap(), project_id, &caption, &hashtags)?;
    if n == 0 {
        return Err("Нет роликов, которым можно назначить текст: все уже в очереди, выложены или ждут проверки".into());
    }
    changed(&app, project_id);
    Ok(n)
}

#[tauri::command]
pub fn queue_prepare_cancel(state: State<'_, AppState>) {
    if let Some(tx) = state.drafting.lock().unwrap().as_ref() {
        let _ = tx.send(true);
    }
}

// ---------------------------------------------------------------- выкладка

fn fail_job(db: &Connection, job_id: i64, message: String) -> String {
    let _ = db.execute("UPDATE publish_jobs SET state = 'failed', error = ?2, updated_at = unixepoch() WHERE id = ?1", params![job_id, message]);
    message
}

/// Файл для пула: копия финала без надписи «Часть N». Собирается заново, только если финал новее копии.
async fn pool_file(app: &AppHandle, draft_id: i64, final_file: &Path) -> anyhow::Result<PathBuf> {
    let state = app.state::<AppState>();
    let out = state.data_dir.join("pool").join(format!("draft_{draft_id}.mp4"));
    if !(out.is_file() && is_newer(&out, final_file)) {
        let reporter = app.clone();
        render_pool_copy(app, draft_id, &out, move |fraction| {
            let _ = reporter.emit("post-progress", PostProgress { draft_id, fraction, message: "Готовлю ролик без надписи «Часть» для пула".into() });
        })
        .await?;
    }
    Ok(out)
}

/// `a` записан не раньше `b`. Если времени не узнать, считаем, что не новее.
fn is_newer(a: &Path, b: &Path) -> bool {
    let at = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    matches!((at(a), at(b)), (Some(x), Some(y)) if x >= y)
}

/// Выкладывает одну задачу с выбранного аккаунта. `Err` — очередь надо остановить; текст ошибки идёт в панель.
async fn send_job(app: &AppHandle, job_id: i64, account: i64) -> Result<(), String> {
    let state = app.state::<AppState>();
    // `draft_id` здесь — ключ ролика: номер черновика, а у готового ролика из папки отрицательный номер готового файла
    let (draft_id, project_id, file, caption, post_id, profile, pool) = {
        let db = state.db.0.lock().unwrap();
        let (draft_id, ready_id, caption, tags_json): (Option<i64>, Option<i64>, String, String) = db
            .query_row(
                "SELECT draft_id, ready_id, caption, hashtags FROM publish_jobs WHERE id = ?1 AND state = 'queued'",
                [job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(|_| "Задача пропала из очереди".to_string())?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let (key, project_id, file, pool) = match (draft_id, ready_id) {
            (Some(draft), _) => {
                let d = get_draft(&db, draft)?;
                let file = match d.final_path.clone().filter(|_| d.status == "final_done") {
                    Some(p) => PathBuf::from(p),
                    None => return Err(fail_job(&db, job_id, "У ролика нет готового файла".into())),
                };
                if !file.is_file() {
                    return Err(fail_job(&db, job_id, format!("Файл не найден: {}", file.display())));
                }
                // два и больше включённых аккаунта — пул: ролик выходит без надписи «Часть N»
                let pool = accounts::enabled_ids(&db).map_err(err)?.len() > 1;
                (draft, d.project_id, file, pool)
            }
            (None, Some(ready)) => {
                let (path, project_id): (String, i64) = db
                    .query_row("SELECT path, project_id FROM ready_videos WHERE id = ?1", [ready], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|_| fail_job(&db, job_id, "Готовый ролик пропал из проекта".into()))?;
                let file = PathBuf::from(path);
                if !file.is_file() {
                    return Err(fail_job(&db, job_id, format!("Файл не найден: {}", file.display())));
                }
                // надпись «Часть N» у готового ролика уже запечена, а перерисовать его без исходника нельзя: пул его не трогает
                (-ready, project_id, file, false)
            }
            (None, None) => return Err(fail_job(&db, job_id, "Задача без ролика".into())),
        };
        if let Err(e) = validate_job(&caption, &tags) {
            return Err(fail_job(&db, job_id, e));
        }
        if state.posting.lock().unwrap().contains(&key) {
            return Err(fail_job(&db, job_id, "Этот ролик уже отправляется другим способом".into()));
        }
        let profile = accounts::profile_dir(&db, &state.data_dir, Some(account)).map_err(|e| fail_job(&db, job_id, e))?;
        db.execute("UPDATE publish_jobs SET state = 'sending', attempts = attempts + 1, account_id = ?2, error = NULL, updated_at = unixepoch() WHERE id = ?1", params![job_id, account]).map_err(err)?;
        let post_id = match (draft_id, ready_id) {
            (Some(draft), _) => begin_post(&db, draft, "browser", "browser"),
            (None, Some(ready)) => begin_ready_post(&db, ready, "browser", "browser"),
            (None, None) => return Err(fail_job(&db, job_id, "Задача без ролика".into())),
        }
        .map_err(err)?;
        (key, project_id, file, make_caption(&caption, &tags, CAPTION_LIMIT), post_id, profile, pool)
    };
    state.posting.lock().unwrap().insert(draft_id);
    changed(app, project_id);

    let result = async {
        let file = if pool { pool_file(app, draft_id, &file).await.map_err(|e| e.context("не собрался ролик без надписи «Часть» для пула"))? } else { file };
        run_browser_post(app, draft_id, BrowserPost { file, caption, auto_publish: true, require_confirmation: true, close_on_success: true, profile }).await
    }
    .await;
    state.posting.lock().unwrap().remove(&draft_id);

    match result {
        Ok(o) if o.confirmed => {
            let detail = "Опубликовано через браузер: TikTok подтвердил".to_string();
            {
                let db = state.db.0.lock().unwrap();
                let now = now_unix();
                finish_post(&db, post_id, &Ok((detail.clone(), None)));
                mark_sent(&db, post_id, Some(account));
                let _ = db.execute("UPDATE publish_jobs SET state = 'done', sent_at = ?2, error = NULL, updated_at = unixepoch() WHERE id = ?1", params![job_id, now]);
                // пауза отсчитывается от выхода этого ролика и сдвигает остальных
                let _ = db.execute("UPDATE publish_jobs SET not_before = ?1 WHERE state = 'queued'", [now + load_settings(&db).pause_min * 60]);
            }
            let _ = app.emit("post-done", PostDone { draft_id, detail });
            changed(app, project_id);
            Ok(())
        }
        other => {
            let (message, after_click) = match other {
                Ok(_) => ("TikTok не подтвердил публикацию. Проверь в TikTok Studio, вышел ли ролик".to_string(), true),
                Err(e) => (format!("{e:#}"), e.downcast_ref::<AfterClick>().is_some()),
            };
            {
                let db = state.db.0.lock().unwrap();
                finish_post(&db, post_id, &Err(message.clone()));
                let new_state = if after_click { "needs_check" } else { "failed" };
                let _ = db.execute("UPDATE publish_jobs SET state = ?2, error = ?3, updated_at = unixepoch() WHERE id = ?1", params![job_id, new_state, message]);
            }
            let _ = app.emit("post-error", PostFailed { draft_id, message: message.clone() });
            changed(app, project_id);
            Err(message)
        }
    }
}

/// Цикл очереди: берёт следующее действие, выполняет или ждёт. Выходит, когда очередь пуста, её остановили или она споткнулась.
async fn run_queue(app: AppHandle, project_id: i64, mut stop: watch::Receiver<bool>) {
    let state = app.state::<AppState>();
    let note: Option<String> = loop {
        if *stop.borrow() {
            break None;
        }
        let now = now_unix();
        let action = {
            let db = state.db.0.lock().unwrap();
            next_action(&make_plan(&db, now), &queued_jobs(&db, project_id))
        };
        match action {
            Action::Idle => break None,
            Action::NoAccounts => break Some("Нет включённых аккаунтов: включи хотя бы один в списке аккаунтов".into()),
            Action::Send { job, account } => {
                if let Err(message) = send_job(&app, job, account).await {
                    break Some(format!("Очередь остановлена: {message}"));
                }
            }
            Action::WaitUntil(ts) | Action::OutsideWindow(ts) | Action::LimitReached(ts) => {
                changed(&app, project_id);
                // просыпаемся не реже чем раз в 30 с: часы могли уйти вперёд (сон компьютера), а настройки — измениться
                let secs = (ts - now).clamp(1, 30) as u64;
                tokio::select! {
                    r = stop.changed() => {
                        if r.is_err() {
                            break None;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
                }
            }
        }
    };
    *state.queue_note.lock().unwrap() = note;
    *state.publisher.lock().unwrap() = None;
    changed(&app, project_id);
}

// ---------------------------------------------------------------- команды

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueDto {
    jobs: Vec<JobDto>,
    settings: QueueSettings,
    /// Пул аккаунтов: круг, выкладки за сутки и основной аккаунт.
    accounts: Vec<accounts::AccountDto>,
    /// Два и больше включённых аккаунта: тогда ролики выходят без надписи «Часть N».
    pool: bool,
    running: bool,
    generating: bool,
    sent_today: i64,
    next_at: Option<i64>,
    note: Option<String>,
    window_min: usize,
    window_max: usize,
    max_tags: usize,
    caption_limit: usize,
    chromium: bool,
}

fn build_state(state: &AppState, project_id: i64) -> CmdResult<QueueDto> {
    let now = now_unix();
    let running = state.publisher.lock().unwrap().is_some();
    let generating = state.drafting.lock().unwrap().is_some();
    let kept_note = state.queue_note.lock().unwrap().clone();
    let db = state.db.0.lock().unwrap();
    let settings = load_settings(&db);
    let jobs = jobs_for_project(&db, project_id).map_err(err)?;
    let plan = make_plan(&db, now);
    let action = next_action(&plan, &queued_jobs(&db, project_id));
    let (next_at, limit_note) = match action {
        Action::WaitUntil(t) => (Some(t), None),
        Action::OutsideWindow(t) => (Some(t), Some("Сейчас вне окна публикации: очередь ждёт, когда окно откроется.".to_string())),
        Action::LimitReached(t) => (Some(t), Some(format!("Суточный лимит на аккаунт ({}) выбран: очередь продолжится с полуночи.", settings.daily_limit))),
        _ => (None, None),
    };
    let pool_accounts = accounts::list(&db, plan.day_start()).map_err(err)?;
    Ok(QueueDto {
        sent_today: pool_accounts.iter().map(|a| a.sent_today).sum(),
        pool: plan.accounts.len() > 1,
        accounts: pool_accounts,
        jobs,
        settings,
        running,
        generating,
        next_at: if running { next_at } else { None },
        note: kept_note.or(if running { limit_note } else { None }),
        window_min: MIN_LEN,
        window_max: MAX_LEN,
        max_tags: MAX_TAGS,
        caption_limit: CAPTION_LIMIT,
        chromium: find_chromium().is_some(),
    })
}

#[tauri::command]
pub fn queue_state(state: State<'_, AppState>, project_id: i64) -> CmdResult<QueueDto> {
    build_state(&state, project_id)
}

#[tauri::command]
pub fn queue_job_update(state: State<'_, AppState>, id: i64, caption: String, hashtags: Vec<String>) -> CmdResult<JobDto> {
    update_job(&state.db.0.lock().unwrap(), id, &caption, &hashtags)
}

/// Выбирает аккаунт для ролика вручную (`None` — снова по кругу). Пока ролик не ушёл в работу.
#[tauri::command]
pub fn queue_job_account(app: AppHandle, state: State<'_, AppState>, id: i64, account: Option<i64>) -> CmdResult<()> {
    let db = state.db.0.lock().unwrap();
    let (job_state, project_id): (String, i64) = db
        .query_row(
            "SELECT j.state, COALESCE(m.project_id, r.project_id) FROM publish_jobs j
             LEFT JOIN drafts d ON d.id = j.draft_id LEFT JOIN moments m ON m.id = d.moment_id LEFT JOIN ready_videos r ON r.id = j.ready_id
             WHERE j.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "Задача не найдена".to_string())?;
    if ["sending", "done", "needs_check"].contains(&job_state.as_str()) {
        return Err("Аккаунт уже не поменять: ролик выкладывается, выложен или ждёт проверки".into());
    }
    if let Some(a) = account {
        let exists: i64 = db.query_row("SELECT COUNT(*) FROM accounts WHERE id = ?1", [a], |r| r.get(0)).map_err(err)?;
        if exists == 0 {
            return Err("Такого аккаунта нет".into());
        }
    }
    db.execute("UPDATE publish_jobs SET wanted_account_id = ?2, updated_at = unixepoch() WHERE id = ?1", params![id, account]).map_err(err)?;
    drop(db);
    changed(&app, project_id);
    Ok(())
}

#[tauri::command]
pub fn queue_job_resolve(app: AppHandle, state: State<'_, AppState>, id: i64, action: String) -> CmdResult<()> {
    let project_id: Option<i64> = {
        let db = state.db.0.lock().unwrap();
        let project_id = db
            .query_row("SELECT m.project_id FROM publish_jobs j JOIN drafts d ON d.id = j.draft_id JOIN moments m ON m.id = d.moment_id WHERE j.id = ?1", [id], |r| r.get(0))
            .ok();
        resolve_job(&db, id, &action)?;
        project_id
    };
    // человек разобрался с задачей: прежнее «очередь остановлена: …» больше не про сейчас
    *state.queue_note.lock().unwrap() = None;
    if let Some(p) = project_id {
        changed(&app, p);
    }
    Ok(())
}

#[tauri::command]
pub fn queue_settings_set(state: State<'_, AppState>, pause_min: i64, daily_limit: i64, window_start: i64, window_end: i64) -> CmdResult<QueueSettings> {
    let db = state.db.0.lock().unwrap();
    kv_set(&db, K_PAUSE, &pause_min.clamp(1, 1440).to_string()).map_err(err)?;
    kv_set(&db, K_LIMIT, &daily_limit.clamp(1, 50).to_string()).map_err(err)?;
    kv_set(&db, K_WINDOW_START, &window_start.clamp(0, DAY_MIN).to_string()).map_err(err)?;
    kv_set(&db, K_WINDOW_END, &window_end.clamp(0, DAY_MIN).to_string()).map_err(err)?;
    Ok(load_settings(&db))
}

#[tauri::command]
pub fn queue_start(app: AppHandle, state: State<'_, AppState>, project_id: i64, confirmed: bool) -> CmdResult<()> {
    // выкладка без присмотра меняет настоящий аккаунт: без явного согласия из окна ничего не стартует
    if !confirmed {
        return Err("Нужно подтверждение запуска".into());
    }
    if find_chromium().is_none() {
        return Err("Не найден Chromium или Chrome: выкладывать нечем".into());
    }
    if state.drafting.lock().unwrap().is_some() {
        return Err("Подожди, пока модель допишет описания".into());
    }
    if state.publisher.lock().unwrap().is_some() {
        return Err("Очередь уже идёт".into());
    }
    let has_accounts = !accounts::enabled_ids(&state.db.0.lock().unwrap()).map_err(err)?.is_empty();
    if !has_accounts {
        return Err("Нет включённых аккаунтов: включи хотя бы один в списке аккаунтов".into());
    }
    start_jobs(&state.db.0.lock().unwrap(), project_id)?;
    let (tx, rx) = watch::channel(false);
    *state.publisher.lock().unwrap() = Some(tx);
    *state.queue_note.lock().unwrap() = None;
    tauri::async_runtime::spawn(run_queue(app.clone(), project_id, rx));
    changed(&app, project_id);
    Ok(())
}

/// Останавливает цикл. Ролик, который выкладывается прямо сейчас, цикл доводит до конца: прервать публикацию на середине нельзя.
#[tauri::command]
pub fn queue_pause(state: State<'_, AppState>) {
    if let Some(tx) = state.publisher.lock().unwrap().as_ref() {
        let _ = tx.send(true);
    }
}

#[tauri::command]
pub fn queue_stop(app: AppHandle, state: State<'_, AppState>, project_id: i64) -> CmdResult<()> {
    queue_pause(state.clone());
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE publish_jobs SET state = 'ready', updated_at = unixepoch() WHERE state = 'queued'
             AND draft_id IN (SELECT d.id FROM drafts d JOIN moments m ON m.id = d.moment_id WHERE m.project_id = ?1)",
            [project_id],
        )
        .map_err(err)?;
    changed(&app, project_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: i64, not_before: i64) -> QueuedJob {
        QueuedJob { id, not_before, wanted: None }
    }

    /// План без окна и с часовым поясом UTC. `accounts` — круг: (аккаунт, его выкладки).
    fn plan(now: i64, accounts: Vec<(i64, Vec<i64>)>, last: Option<i64>, limit: i64) -> Plan {
        Plan {
            now,
            offset: 0,
            window_start: 0,
            window_end: DAY_MIN,
            daily_limit: limit,
            accounts: accounts.into_iter().map(|(id, sent)| Turn { id, sent }).collect(),
            last,
        }
    }

    /// То же окно, что и `plan`, но с заданными границами (минуты от полуночи).
    fn windowed(now: i64, start: i64, end: i64) -> Plan {
        Plan { window_start: start, window_end: end, ..plan(now, vec![(1, vec![])], None, 5) }
    }

    #[test]
    fn an_empty_queue_is_idle_and_no_accounts_is_its_own_answer() {
        assert_eq!(next_action(&plan(1000, vec![(1, vec![])], None, 5), &[]), Action::Idle);
        assert_eq!(next_action(&plan(1000, vec![], None, 5), &[q(7, 0)]), Action::NoAccounts);
    }

    #[test]
    fn a_free_account_sends_the_first_job_in_time() {
        let p = plan(1000, vec![(1, vec![])], None, 5);
        assert_eq!(next_action(&p, &[q(7, 0), q(8, 0)]), Action::Send { job: 7, account: 1 });
        assert_eq!(next_action(&p, &[q(7, 1000)]), Action::Send { job: 7, account: 1 }, "ровно в срок — уже можно");
    }

    #[test]
    fn the_pause_between_videos_is_waited_out() {
        assert_eq!(next_action(&plan(1000, vec![(1, vec![])], Some(2), 5), &[q(7, 1900), q(8, 0)]), Action::WaitUntil(1900));
        assert_eq!(next_action(&plan(1900, vec![(1, vec![])], Some(2), 5), &[q(7, 1900)]), Action::Send { job: 7, account: 1 });
    }

    #[test]
    fn the_videos_go_round_the_pool_and_skip_an_account_at_its_limit() {
        let now = 10 * DAY + 3600;
        // после аккаунта 1 идёт 2
        assert_eq!(next_action(&plan(now, vec![(1, vec![]), (2, vec![]), (3, vec![])], Some(1), 1), &[q(7, 0)]), Action::Send { job: 7, account: 2 });
        // у 2 лимит выбран, очередь переходит к 3
        assert_eq!(next_action(&plan(now, vec![(1, vec![]), (2, vec![now - 60]), (3, vec![])], Some(1), 1), &[q(7, 0)]), Action::Send { job: 7, account: 3 });
        // за последним идёт первый
        assert_eq!(next_action(&plan(now, vec![(1, vec![]), (2, vec![])], Some(2), 1), &[q(7, 0)]), Action::Send { job: 7, account: 1 });
        // без истории, или если последний аккаунт уже не в круге, начинаем с первого
        assert_eq!(next_action(&plan(now, vec![(4, vec![]), (5, vec![])], None, 1), &[q(7, 0)]), Action::Send { job: 7, account: 4 });
        assert_eq!(next_action(&plan(now, vec![(4, vec![]), (5, vec![])], Some(99), 1), &[q(7, 0)]), Action::Send { job: 7, account: 4 });
    }

    #[test]
    fn the_daily_limit_is_per_account_and_waits_for_midnight_when_everyone_is_at_it() {
        let now = 10 * DAY + 12 * 3600;
        let midnight = 11 * DAY;
        assert_eq!(next_action(&plan(now, vec![(1, vec![now - 3000]), (2, vec![now - 2000])], None, 1), &[q(7, 0)]), Action::LimitReached(midnight));
        // лимит 2: у первого две выкладки, у второго одна — второй ещё может
        let uneven = vec![(1, vec![now - 3000, now - 2500]), (2, vec![now - 2000])];
        assert_eq!(next_action(&plan(now, uneven, None, 2), &[q(7, 0)]), Action::Send { job: 7, account: 2 });
        // пауза позже полуночи: ждём её
        assert_eq!(next_action(&plan(now, vec![(1, vec![now - 3000])], None, 1), &[q(7, now + 2 * DAY)]), Action::LimitReached(now + 2 * DAY));
    }

    #[test]
    fn a_day_is_a_calendar_day_so_yesterdays_posts_do_not_count() {
        let now = 10 * DAY + 60;
        // вчера в 23:59 — не в счёт, хотя с той выкладки прошло меньше суток
        assert_eq!(next_action(&plan(now, vec![(1, vec![10 * DAY - 60])], None, 1), &[q(7, 0)]), Action::Send { job: 7, account: 1 });
        // сегодня в 00:00:30 — в счёт
        assert_eq!(next_action(&plan(now, vec![(1, vec![10 * DAY + 30])], None, 1), &[q(7, 0)]), Action::LimitReached(11 * DAY));
    }

    #[test]
    fn midnight_is_the_users_local_midnight() {
        // Москва, UTC+3: местная полночь 10-го дня наступает раньше, чем в UTC
        let offset = 3 * 3600;
        let now = 10 * DAY - offset + 1800; // 00:30 по местному времени
        let p = Plan { now, offset, ..plan(now, vec![(1, vec![10 * DAY - offset - 1800])], None, 1) };
        assert_eq!(next_action(&p, &[q(7, 0)]), Action::Send { job: 7, account: 1 }, "23:30 вчера по местному — не сегодня");
        let p = Plan { accounts: vec![Turn { id: 1, sent: vec![now - 10] }], ..p };
        assert_eq!(next_action(&p, &[q(7, 0)]), Action::LimitReached(11 * DAY - offset), "следующая местная полночь");
    }

    #[test]
    fn the_window_holds_the_queue_outside_it_and_opens_it_on_time() {
        let at = |h: i64| 10 * DAY + h * 3600;
        // окно 10:00–22:00
        assert_eq!(next_action(&windowed(at(12), 600, 1320), &[q(7, 0)]), Action::Send { job: 7, account: 1 });
        assert_eq!(next_action(&windowed(at(10), 600, 1320), &[q(7, 0)]), Action::Send { job: 7, account: 1 }, "ровно в начало — уже можно");
        assert_eq!(next_action(&windowed(at(9), 600, 1320), &[q(7, 0)]), Action::OutsideWindow(at(10)), "до открытия — сегодняшнее открытие");
        assert_eq!(next_action(&windowed(at(22), 600, 1320), &[q(7, 0)]), Action::OutsideWindow(11 * DAY + 10 * 3600), "после закрытия — завтрашнее");
        // одинаковые начало и конец — без ограничения
        assert_eq!(next_action(&windowed(at(3), 600, 600), &[q(7, 0)]), Action::Send { job: 7, account: 1 });
    }

    #[test]
    fn a_window_past_midnight_covers_the_night() {
        let at = |h: i64| 10 * DAY + h * 3600;
        // окно 22:00–06:00
        assert_eq!(next_action(&windowed(at(23), 1320, 360), &[q(7, 0)]), Action::Send { job: 7, account: 1 });
        assert_eq!(next_action(&windowed(at(3), 1320, 360), &[q(7, 0)]), Action::Send { job: 7, account: 1 });
        assert_eq!(next_action(&windowed(at(12), 1320, 360), &[q(7, 0)]), Action::OutsideWindow(at(22)));
    }

    #[test]
    fn after_the_computer_sleeps_the_queue_just_sees_the_new_time() {
        let jobs = [q(1, 5000)];
        assert_eq!(next_action(&plan(1000, vec![(1, vec![])], None, 5), &jobs), Action::WaitUntil(5000));
        assert_eq!(next_action(&plan(900_000, vec![(1, vec![])], None, 5), &jobs), Action::Send { job: 1, account: 1 });
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        for (pid, h) in [(1, "h1"), (2, "h2")] {
            c.execute("INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (?1, '/v', ?2, 'v', 100, 1, 1, 30, 1)", params![pid, h]).unwrap();
        }
        // ролики 1 и 2 проекта 1 (части 2 и 1), ролик 3 — проекта 2; готовы все, кроме 4
        for (id, pid, part, status) in [(1, 1, 2, "final_done"), (2, 1, 1, "final_done"), (3, 2, 1, "final_done"), (4, 1, 3, "ready")] {
            c.execute("INSERT INTO moments (id, project_id, start, \"end\", status, title, transcript) VALUES (?1, ?2, 0, 30, 'accepted', ?3, 'речь')", params![id, pid, format!("Ролик {id}")]).unwrap();
            c.execute("INSERT INTO drafts (id, moment_id, part, status) VALUES (?1, ?1, ?2, ?3)", params![id, part, status]).unwrap();
        }
        c
    }

    fn good_text(extra_tag_len: usize) -> (String, Vec<String>) {
        let tags: Vec<String> = ["юмор", "сериал", "тикток"].iter().map(|s| s.to_string()).collect();
        let tag_part = make_caption("", &tags, usize::MAX);
        let want = 3995usize.saturating_sub(tag_part.encode_utf16().count() + 1 + extra_tag_len);
        ("а".repeat(want), tags)
    }

    #[test]
    fn migration_creates_jobs_that_vanish_with_their_draft() {
        let c = db();
        c.execute("INSERT INTO publish_jobs (draft_id) VALUES (1)", []).unwrap();
        assert!(c.execute("INSERT INTO publish_jobs (draft_id) VALUES (1)", []).is_err(), "одна задача на ролик");
        c.execute("DELETE FROM drafts WHERE id = 1", []).unwrap();
        assert_eq!(c.query_row::<i64, _, _>("SELECT count(*) FROM publish_jobs", [], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn only_finished_unwritten_videos_of_the_project_get_a_drafting_job() {
        let c = db();
        let todo = create_drafting_jobs(&c, 1).unwrap();
        assert_eq!(todo.iter().map(|t| t.draft_id).collect::<Vec<_>>(), vec![2, 1], "по порядку частей, без неготового и чужого проекта");
        assert_eq!((todo[0].title.as_str(), todo[0].transcript.as_str()), ("Ролик 2", "речь"));
        assert!(create_drafting_jobs(&c, 1).unwrap().is_empty(), "повторный вызов ничего не создаёт");
        // упавшее описание пишется заново, готовое не трогаем
        store_ready(&c, 2, "текст", &["#а".into()]).unwrap();
        store_text_failed(&c, 1, "сбой");
        let again = create_drafting_jobs(&c, 1).unwrap();
        assert_eq!(again.iter().map(|t| t.draft_id).collect::<Vec<_>>(), vec![1]);
        let by_draft = |d: i64| jobs_for_project(&c, 1).unwrap().into_iter().find(|j| j.draft_id == Some(d)).unwrap();
        assert_eq!(by_draft(2).state, "ready", "готовое описание не переписывается");
        assert_eq!(by_draft(1).state, "drafting", "упавшее пишется заново");
    }

    #[test]
    fn videos_already_published_by_hand_are_skipped_but_inbox_drafts_are_not() {
        let c = db();
        let p = begin_post(&c, 1, "browser", "browser").unwrap();
        finish_post(&c, p, &Ok(("Нажата кнопка «Опубликовать» в окне TikTok. Проверь, что ролик вышел.".into(), None)));
        let p = begin_post(&c, 2, "api", "inbox").unwrap();
        finish_post(&c, p, &Ok(("Ролик в черновиках TikTok.".into(), None)));
        let todo = create_drafting_jobs(&c, 1).unwrap();
        assert_eq!(todo.iter().map(|t| t.draft_id).collect::<Vec<_>>(), vec![2], "в черновиках — это ещё не выложено");
    }

    #[test]
    fn any_text_must_fit_the_page_and_the_tag_limit() {
        let (text, tags) = good_text(0);
        let (norm, len) = validate_job(&text, &tags).unwrap();
        assert_eq!((norm.len(), len), (3, 3995));
        // свой текст может быть любой длины, лишь бы влезал в страницу
        assert_eq!(validate_job("Коротко", &["#а".to_string()]).unwrap().1, 7 + 1 + 2);
        // подпись = текст + перевод строки + «#а»: 3 лишних символа
        assert_eq!(validate_job(&"а".repeat(CAPTION_LIMIT - 3), &["#а".to_string()]).unwrap().1, CAPTION_LIMIT, "верхняя граница включена");
        for (cap, tags, needle) in [
            ("а".repeat(CAPTION_LIMIT - 2), vec!["#а".to_string()], "не больше 4000"),
            ("   ".to_string(), vec!["#а".to_string()], "пустое"),
            (text.clone(), vec![], "хотя бы один"),
            (text.clone(), (0..6).map(|i| format!("тег{i}")).collect(), "не больше 5"),
        ] {
            let e = validate_job(&cap, &tags).unwrap_err();
            assert!(e.contains(needle), "{e}");
        }
        // повторы и регистр не считаются разными хэштегами: «#а», «а» и «А» — один (в подписи остаётся «#а»)
        assert_eq!(validate_job(&text, &["#а".into(), "а".into(), "А".into()]).unwrap().1, 3973 + 1 + 2);
    }

    #[test]
    fn model_text_must_land_in_the_window() {
        let (text, tags) = good_text(0);
        assert_eq!(validate_generated(&text, &tags).unwrap().1, 3995);
        // подпись = текст + перевод строки + «#а»: 3 лишних символа
        assert_eq!(validate_generated(&"а".repeat(3999 - 3), &["#а".to_string()]).unwrap().1, 3999, "верхняя граница включена");
        assert_eq!(validate_generated(&"а".repeat(3990 - 3), &["#а".to_string()]).unwrap().1, 3990, "нижняя граница включена");
        for (cap, needle) in [("а".repeat(3999 - 2), "нужно 3990–3999"), ("а".repeat(3990 - 4), "нужно 3990–3999"), ("а".repeat(CAPTION_LIMIT), "не больше 4000")] {
            let e = validate_generated(&cap, &["#а".to_string()]).unwrap_err();
            assert!(e.contains(needle), "{e}");
        }
    }

    #[test]
    fn a_hand_edit_is_checked_and_only_allowed_before_the_job_goes_out() {
        let c = db();
        create_drafting_jobs(&c, 1).unwrap();
        let (text, tags) = good_text(0);
        let id = job_by_id(&c, jobs_for_project(&c, 1).unwrap()[0].id).unwrap().id;
        assert!(update_job(&c, id, &text, &tags).unwrap_err().contains("Сейчас править нельзя"), "пока модель пишет — нельзя");
        store_ready(&c, 2, "черновой", &["#а".into()]).unwrap();
        assert!(update_job(&c, id, &"а".repeat(4001), &tags).unwrap_err().contains("не больше 4000"));
        assert!(update_job(&c, id, "  ", &tags).unwrap_err().contains("пустое"));
        assert_eq!(update_job(&c, id, "Свой короткий текст", &tags).unwrap().state, "ready", "свой текст окну не подчиняется");
        let j = update_job(&c, id, &format!("  {text}  "), &tags).unwrap();
        assert_eq!((j.state.as_str(), j.len, j.caption.len() == text.len()), ("ready", 3995, true), "пробелы по краям убраны");
        c.execute("UPDATE publish_jobs SET state = 'queued' WHERE id = ?1", [id]).unwrap();
        assert!(update_job(&c, id, &text, &tags).is_err(), "в очереди — нельзя");
    }

    #[test]
    fn starting_queues_only_ready_jobs_of_the_project_and_refuses_bad_text() {
        let c = db();
        create_drafting_jobs(&c, 1).unwrap();
        create_drafting_jobs(&c, 2).unwrap();
        assert!(start_jobs(&c, 1).unwrap_err().contains("Нет роликов с готовым описанием"));
        let (text, tags) = good_text(0);
        for d in [1, 2, 3] {
            store_ready(&c, d, &text, &normalize_hashtags(&tags)).unwrap();
        }
        assert_eq!(start_jobs(&c, 1).unwrap(), 2);
        let states: Vec<_> = jobs_for_project(&c, 1).unwrap().iter().map(|j| (j.draft_id.unwrap(), j.state.clone())).collect();
        assert_eq!(states, vec![(2, "queued".into()), (1, "queued".into())], "в порядке частей");
        assert_eq!(jobs_for_project(&c, 2).unwrap()[0].state, "ready", "чужой проект не тронут");
        assert_eq!(queued_jobs(&c, 1).iter().map(|j| j.id).collect::<Vec<_>>().len(), 2);

        // испорченный текст в базе не уйдёт в очередь
        let c = db();
        create_drafting_jobs(&c, 1).unwrap();
        store_ready(&c, 1, "", &["#а".into()]).unwrap();
        assert!(start_jobs(&c, 1).unwrap_err().contains("не подходит"));
    }

    #[test]
    fn one_text_goes_to_every_finished_video_that_can_take_it() {
        let c = db();
        let tags = vec!["юмор".to_string(), "#сериал".to_string()];
        assert_eq!(apply_text(&c, 1, "Один текст на всех", &tags).unwrap(), 2, "готовые ролики проекта 1: 2 и 1, неготовый 4 и чужой 3 не берём");
        let jobs = jobs_for_project(&c, 1).unwrap();
        assert_eq!(jobs.iter().map(|j| (j.draft_id.unwrap(), j.state.as_str())).collect::<Vec<_>>(), vec![(2, "ready"), (1, "ready")], "в порядке частей");
        assert!(jobs.iter().all(|j| j.caption == "Один текст на всех" && j.hashtags == ["#юмор", "#сериал"]));
        assert!(jobs_for_project(&c, 2).unwrap().is_empty(), "другой проект не тронут");
        assert_eq!(jobs[0].len, "Один текст на всех\n#юмор #сериал".chars().count());

        // новый текст заменяет прежний у готовых и упавших; идущие в работу и выложенные не трогаем
        let by_draft = |d: i64| jobs_for_project(&c, 1).unwrap().into_iter().find(|j| j.draft_id == Some(d)).unwrap();
        c.execute("UPDATE publish_jobs SET state = 'queued' WHERE draft_id = 1", []).unwrap();
        assert_eq!(apply_text(&c, 1, "Новый текст", &tags).unwrap(), 1);
        assert_eq!((by_draft(2).caption.as_str(), by_draft(2).state.as_str()), ("Новый текст", "ready"));
        assert_eq!((by_draft(1).caption.as_str(), by_draft(1).state.as_str()), ("Один текст на всех", "queued"), "в очереди остаётся со своим текстом");
        for state in ["sending", "done", "needs_check", "drafting"] {
            c.execute("UPDATE publish_jobs SET state = ?1 WHERE draft_id = 2", [state]).unwrap();
            c.execute("UPDATE publish_jobs SET state = 'queued' WHERE draft_id = 1", []).unwrap();
            assert_eq!(apply_text(&c, 1, "Ещё текст", &tags).unwrap(), 0, "ролик в состоянии «{state}» не трогаем");
            assert_eq!(by_draft(2).caption, "Новый текст");
        }
        c.execute("UPDATE publish_jobs SET state = 'failed', error = 'сбой' WHERE draft_id = 2", []).unwrap();
        assert_eq!(apply_text(&c, 1, "После сбоя", &tags).unwrap(), 1);
        assert_eq!((by_draft(2).state.as_str(), by_draft(2).error), ("ready", None), "упавший снова готов, ошибка сброшена");
    }

    #[test]
    fn the_shared_text_follows_the_same_rules_and_changes_nothing_when_it_is_bad() {
        let c = db();
        for (cap, tags, needle) in [
            ("".to_string(), vec!["а".to_string()], "пустое"),
            ("Текст".to_string(), vec![], "хотя бы один"),
            ("Текст".to_string(), (0..6).map(|i| format!("тег{i}")).collect(), "не больше 5"),
            ("а".repeat(4000), vec!["а".to_string()], "не больше 4000"),
        ] {
            let e = apply_text(&c, 1, &cap, &tags).unwrap_err();
            assert!(e.contains(needle), "{e}");
        }
        assert!(jobs_for_project(&c, 1).unwrap().is_empty(), "после отказа задач не появилось");
    }

    #[test]
    fn videos_published_by_hand_do_not_get_the_shared_text() {
        let c = db();
        let p = begin_post(&c, 1, "browser", "browser").unwrap();
        finish_post(&c, p, &Ok(("Нажата кнопка «Опубликовать» в окне TikTok.".into(), None)));
        assert_eq!(apply_text(&c, 1, "Текст", &["а".to_string()]).unwrap(), 1);
        assert_eq!(jobs_for_project(&c, 1).unwrap().iter().map(|j| j.draft_id.unwrap()).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn resolving_a_stuck_job_retries_marks_it_done_or_removes_it() {
        let c = db();
        create_drafting_jobs(&c, 1).unwrap();
        let ids: Vec<i64> = jobs_for_project(&c, 1).unwrap().iter().map(|j| j.id).collect();
        c.execute("UPDATE publish_jobs SET state = 'needs_check', error = 'проверь' WHERE id = ?1", [ids[0]]).unwrap();
        c.execute("UPDATE publish_jobs SET state = 'failed', error = 'сбой' WHERE id = ?1", [ids[1]]).unwrap();

        resolve_job(&c, ids[0], "retry").unwrap();
        let j = job_by_id(&c, ids[0]).unwrap();
        assert_eq!((j.state.as_str(), j.error, j.not_before), ("queued", None, 0));

        resolve_job(&c, ids[1], "mark_done").unwrap();
        assert_eq!(job_by_id(&c, ids[1]).unwrap().state, "done");
        assert_eq!(make_plan(&c, now_unix()).accounts[0].sent.len(), 1, "«он вышел» входит в суточный счёт основного аккаунта");
        let posts = latest_posts(&c, 1).unwrap();
        assert!(posts.iter().any(|p| p.draft_id == 1 && p.status == "done" && was_published(p)), "{posts:?}");

        assert!(resolve_job(&c, ids[0], "mark_done").is_err(), "из очереди так нельзя");
        assert!(resolve_job(&c, ids[0], "что-то").is_err());
        c.execute("UPDATE publish_jobs SET state = 'sending' WHERE id = ?1", [ids[0]]).unwrap();
        assert!(resolve_job(&c, ids[0], "remove").is_err(), "выкладываемую убрать нельзя");
        c.execute("UPDATE publish_jobs SET state = 'failed' WHERE id = ?1", [ids[0]]).unwrap();
        resolve_job(&c, ids[0], "remove").unwrap();
        assert_eq!(jobs_for_project(&c, 1).unwrap().len(), 1);
    }

    #[test]
    fn after_a_restart_interrupted_work_asks_for_a_human_and_the_queue_waits_for_consent() {
        let c = db();
        create_drafting_jobs(&c, 1).unwrap();
        create_drafting_jobs(&c, 2).unwrap();
        let ids: Vec<i64> = jobs_for_project(&c, 1).unwrap().iter().map(|j| j.id).collect();
        c.execute("UPDATE publish_jobs SET state = 'sending' WHERE id = ?1", [ids[0]]).unwrap();
        c.execute("UPDATE publish_jobs SET state = 'queued' WHERE id = ?1", [ids[1]]).unwrap();
        recover_interrupted_jobs(&c);
        let a = job_by_id(&c, ids[0]).unwrap();
        assert_eq!(a.state, "needs_check");
        assert!(a.error.unwrap().contains("Проверь в TikTok Studio"));
        assert_eq!(job_by_id(&c, ids[1]).unwrap().state, "ready", "очередь без присмотра сама не стартует");
        let other = &jobs_for_project(&c, 2).unwrap()[0];
        assert_eq!(other.state, "failed", "недописанное описание пишется заново");
        assert!(other.error.as_deref().unwrap().contains("Описание не дописано"));
    }

    #[test]
    fn jobs_are_listed_by_part_and_report_their_caption_length() {
        let c = db();
        create_drafting_jobs(&c, 1).unwrap();
        let (text, tags) = good_text(0);
        store_ready(&c, 1, &text, &normalize_hashtags(&tags)).unwrap();
        let jobs = jobs_for_project(&c, 1).unwrap();
        assert_eq!(jobs.iter().map(|j| j.draft_id.unwrap()).collect::<Vec<_>>(), vec![2, 1]);
        let j = jobs.iter().find(|j| j.draft_id == Some(1)).unwrap();
        assert_eq!(j.len, 3995);
        assert_eq!(j.len, validate_job(&j.caption, &j.hashtags).unwrap().1);
        assert_eq!(jobs.iter().find(|j| j.draft_id == Some(2)).unwrap().len, 0);
    }

    #[test]
    fn settings_default_to_a_cautious_pace_and_clamp_garbage() {
        let c = db();
        assert_eq!(load_settings(&c), QueueSettings { pause_min: 45, daily_limit: 5, window_start: 0, window_end: DAY_MIN });
        kv_set(&c, K_PAUSE, "0").unwrap();
        kv_set(&c, K_LIMIT, "9999").unwrap();
        kv_set(&c, K_WINDOW_START, "-5").unwrap();
        kv_set(&c, K_WINDOW_END, "99999").unwrap();
        assert_eq!(load_settings(&c), QueueSettings { pause_min: 1, daily_limit: 50, window_start: 0, window_end: DAY_MIN });
        kv_set(&c, K_PAUSE, "не число").unwrap();
        assert_eq!(load_settings(&c).pause_min, 45);
    }

    fn with_ready_video(c: &Connection, id: i64, project: i64) {
        c.execute("INSERT INTO ready_videos (id, project_id, path, name) VALUES (?1, ?2, ?3, ?4)", params![id, project, format!("/г/{id}.mp4"), format!("{id}.mp4")]).unwrap();
        c.execute("INSERT INTO publish_jobs (ready_id, state) VALUES (?1, 'drafting')", [id]).unwrap();
    }

    #[test]
    fn a_chosen_account_takes_its_video_even_when_the_circle_points_elsewhere() {
        // по кругу следующим идёт аккаунт 2, а ролик выбрал аккаунт 1
        let p = plan(1000, vec![(1, vec![]), (2, vec![])], Some(1), 5);
        assert_eq!(next_action(&p, &[QueuedJob { id: 7, not_before: 0, wanted: Some(1) }]), Action::Send { job: 7, account: 1 });
    }

    #[test]
    fn a_video_whose_chosen_account_is_at_its_limit_waits_instead_of_moving_to_another_account() {
        let p = plan(1000, vec![(1, vec![999]), (2, vec![])], None, 1);
        let queue = [QueuedJob { id: 7, not_before: 0, wanted: Some(1) }, q(8, 0)];
        assert!(matches!(next_action(&p, &queue), Action::LimitReached(_)), "ролик 7 ждёт аккаунт 1, ролик 8 за ним");
    }

    #[test]
    fn a_chosen_account_that_is_switched_off_falls_back_to_the_circle() {
        let p = plan(1000, vec![(2, vec![])], None, 5);
        assert_eq!(next_action(&p, &[QueuedJob { id: 7, not_before: 0, wanted: Some(9) }]), Action::Send { job: 7, account: 2 });
    }

    #[test]
    fn a_ready_video_has_a_job_without_a_draft_and_shows_its_file_name() {
        let c = db();
        with_ready_video(&c, 1, 1);
        let jobs = jobs_for_project(&c, 1).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].draft_id, jobs[0].ready_id, jobs[0].name.as_str()), (None, Some(1), "1.mp4"));
        assert!(jobs_for_project(&c, 2).unwrap().is_empty(), "чужой проект ролик не видит");
    }

    #[test]
    fn a_ready_video_takes_the_shared_text_and_then_joins_the_queue() {
        let c = db();
        with_ready_video(&c, 1, 1);
        let (caption, tags) = good_text(0);
        // два готовых финала проекта 1 (черновик 4 ещё не готов, черновик 3 — чужой проект) и готовый ролик
        assert_eq!(apply_text(&c, 1, &caption, &tags).unwrap(), 3);
        let job = jobs_for_project(&c, 1).unwrap().into_iter().find(|j| j.ready_id == Some(1)).unwrap();
        assert_eq!(job.state, "ready");
        assert_eq!(start_jobs(&c, 1).unwrap(), 3, "в очередь ушли и два финала, и готовый ролик");
    }

    #[test]
    fn a_job_must_point_at_a_draft_or_a_ready_video() {
        let c = db();
        assert!(c.execute("INSERT INTO publish_jobs (state) VALUES ('ready')", []).is_err());
    }
}
