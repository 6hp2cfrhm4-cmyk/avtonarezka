//! Команды анализа и отбора моментов (этап 1).

use std::sync::Arc;

use crate::host::{AppHandle, Emitter, Manager, State};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{
    analyze::{
        llm,
        pipeline::{self, Analysis, Options, Progress},
        score, Sentence, Signals,
    },
    models::ModelPaths,
    state::AppState,
};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
pub struct ModelsStatus {
    dir: String,
    whisper: bool,
    yamnet: bool,
    llm: bool,
    worker: bool,
    /// Все модели на месте (проверка по размеру).
    ready: bool,
    /// Сейчас качаются.
    downloading: bool,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn models_status(state: State<'_, AppState>) -> ModelsStatus {
    let m = ModelPaths::in_dir(&state.models_dir());
    ModelsStatus {
        dir: state.models_dir().display().to_string(),
        whisper: m.whisper.is_file(),
        yamnet: m.yamnet.is_file() && m.yamnet_classes.is_file(),
        llm: m.llm.is_file(),
        worker: llm::find_worker().is_some(),
        ready: crate::model_download::ready(&state.models_dir()),
        downloading: state
            .models_downloading
            .load(std::sync::atomic::Ordering::SeqCst),
    }
}

/// Настройки анализа из интерфейса. Всё необязательное.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct UiOptions {
    use_events: Option<bool>,
    use_speech: Option<bool>,
    use_llm: Option<bool>,
    min_len: Option<f64>,
    max_len: Option<f64>,
    top_k: Option<usize>,
    gpu_layers: Option<u32>,
    language: Option<String>,
    fixed_seconds: Option<f64>,
    weights: Option<score::Weights>,
    llm_top: Option<usize>,
    llm_weight: Option<f64>,
    scout_enabled: Option<bool>,
    scout_coverage: Option<f64>,
}

fn finite_clamp(v: Option<f64>, default: f64, min: f64, max: f64) -> f64 {
    v.filter(|v| v.is_finite())
        .unwrap_or(default)
        .clamp(min, max)
}

impl UiOptions {
    pub(crate) fn into_options(self) -> Options {
        let mut o = Options::default();
        o.scout_enabled = self.scout_enabled.unwrap_or(o.scout_enabled);
        o.scout_coverage = finite_clamp(self.scout_coverage, o.scout_coverage, 0.1, 0.75);
        o.use_events = self.use_events.unwrap_or(o.use_events);
        o.use_speech = self.use_speech.unwrap_or(o.use_speech);
        o.use_llm = self.use_llm.unwrap_or(o.use_llm);
        o.llm_gpu_layers = self.gpu_layers.unwrap_or(o.llm_gpu_layers).min(999);
        o.language = self
            .language
            .filter(|v| !v.contains('\0') && (v == "auto" || whisper_rs::get_lang_id(v).is_some()))
            .unwrap_or(o.language);
        o.params.min_len = finite_clamp(self.min_len, o.params.min_len, 5.0, 170.0);
        o.params.max_len = finite_clamp(
            self.max_len,
            o.params.max_len,
            o.params.min_len + 5.0,
            180.0,
        );
        o.params.top_k = self.top_k.unwrap_or(o.params.top_k).clamp(1, 60);
        o.llm_top = self.llm_top.unwrap_or(o.llm_top).clamp(1, 60);
        o.llm_weight = finite_clamp(self.llm_weight, o.llm_weight, 0.0, 1.0);
        o.fixed_seconds = self
            .fixed_seconds
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(5.0, 180.0));
        if let Some(w) = self.weights {
            let d = score::Weights::default();
            o.weights = score::Weights {
                loudness: finite_clamp(Some(w.loudness as f64), d.loudness as f64, 0.0, 1.0) as f32,
                motion: finite_clamp(Some(w.motion as f64), d.motion as f64, 0.0, 1.0) as f32,
                events: finite_clamp(Some(w.events as f64), d.events as f64, 0.0, 1.0) as f32,
                speech: finite_clamp(Some(w.speech as f64), d.speech as f64, 0.0, 1.0) as f32,
                chat: finite_clamp(Some(w.chat as f64), d.chat as f64, 0.0, 1.0) as f32,
                viewer_clips: finite_clamp(
                    Some(w.viewer_clips as f64),
                    d.viewer_clips as f64,
                    0.0,
                    1.0,
                ) as f32,
                camera: finite_clamp(Some(w.camera as f64), d.camera as f64, 0.0, 1.0) as f32,
            };
            if o.weights.loudness
                + o.weights.motion
                + o.weights.events
                + o.weights.speech
                + o.weights.chat
                + o.weights.viewer_clips
                + o.weights.camera
                == 0.0
            {
                o.weights = d;
            }
        }
        o
    }

    pub(crate) fn normalized(self) -> Self {
        let o = self.into_options();
        Self {
            use_events: Some(o.use_events),
            use_speech: Some(o.use_speech),
            use_llm: Some(o.use_llm),
            min_len: Some(o.params.min_len),
            max_len: Some(o.params.max_len),
            top_k: Some(o.params.top_k),
            gpu_layers: Some(o.llm_gpu_layers),
            language: Some(o.language),
            fixed_seconds: o.fixed_seconds,
            scout_enabled: Some(o.scout_enabled),
            scout_coverage: Some(o.scout_coverage),
            weights: Some(o.weights),
            llm_top: Some(o.llm_top),
            llm_weight: Some(o.llm_weight),
        }
    }
}

const OPTIONS_KEY: &str = "analysis_options";

pub(crate) fn load_options(db: &Connection) -> UiOptions {
    db.query_row("SELECT value FROM kv WHERE key = ?1", [OPTIONS_KEY], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|s| serde_json::from_str::<UiOptions>(&s).ok())
    .unwrap_or_default()
    .normalized()
}

fn store_options(db: &Connection, options: UiOptions) -> CmdResult<UiOptions> {
    let options = options.normalized();
    db.execute("INSERT INTO kv (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![OPTIONS_KEY, serde_json::to_string(&options).map_err(err)?]).map_err(err)?;
    Ok(options)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_options_get(state: State<'_, AppState>) -> UiOptions {
    load_options(&state.db.0.lock().unwrap())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_options_set(
    state: State<'_, AppState>,
    options: UiOptions,
) -> CmdResult<UiOptions> {
    store_options(&state.db.0.lock().unwrap(), options)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    project_id: i64,
    stage: &'static str,
    fraction: f64,
    message: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    project_id: i64,
    moments: usize,
    notes: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    project_id: i64,
    message: String,
    cancelled: bool,
}

/// Что пайплайну нужно от проекта: снимается из базы до запуска.
pub(crate) struct AnalysisInput {
    project_id: i64,
    path: String,
    duration: f64,
    has_audio: bool,
    tight: bool,
}

/// Регистрирует анализ в списке идущих. Второй анализ того же видео отвергается.
pub(crate) fn begin(
    state: &AppState,
    project_id: i64,
) -> CmdResult<(AnalysisInput, watch::Receiver<bool>)> {
    let (path, duration, has_audio, tight): (String, f64, bool, bool) = state
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT path, duration, has_audio, tight FROM projects WHERE id = ?1",
            [project_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, i64>(3)? != 0,
                ))
            },
        )
        .map_err(|_| "проект не найден".to_string())?;

    let (tx, rx) = watch::channel(false);
    {
        let mut jobs = state.analyses.lock().unwrap();
        if jobs.contains_key(&project_id) {
            return Err("анализ этого видео уже идёт".into());
        }
        jobs.insert(project_id, tx);
    }
    Ok((
        AnalysisInput {
            project_id,
            path,
            duration,
            has_audio,
            tight,
        },
        rx,
    ))
}

/// Сам анализ: пайплайн, сохранение моментов, события. Возвращает число моментов или текст ошибки.
pub(crate) async fn execute(
    app: AppHandle,
    input: AnalysisInput,
    rx: watch::Receiver<bool>,
    mut opt: Options,
) -> CmdResult<usize> {
    let file_state = app.state::<AppState>();
    let _source_use = file_state.source_use.read().await;
    let AnalysisInput {
        project_id,
        path,
        duration,
        has_audio,
        tight,
    } = input;
    opt.tight = tight;
    opt.camera = crate::factory::profiles::project_camera(
        &app.state::<AppState>().db.0.lock().unwrap(),
        project_id,
    );
    let extra =
        crate::analysis_inputs::load(&app.state::<AppState>().db.0.lock().unwrap(), project_id);
    opt.chat = extra.chat.map(|f| f.curve);
    opt.viewer_clips = extra.clips.filter(|f| f.clips > 0).map(|f| f.curve);
    let models = ModelPaths::in_dir(&app.state::<AppState>().models_dir());
    let emit_app = app.clone();
    let report: pipeline::Report = Arc::new(move |p: Progress| {
        let _ = emit_app.emit(
            "analysis-progress",
            ProgressEvent {
                project_id,
                stage: p.stage,
                fraction: p.fraction,
                message: p.message,
            },
        );
    });
    let cancelled_rx = rx.clone();
    let gate = app.state::<AppState>().ml_gate.clone();
    let mut gate_cancel = rx.clone();
    let mut result = async {
        let _permit = tokio::select! {
            permit = gate.acquire_owned() => permit.map_err(|e| anyhow::anyhow!(e))?,
            _ = gate_cancel.changed() => return Err(anyhow::anyhow!("Анализ отменён в ожидании ML")),
        };
        if *rx.borrow() { return Err(anyhow::anyhow!("Анализ отменён")); }
        pipeline::run(std::path::Path::new(&path), duration, has_audio, &models, &opt, rx, report).await
    }.await;

    if let (Ok(a), Some(error)) = (&mut result, extra.chat_error) {
        a.notes.push(format!("Чат пропущен: {error}"));
    }
    let state = app.state::<AppState>();
    state.analyses.lock().unwrap().remove(&project_id);
    // При сплошной нарезке (вплотную или N секунд) сохраняются все части.
    let accept_all = tight || opt.fixed_seconds.is_some();
    match result.and_then(|a| {
        save(&state.db.0.lock().unwrap(), project_id, &a, accept_all)
            .map(|n| (n, a.notes))
            .map_err(Into::into)
    }) {
        Ok((moments, notes)) => {
            let _ = app.emit(
                "analysis-done",
                DoneEvent {
                    project_id,
                    moments,
                    notes,
                },
            );
            Ok(moments)
        }
        Err(e) => {
            let message = err(&e);
            let _ = app.emit(
                "analysis-error",
                ErrorEvent {
                    project_id,
                    message: message.clone(),
                    cancelled: *cancelled_rx.borrow(),
                },
            );
            Err(message)
        }
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_start(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
    options: Option<UiOptions>,
) -> CmdResult<()> {
    let options = {
        let db = state.db.0.lock().unwrap();
        match options {
            Some(options) => store_options(&db, options)?,
            None => load_options(&db),
        }
    };
    let (input, rx) = begin(&state, project_id)?;
    let opt = options.into_options();
    crate::host::spawn(async move {
        // результат уже ушёл в события «analysis-done» / «analysis-error»
        let _ = execute(app, input, rx, opt).await;
    });
    Ok(())
}

/// Сохраняет результат анализа вместо прежнего. `accept_all`: все моменты сразу «приняты» (нарезка вплотную).
fn save(
    db: &Connection,
    project_id: i64,
    a: &Analysis,
    accept_all: bool,
) -> rusqlite::Result<usize> {
    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM moments WHERE project_id = ?1", [project_id])?;
    tx.execute(
        "INSERT OR REPLACE INTO analysis (project_id, signals_json, words_json, sentences_json, notes_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            project_id,
            serde_json::to_string(&a.signals).unwrap_or_default(),
            serde_json::to_string(&a.words).unwrap_or_default(),
            serde_json::to_string(&a.sentences).unwrap_or_default(),
            serde_json::to_string(&a.notes).unwrap_or_default(),
        ],
    )?;
    let status = if accept_all { "accepted" } else { "candidate" };
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO moments (project_id, start, \"end\", score, signal_score, llm_score, title, caption, hashtags, climax_t, transcript, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for m in &a.moments {
            insert.execute(params![
                project_id,
                m.start,
                m.end,
                m.score,
                m.signal_score,
                m.llm_score,
                m.title,
                m.caption,
                serde_json::to_string(&m.hashtags).unwrap_or_else(|_| "[]".into()),
                m.climax,
                m.transcript,
                status
            ])?;
        }
    }
    tx.execute(
        "UPDATE projects SET status = 'analyzed' WHERE id = ?1",
        [project_id],
    )?;
    tx.commit()?;
    Ok(a.moments.len())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_cancel(state: State<'_, AppState>, project_id: i64) {
    if let Some(tx) = state.analyses.lock().unwrap().get(&project_id) {
        let _ = tx.send(true);
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analysis_notes(state: State<'_, AppState>, project_id: i64) -> Vec<String> {
    state
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT notes_json FROM analysis WHERE project_id = ?1",
            [project_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentDto {
    id: i64,
    project_id: i64,
    start: f64,
    end: f64,
    score: f64,
    signal_score: f64,
    llm_score: f64,
    title: String,
    caption: String,
    hashtags: Vec<String>,
    climax: f64,
    transcript: String,
    status: String,
    manual: bool,
}

const MOMENT_COLS: &str =
    "id, project_id, start, \"end\", score, signal_score, llm_score, title, caption, hashtags, COALESCE(climax_t, (start + \"end\") / 2), transcript, status, manual";

fn moment_from_row(r: &rusqlite::Row) -> rusqlite::Result<MomentDto> {
    Ok(MomentDto {
        id: r.get(0)?,
        project_id: r.get(1)?,
        start: r.get(2)?,
        end: r.get(3)?,
        score: r.get(4)?,
        signal_score: r.get(5)?,
        llm_score: r.get(6)?,
        title: r.get(7)?,
        caption: r.get(8)?,
        hashtags: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
        climax: r.get(10)?,
        transcript: r.get(11)?,
        status: r.get(12)?,
        manual: r.get::<_, i64>(13)? != 0,
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn moments_list(state: State<'_, AppState>, project_id: i64) -> CmdResult<Vec<MomentDto>> {
    let db = state.db.0.lock().unwrap();
    let mut stmt = db
        .prepare(&format!(
            "SELECT {MOMENT_COLS} FROM moments WHERE project_id = ?1 ORDER BY score DESC, id"
        ))
        .map_err(err)?;
    let rows = stmt.query_map([project_id], moment_from_row).map_err(err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn moment_set_status(state: State<'_, AppState>, id: i64, status: String) -> CmdResult<()> {
    if !["candidate", "accepted", "rejected"].contains(&status.as_str()) {
        return Err(format!("неизвестный статус: {status}"));
    }
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE moments SET status = ?2 WHERE id = ?1",
            params![id, status],
        )
        .map_err(err)?;
    Ok(())
}

fn sentences_of(db: &Connection, project_id: i64) -> Vec<Sentence> {
    db.query_row(
        "SELECT sentences_json FROM analysis WHERE project_id = ?1",
        [project_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|j| serde_json::from_str(&j).ok())
    .unwrap_or_default()
}

fn transcript_in(sentences: &[Sentence], start: f64, end: f64) -> String {
    sentences
        .iter()
        .filter(|s| s.end > start && s.start < end)
        .map(|s| s.text.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Ручная правка границ. Текст пересчитывается, баннер остаётся на месте, но не выходит за новые границы.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn moment_update(
    state: State<'_, AppState>,
    id: i64,
    start: f64,
    end: f64,
    climax: Option<f64>,
) -> CmdResult<MomentDto> {
    update_moment(&state.db.0.lock().unwrap(), id, start, end, climax)
}

pub(crate) fn update_moment(
    db: &Connection,
    id: i64,
    start: f64,
    end: f64,
    climax: Option<f64>,
) -> CmdResult<MomentDto> {
    let (project_id, old_climax): (i64, f64) = db
        .query_row("SELECT project_id, COALESCE(climax_t, (start + \"end\") / 2) FROM moments WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|_| "момент не найден".to_string())?;
    let duration: f64 = db
        .query_row(
            "SELECT duration FROM projects WHERE id = ?1",
            [project_id],
            |r| r.get(0),
        )
        .map_err(err)?;

    let (start, end) = (start.clamp(0.0, duration), end.clamp(0.0, duration));
    if end - start < 3.0 {
        return Err("момент не может быть короче 3 секунд".into());
    }
    let climax = climax
        .unwrap_or(old_climax)
        .clamp(start + 0.2 * (end - start), end - 0.1 * (end - start));
    let text = transcript_in(&sentences_of(db, project_id), start, end);
    db.execute(
        "UPDATE moments SET start = ?2, \"end\" = ?3, climax_t = ?4, transcript = ?5 WHERE id = ?1",
        params![id, start, end, climax, text],
    )
    .map_err(err)?;
    db.query_row(
        &format!("SELECT {MOMENT_COLS} FROM moments WHERE id = ?1"),
        [id],
        moment_from_row,
    )
    .map_err(err)
}

/// Момент, выбранный пользователем вручную; сразу принят. Баннер ставится на пик кривой интереса.
/// `async`: на каждый вызов разбирается весь кэш анализа, в главном потоке окна это подвешивало бы интерфейс.
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn moment_add_manual(
    state: State<'_, AppState>,
    project_id: i64,
    start: f64,
    end: f64,
) -> CmdResult<MomentDto> {
    add_manual(&state.db.0.lock().unwrap(), project_id, start, end)
}

fn add_manual(db: &Connection, project_id: i64, start: f64, end: f64) -> CmdResult<MomentDto> {
    let duration: f64 = db
        .query_row(
            "SELECT duration FROM projects WHERE id = ?1",
            [project_id],
            |r| r.get(0),
        )
        .map_err(|_| "проект не найден".to_string())?;
    let (start, end) = (start.clamp(0.0, duration), end.clamp(0.0, duration));
    if end - start < 3.0 {
        return Err("момент не может быть короче 3 секунд".into());
    }
    let climax = db
        .query_row(
            "SELECT signals_json FROM analysis WHERE project_id = ?1",
            [project_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|j| serde_json::from_str::<Signals>(&j).ok())
        .map(|s| {
            score::pick_climax(
                &score::interest_curve(&s, &score::Weights::default()),
                start,
                end,
            )
        })
        .unwrap_or((start + end) / 2.0);
    let text = transcript_in(&sentences_of(db, project_id), start, end);
    let title = if text.is_empty() {
        "Мой момент".to_string()
    } else {
        text.chars().take(60).collect()
    };
    db.execute(
        "INSERT INTO moments (project_id, start, \"end\", title, climax_t, transcript, status, manual) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'accepted', 1)",
        params![project_id, start, end, title, climax, text],
    )
    .map_err(err)?;
    let id = db.last_insert_rowid();
    db.query_row(
        &format!("SELECT {MOMENT_COLS} FROM moments WHERE id = ?1"),
        [id],
        moment_from_row,
    )
    .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze::pipeline::Moment, render::Word};

    #[test]
    fn analysis_settings_round_trip_in_kv_and_old_settings_still_work() {
        let c = db();
        let mut o: UiOptions = serde_json::from_str(r#"{"language":"auto","fixedSeconds":30,"llmTop":7,"llmWeight":0.7,"weights":{"motion":0.9}}"#).unwrap();
        o.use_events = Some(false);
        let saved = store_options(&c, o).unwrap();
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::to_value(load_options(&c)).unwrap()
        );
        let p = load_options(&c).into_options();
        assert_eq!(
            (p.language.as_str(), p.fixed_seconds, p.llm_top),
            ("auto", Some(30.0), 7)
        );
        assert!(!p.use_events);
        assert_eq!(p.weights.motion, 0.9);
        let old: UiOptions = serde_json::from_str(r#"{"minLen":25,"useSpeech":false}"#).unwrap();
        let p = old.into_options();
        assert_eq!(
            (
                p.params.min_len,
                p.use_speech,
                p.language.as_str(),
                p.fixed_seconds
            ),
            (25.0, false, "ru", None)
        );
    }

    #[test]
    fn malformed_settings_are_finite_bounded_and_recoverable() {
        let o = UiOptions {
            min_len: Some(170.0),
            max_len: Some(10.0),
            fixed_seconds: Some(-1.0),
            llm_top: Some(999),
            llm_weight: Some(f64::NAN),
            language: Some("wrong".into()),
            weights: Some(score::Weights {
                loudness: 0.0,
                motion: 0.0,
                events: 0.0,
                speech: 0.0,
                chat: 0.0,
                viewer_clips: 0.0,
                camera: 0.0,
            }),
            ..Default::default()
        }
        .into_options();
        assert_eq!(
            (
                o.params.min_len,
                o.params.max_len,
                o.fixed_seconds,
                o.llm_top,
                o.llm_weight
            ),
            (170.0, 175.0, Some(5.0), 60, 0.5)
        );
        assert_eq!(o.language, "ru");
        assert!(o.weights.loudness > 0.0);
        let c = db();
        c.execute("INSERT INTO kv VALUES (?1,'bad json')", [OPTIONS_KEY])
            .unwrap();
        assert_eq!(load_options(&c).into_options().language, "ru");
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (1, '/v.mp4', 'h', 'v', 600, 1920, 1080, 30, 1)",
            [],
        )
        .unwrap();
        c
    }

    fn analysis(n: usize) -> Analysis {
        let sentences = (0..60)
            .map(|i| Sentence {
                start: i as f64 * 10.0,
                end: i as f64 * 10.0 + 9.0,
                text: format!("фраза номер {i}"),
            })
            .collect();
        Analysis {
            signals: Signals {
                loudness: vec![0.0; 1200],
                motion: vec![0.0; 1200],
                ..Default::default()
            },
            words: vec![Word {
                text: "слово".into(),
                start: 1.0,
                end: 1.4,
            }],
            sentences,
            moments: (0..n)
                .map(|i| Moment {
                    start: i as f64 * 100.0 + 20.0,
                    end: i as f64 * 100.0 + 60.0,
                    score: 0.9 - i as f64 * 0.1,
                    signal_score: 0.5,
                    llm_score: 0.8,
                    title: format!("Момент {i}"),
                    caption: "подпись".into(),
                    hashtags: vec!["#а".into(), "#б".into()],
                    climax: i as f64 * 100.0 + 40.0,
                    transcript: "текст".into(),
                })
                .collect(),
            notes: vec!["заметка".into()],
        }
    }

    fn list(db: &Connection) -> Vec<MomentDto> {
        let mut st = db
            .prepare(&format!(
                "SELECT {MOMENT_COLS} FROM moments WHERE project_id = 1 ORDER BY score DESC, id"
            ))
            .unwrap();
        st.query_map([], moment_from_row)
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn saved_analysis_round_trips_and_marks_the_project_analyzed() {
        let c = db();
        assert_eq!(save(&c, 1, &analysis(3), false).unwrap(), 3);
        let m = list(&c);
        assert_eq!(m.len(), 3);
        assert_eq!(
            (m[0].title.as_str(), m[0].status.as_str()),
            ("Момент 0", "candidate")
        );
        assert_eq!(m[0].hashtags, vec!["#а", "#б"]);
        assert_eq!(m[0].climax, 40.0);
        let status: String = c
            .query_row("SELECT status FROM projects WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "analyzed");
        assert_eq!(sentences_of(&c, 1).len(), 60);
    }

    #[test]
    fn tight_analysis_takes_every_piece_at_once() {
        let c = db();
        assert_eq!(save(&c, 1, &analysis(4), true).unwrap(), 4);
        assert!(list(&c).iter().all(|m| m.status == "accepted" && !m.manual));
        // повторный анализ без отметки возвращает обычную очередь на просмотр
        save(&c, 1, &analysis(2), false).unwrap();
        assert!(list(&c).iter().all(|m| m.status == "candidate"));
    }

    #[test]
    fn the_tight_flag_defaults_to_off_and_is_stored_per_project() {
        let c = db();
        let tight = |c: &Connection| {
            c.query_row::<i64, _, _>("SELECT tight FROM projects WHERE id = 1", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(tight(&c), 0);
        c.execute("UPDATE projects SET tight = 1 WHERE id = 1", [])
            .unwrap();
        assert_eq!(tight(&c), 1);
    }

    #[test]
    fn reanalysis_replaces_old_moments() {
        let c = db();
        save(&c, 1, &analysis(5), false).unwrap();
        save(&c, 1, &analysis(2), false).unwrap();
        assert_eq!(list(&c).len(), 2);
    }

    #[test]
    fn deleting_a_project_removes_its_moments_and_cache() {
        let c = db();
        save(&c, 1, &analysis(3), false).unwrap();
        c.execute("DELETE FROM projects WHERE id = 1", []).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM moments", [], |r| r.get(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM analysis", [], |r| r.get(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn editing_bounds_recomputes_text_and_keeps_the_banner_inside() {
        let c = db();
        save(&c, 1, &analysis(1), false).unwrap();
        let id = list(&c)[0].id;
        // граница сдвинута: баннер (40 с) выходит за новый конец и подтягивается
        let m = update_moment(&c, id, 20.0, 35.0, None).unwrap();
        assert!(
            m.climax <= 35.0 - 0.1 * 15.0 + 1e-9 && m.climax >= 20.0 + 0.2 * 15.0,
            "{}",
            m.climax
        );
        assert!(
            m.transcript.contains("фраза номер 2") && !m.transcript.contains("фраза номер 5"),
            "{}",
            m.transcript
        );
        // явная секунда баннера принимается
        assert_eq!(
            update_moment(&c, id, 20.0, 60.0, Some(45.0))
                .unwrap()
                .climax,
            45.0
        );
    }

    #[test]
    fn bad_bounds_are_rejected_and_clamped_to_the_video() {
        let c = db();
        save(&c, 1, &analysis(1), false).unwrap();
        let id = list(&c)[0].id;
        assert!(update_moment(&c, id, 30.0, 31.0, None).is_err());
        let m = update_moment(&c, id, 590.0, 9999.0, None).unwrap();
        assert_eq!(m.end, 600.0);
        assert!(update_moment(&c, 12345, 0.0, 10.0, None).is_err());
    }

    #[test]
    fn manual_moment_is_accepted_at_once_and_gets_a_title_from_the_text() {
        let c = db();
        save(&c, 1, &analysis(1), false).unwrap();
        let m = add_manual(&c, 1, 100.0, 140.0).unwrap();
        assert_eq!((m.status.as_str(), m.manual), ("accepted", true));
        assert!(m.title.starts_with("фраза номер"), "{}", m.title);
        assert!(m.climax > 100.0 && m.climax < 140.0);
        assert!(add_manual(&c, 1, 10.0, 11.0).is_err());
        assert!(add_manual(&c, 99, 10.0, 50.0).is_err());
    }
}
