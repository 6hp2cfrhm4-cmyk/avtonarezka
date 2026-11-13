//! Пачка: ссылки и файлы по очереди. Скачать (ссылка) → добавить в проекты → нарезать (как одно видео)
//! → черновики → ждём одобрения человека → финал. Уже нарезанный файл (`precut`) идёт без поиска моментов.
//! Ошибка одного элемента не останавливает пачку. Одобрение дальше подхватывает раннер сам.

use std::{collections::HashMap, time::Duration};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::{sync::watch, task::JoinHandle};

use crate::{
    analysis_cmds, analyze::pipeline::Options, commands::{self, Project}, drafts, state::AppState,
    youtube::{select, url},
    youtube_cmds::{self, StartArgs},
};

type CmdResult<T> = Result<T, String>;

const QUEUED: &str = "queued";
const DOWNLOADING: &str = "downloading";
const IMPORTING: &str = "importing";
const ANALYZING: &str = "analyzing";
const DRAFTING: &str = "drafting";
const AWAITING: &str = "awaiting_approval";
const RENDERING: &str = "rendering";
const DONE: &str = "done";
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
}

fn default_preset() -> String {
    "1080".into()
}

// ---------------------------------------------------------------- база

pub(crate) fn add(db: &Connection, items: &[NewItem]) -> rusqlite::Result<usize> {
    let next: i64 = db.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM batch_items", [], |r| r.get(0))?;
    for (i, it) in items.iter().enumerate() {
        db.execute(
            "INSERT INTO batch_items (position, kind, source, title, preset, precut, stage) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![next + i as i64, it.kind, it.source.trim(), it.title.trim(), it.preset, it.precut as i64, QUEUED],
        )?;
    }
    Ok(items.len())
}

pub(crate) fn list(db: &Connection) -> rusqlite::Result<Vec<ItemDto>> {
    let mut st = db.prepare("SELECT id, kind, source, title, precut, stage, error, project_id FROM batch_items ORDER BY position, id")?;
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
        })
    })?;
    rows.collect()
}

fn set_stage(db: &Connection, id: i64, stage: &str, error: Option<&str>) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE batch_items SET stage = ?2, error = ?3, updated_at = unixepoch() WHERE id = ?1",
        params![id, stage, error],
    )?;
    Ok(())
}

/// Перезапуск приложения прерывает то, что раннер делал сам. Ожидание одобрения и финал сохраняются.
pub(crate) fn recover_interrupted(db: &Connection) {
    let _ = db.execute(
        "UPDATE batch_items SET stage = 'failed', error = 'Прервано: приложение было закрыто. Нажми «Повторить»', updated_at = unixepoch()
         WHERE stage IN ('downloading', 'importing', 'analyzing', 'drafting')",
        [],
    );
}

/// Следующий элемент, который раннер должен взять: самый ранний из ждущих или уже скачивающихся.
fn next_item(db: &Connection) -> rusqlite::Result<Option<(i64, NewItem, Option<i64>)>> {
    let mut st = db.prepare(
        "SELECT id, kind, source, title, preset, precut, project_id FROM batch_items
         WHERE stage IN ('queued', 'downloading') ORDER BY position, id LIMIT 1",
    )?;
    let mut rows = st.query([])?;
    let Some(r) = rows.next()? else { return Ok(None) };
    let item = NewItem { kind: r.get(1)?, source: r.get(2)?, title: r.get(3)?, preset: r.get(4)?, precut: r.get::<_, i64>(5)? != 0 };
    Ok(Some((r.get(0)?, item, r.get(6)?)))
}

/// Элементы, которые ждут одобрения или уже рендерят финал: раннер не выходит, пока они есть.
fn pending(db: &Connection) -> rusqlite::Result<Vec<(i64, i64)>> {
    let mut st = db.prepare("SELECT id, project_id FROM batch_items WHERE stage IN ('awaiting_approval', 'rendering') AND project_id IS NOT NULL")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

fn has_queued(db: &Connection) -> rusqlite::Result<bool> {
    db.query_row("SELECT EXISTS (SELECT 1 FROM batch_items WHERE stage IN ('queued', 'downloading'))", [], |r| r.get(0))
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
    let (tx, rx) = watch::channel(false);
    {
        let mut stop = state.batch.stop.lock().unwrap();
        if stop.is_some() {
            return;
        }
        *stop = Some(tx);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        run(app.clone(), rx).await;
        let _ = app.emit("batch-changed", ());
    });
}

/// После перезапуска: если остались элементы, которые ждут одобрения или рендерятся, раннер снова следит за ними.
pub(crate) fn resume(app: &AppHandle) {
    let waiting = {
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        pending(&db).map(|p| !p.is_empty()).unwrap_or(false)
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
        settle(&app);
        let next = {
            let state = app.state::<AppState>();
            let db = state.db.0.lock().unwrap();
            next_item(&db).ok().flatten()
        };
        match next {
            Some((id, item, project_id)) => process(&app, id, item, project_id, &mut prefetch).await,
            None => {
                // больше нечего скачивать: выходим, если никто не ждёт одобрения; иначе смотрим за ним
                let state = app.state::<AppState>();
                let mut guard = state.batch.stop.lock().unwrap();
                let idle = {
                    let db = state.db.0.lock().unwrap();
                    pending(&db).map(|p| p.is_empty()).unwrap_or(true) && !has_queued(&db).unwrap_or(false)
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
fn settle(app: &AppHandle) {
    let state = app.state::<AppState>();
    let waiting = match pending(&state.db.0.lock().unwrap()) {
        Ok(w) => w,
        Err(_) => return,
    };
    for (id, project_id) in waiting {
        let step = {
            let db = state.db.0.lock().unwrap();
            match tally(&db, project_id) {
                Ok(t) => step_after(&t),
                Err(_) => continue,
            }
        };
        match step {
            Step::Wait => {}
            Step::StartFinals => match drafts::start_finals(app, project_id) {
                Ok(_) => update(app, id, RENDERING, None),
                Err(e) => update(app, id, FAILED, Some(&e)),
            },
            Step::Finish => update(app, id, DONE, None),
        }
    }
}

fn update(app: &AppHandle, id: i64, stage: &str, error: Option<&str>) {
    let state = app.state::<AppState>();
    let _ = set_stage(&state.db.0.lock().unwrap(), id, stage, error);
    let _ = app.emit("batch-changed", ());
}

fn set_project(app: &AppHandle, id: i64, project: &Project) {
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    // название берём у видео, если пользователь его не вписал
    let _ = db.execute(
        "UPDATE batch_items SET project_id = ?2, title = CASE WHEN title = '' THEN ?3 ELSE title END, updated_at = unixepoch() WHERE id = ?1",
        params![id, project.id, project.title],
    );
    drop(db);
    let _ = app.emit("batch-changed", ());
}

/// Делает один элемент: скачать или импортировать → нарезать (или полный кадр) → черновики.
async fn process(app: &AppHandle, id: i64, item: NewItem, project_id: Option<i64>, prefetch: &mut HashMap<i64, JoinHandle<CmdResult<Project>>>) {
    let outcome = async {
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
                set_project(app, id, &project);
                project.id
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
                analyze(app, project).await?;
            }
        }

        update(app, id, DRAFTING, None);
        drafts::generate_and_render(app, project)?;
        update(app, id, AWAITING, None);
        Ok::<(), String>(())
    }
    .await;

    if let Err(message) = outcome {
        let stopped = app.state::<AppState>().batch.stop.lock().unwrap().as_ref().is_some_and(|tx| *tx.borrow());
        let message = if stopped { "Остановлено".to_string() } else { message };
        update(app, id, FAILED, Some(&message));
    }
}

/// Анализ уже был (хоть один момент). Удаление моментов каскадом стирает черновики, поэтому повтор его не запускает.
fn has_moments(db: &Connection, project_id: i64) -> CmdResult<bool> {
    db.query_row("SELECT EXISTS (SELECT 1 FROM moments WHERE project_id = ?1)", [project_id], |r| r.get(0)).map_err(|e| e.to_string())
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
async fn download(app: &AppHandle, id: i64, item: &NewItem) -> CmdResult<Project> {
    let args = StartArgs { url: item.source.clone(), preset: item.preset.clone(), title: item.title.clone(), channel: String::new(), thumb: None };
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
            "SELECT id, kind, source, title, preset, precut FROM batch_items WHERE stage = 'queued' AND kind = 'link' AND project_id IS NULL ORDER BY position, id LIMIT 1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, NewItem { kind: r.get(1)?, source: r.get(2)?, title: r.get(3)?, preset: r.get(4)?, precut: r.get::<_, i64>(5)? != 0 })),
        )
        .ok()
    };
    let Some((id, item)) = next else { return };
    update(app, id, DOWNLOADING, None);
    let app = app.clone();
    prefetch.insert(id, tokio::spawn(async move { download(&app, id, &item).await }));
}

/// Анализ точно как у одного видео: те же настройки по умолчанию, что у кнопки «Анализ». Все найденные моменты принимаются.
async fn analyze(app: &AppHandle, project_id: i64) -> CmdResult<()> {
    let (input, rx) = {
        let state = app.state::<AppState>();
        analysis_cmds::begin(&state, project_id)?
    };
    *app.state::<AppState>().batch.analysis.lock().unwrap() = Some(project_id);
    let opt: Options = analysis_cmds::UiOptions::default().into_options();
    let result = analysis_cmds::execute(app.clone(), input, rx, opt).await;
    *app.state::<AppState>().batch.analysis.lock().unwrap() = None;
    result?;
    let state = app.state::<AppState>();
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE moments SET status = 'accepted' WHERE project_id = ?1 AND status = 'candidate'", [project_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------- команды

#[tauri::command]
pub fn batch_list(state: State<'_, AppState>) -> CmdResult<Vec<ItemDto>> {
    list(&state.db.0.lock().unwrap()).map_err(|e| e.to_string())
}

/// Добавляет ссылки и файлы в конец пачки. Проверяем сразу, чтобы ошибка была видна на месте, а не через час.
#[tauri::command]
pub fn batch_add(app: AppHandle, state: State<'_, AppState>, items: Vec<NewItem>) -> CmdResult<usize> {
    if items.is_empty() {
        return Err("Нет ни одной ссылки или файла".into());
    }
    for it in &items {
        match it.kind.as_str() {
            "link" => {
                url::extract_video_id(&it.source).map_err(|e| format!("«{}»: {e}", it.source))?;
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
    Ok(n)
}

#[tauri::command]
pub fn batch_start(app: AppHandle) {
    start(&app);
}

/// Останавливает пачку: текущая загрузка или анализ прерываются, остальное остаётся в очереди.
#[tauri::command]
pub fn batch_cancel(state: State<'_, AppState>) {
    if let Some(tx) = state.batch.stop.lock().unwrap().as_ref() {
        let _ = tx.send(true);
    }
    for job in state.batch.jobs.lock().unwrap().values() {
        youtube_cmds::cancel_job(&state, job);
    }
    if let Some(pid) = *state.batch.analysis.lock().unwrap() {
        if let Some(tx) = state.analyses.lock().unwrap().get(&pid) {
            let _ = tx.send(true);
        }
    }
}

/// Убирает элемент, который не сейчас в работе.
#[tauri::command]
pub fn batch_remove(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let n = state
        .db
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM batch_items WHERE id = ?1 AND stage NOT IN ('downloading', 'importing', 'analyzing', 'drafting')", [id])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("Этот элемент сейчас в работе: сначала останови пачку".into());
    }
    let _ = app.emit("batch-changed", ());
    Ok(())
}

/// Ставит упавший элемент обратно в очередь. Уже добавленное видео не добавляется второй раз.
#[tauri::command]
pub fn batch_retry(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE batch_items SET stage = 'queued', error = NULL, updated_at = unixepoch() WHERE id = ?1 AND stage = 'failed'", [id])
        .map_err(|e| e.to_string())?;
    start(&app);
    let _ = app.emit("batch-changed", ());
    Ok(())
}

/// Одобряет готовые черновики всех роликов пачки. Финал запустится сам, раннер это заметит.
#[tauri::command]
pub fn batch_approve_ready(app: AppHandle, state: State<'_, AppState>) -> CmdResult<usize> {
    let db = state.db.0.lock().unwrap();
    let projects: Vec<i64> = pending(&db).map_err(|e| e.to_string())?.into_iter().map(|(_, p)| p).collect();
    let mut n = 0;
    for pid in projects {
        n += drafts::approve_ready(&db, pid).map_err(|e| e.to_string())?;
    }
    drop(db);
    let _ = app.emit("batch-changed", ());
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

    fn link(source: &str) -> NewItem {
        NewItem { kind: "link".into(), source: source.into(), title: String::new(), preset: "1080".into(), precut: false }
    }

    #[test]
    fn items_keep_their_order_and_the_runner_takes_the_earliest_waiting_one() {
        let c = db();
        add(&c, &[link("a"), link("b")]).unwrap();
        add(&c, &[link("c")]).unwrap();
        let (id, item, _) = next_item(&c).unwrap().unwrap();
        assert_eq!((id, item.source.as_str()), (1, "a"));
        set_stage(&c, 1, DONE, None).unwrap();
        assert_eq!(next_item(&c).unwrap().unwrap().1.source, "b");
    }

    #[test]
    fn a_draft_that_is_ready_waits_for_a_human_and_an_approved_one_starts_the_final() {
        assert_eq!(step_after(&Tally { ready: 2, ..Default::default() }), Step::Wait);
        assert_eq!(step_after(&Tally { ready: 1, approved: 1, ..Default::default() }), Step::StartFinals);
        assert_eq!(step_after(&Tally { busy: 1, ..Default::default() }), Step::Wait);
    }

    #[test]
    fn the_item_closes_only_when_no_draft_is_left_waiting_rendering_or_broken() {
        assert_eq!(step_after(&Tally::default()), Step::Finish);
        assert_eq!(step_after(&Tally { failed: 3, ..Default::default() }), Step::Wait, "упавшие ждут починки или отклонения");
    }

    #[test]
    fn tally_counts_each_draft_state_once_and_only_for_its_project() {
        let c = db();
        c.execute("INSERT INTO moments (id, project_id, start, \"end\", status) VALUES (1, 1, 0, 10, 'accepted'), (2, 1, 10, 20, 'accepted')", []).unwrap();
        c.execute("INSERT INTO drafts (moment_id, status) VALUES (1, 'ready'), (2, 'final_failed')", []).unwrap();
        assert_eq!(tally(&c, 1).unwrap(), Tally { ready: 1, approved: 0, busy: 0, failed: 1 });
        assert_eq!(tally(&c, 2).unwrap(), Tally::default());
    }

    #[test]
    fn a_restart_turns_interrupted_steps_into_failures_but_keeps_the_waiting_ones() {
        let c = db();
        add(&c, &[link("a"), link("b"), link("c")]).unwrap();
        set_stage(&c, 1, DOWNLOADING, None).unwrap();
        set_stage(&c, 2, AWAITING, None).unwrap();
        set_stage(&c, 3, RENDERING, None).unwrap();
        recover_interrupted(&c);
        let stages: Vec<String> = list(&c).unwrap().into_iter().map(|i| i.stage).collect();
        assert_eq!(stages, vec![FAILED, AWAITING, RENDERING]);
    }

    #[test]
    fn a_precut_item_keeps_its_flag_and_its_project_once_known() {
        let c = db();
        add(&c, &[NewItem { kind: "file".into(), source: "/x.mp4".into(), title: String::new(), preset: "1080".into(), precut: true }]).unwrap();
        c.execute("UPDATE batch_items SET project_id = 1 WHERE id = 1", []).unwrap();
        let item = list(&c).unwrap().remove(0);
        assert!(item.precut);
        assert_eq!(item.project_id, Some(1));
    }
}
