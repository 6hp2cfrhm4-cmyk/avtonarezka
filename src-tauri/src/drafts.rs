//! Черновики (быстрые превью 540×960) и финальный рендер 1080×1920.
//!
//! Жизнь черновика: `queued` → `rendering` → `ready` → `approved` | `rejected`.
//! Финал: `approved` → `final_queued` → `final_rendering` → `final_done`. Сбой — `failed`.

use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}};

use rand::Rng;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

use crate::{
    analysis_cmds::{update_moment, MomentDto},
    assets_cmds::{load_asset, AssetDto},
    gameplay::{self, Clip, Slice},
    media,
    render::{self, Decode, Encoder, MediaRef, Profile, RenderJob, Style, TopFit, Word},
    state::AppState,
};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// Что именно собрано в ролике: стиль и выбранные ассеты со смещениями.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DraftConfig {
    pub style: Style,
    pub banner_id: Option<i64>,
    pub background_id: Option<i64>,
    /// С какой секунды фона начинать.
    pub gameplay_offset: f64,
    pub music_id: Option<i64>,
    pub music_offset: f64,
}

/// Версия раскладки умолчаний. 1: верх 16:9 виден целиком (`Auto`), срез снизу выключен.
const LAYOUT_V: u32 = 1;

/// Настройки, с которых начинаются новые черновики.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Defaults {
    pub style: Style,
    pub banner_id: Option<i64>,
    /// Фоны по порядку: лента геймплея идёт по ним, а когда они кончаются — по кругу (см. `gameplay`).
    pub background_ids: Vec<i64>,
    pub music_id: Option<i64>,
    /// Версия раскладки, с которой умолчания сохранены (см. `LAYOUT_V`).
    pub layout_v: u32,
}

/// Старые умолчания раскладки (`Fill` и срез снизу 20 %) заменяются новыми. Выбранное вручную не трогается.
fn upgrade_layout(style: &mut Style) -> bool {
    let old = style.top_fit == TopFit::Fill && (style.top_crop_bottom - 0.2).abs() < 1e-6;
    if old {
        style.top_fit = TopFit::Auto;
        style.top_crop_bottom = 0.0;
    }
    old
}

pub(crate) fn kv_get(db: &Connection, key: &str) -> Option<String> {
    db.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).ok()
}

pub(crate) fn kv_set(db: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    db.execute("INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
    Ok(())
}

pub(crate) fn load_defaults(db: &Connection) -> Defaults {
    let saved = kv_get(db, "draft_defaults");
    let mut d: Defaults = saved.as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
    if d.layout_v < LAYOUT_V {
        upgrade_layout(&mut d.style);
        d.layout_v = LAYOUT_V;
        if saved.is_some() {
            if let Ok(json) = serde_json::to_string(&d) {
                let _ = kv_set(db, "draft_defaults", &json);
            }
        }
    }
    d
}

/// С какой секунды фона по умолчанию начинается геймплей (не 0: нулевой кадр у некоторых роликов чёрный).
pub(crate) const GAMEPLAY_START: f64 = 0.01;

/// Случайное начало куска длиной `need` внутри файла длиной `media_len` (с шагом 0.5 с).
/// Если файл короче — с нуля: ролик зациклит его.
pub(crate) fn pick_offset(rng: &mut impl Rng, media_len: f64, need: f64) -> f64 {
    let room = media_len - need - 1.0;
    if room <= 0.0 { 0.0 } else { (rng.random_range(0.0..room) * 2.0).round() / 2.0 }
}

/// Ассеты из настроек по умолчанию. Читаются из базы один раз на всю пачку черновиков,
/// а не заново для каждого.
struct DefaultAssets {
    banner: Option<AssetDto>,
    backgrounds: Vec<AssetDto>,
    music: Option<AssetDto>,
}

impl DefaultAssets {
    fn load(db: &Connection, d: &Defaults) -> Self {
        Self {
            banner: d.banner_id.and_then(|id| load_asset(db, id)).filter(|a| a.kind == "banner"),
            backgrounds: d.background_ids.iter().filter_map(|id| load_asset(db, *id)).filter(|a| a.kind == "background").collect(),
            music: d.music_id.and_then(|id| load_asset(db, id)).filter(|a| a.kind == "music"),
        }
    }
}

impl DefaultAssets {
    /// Сколько секунд фона нужно ролику: клип плюс время, пока играет баннер.
    fn need(&self, clip_len: f64) -> f64 {
        clip_len + self.banner.as_ref().map_or(0.0, |b| b.meta.duration)
    }

    /// Фон для каждого принятого момента проекта по таймкодам (см. `gameplay`): `moment_id → место на ленте`.
    /// Считается по всем принятым моментам сразу, поэтому места не зависят от того, для каких уже есть черновики.
    fn gameplay_slices(&self, db: &Connection, project_id: i64, accepted: &[(i64, f64, f64)]) -> HashMap<i64, Slice> {
        let tape: Vec<(i64, f64)> = self.backgrounds.iter().map(|b| (b.id, b.meta.duration)).collect();
        let clips: Vec<Clip> = accepted.iter().map(|&(id, start, end)| Clip { id, start, need: self.need(end - start) }).collect();
        gameplay::allocate(db, project_id, &tape, &clips).into_iter().map(|s| (s.clip, s)).collect()
    }
}

fn new_config(a: &DefaultAssets, d: &Defaults, clip_len: f64, slice: Option<&Slice>, rng: &mut impl Rng) -> DraftConfig {
    DraftConfig {
        style: d.style.clone(),
        banner_id: a.banner.as_ref().map(|b| b.id),
        background_id: slice.map(|s| s.asset),
        gameplay_offset: slice.map_or(0.0, |s| s.offset),
        music_id: a.music.as_ref().map(|m| m.id),
        music_offset: a.music.as_ref().map_or(0.0, |m| pick_offset(rng, m.meta.duration, a.need(clip_len))),
    }
}

/// Принятые моменты проекта по порядку в видео: `(id, начало, конец)`.
fn accepted_moments(db: &Connection, project_id: i64) -> CmdResult<Vec<(i64, f64, f64)>> {
    let mut st = db
        .prepare("SELECT id, start, \"end\" FROM moments WHERE project_id = ?1 AND status = 'accepted' ORDER BY start")
        .map_err(err)?;
    let rows = st.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

/// Создаёт черновики для принятых моментов, у которых их ещё нет, и заново нумерует части по порядку в видео.
pub(crate) fn generate(db: &Connection, project_id: i64) -> CmdResult<usize> {
    let accepted = accepted_moments(db, project_id)?;
    if accepted.is_empty() {
        return Err("Сначала отбери хотя бы один момент".into());
    }
    let have: HashSet<i64> = db
        .prepare("SELECT d.moment_id FROM drafts d JOIN moments m ON m.id = d.moment_id WHERE m.project_id = ?1")
        .map_err(err)?
        .query_map([project_id], |r| r.get(0))
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;

    let defaults = load_defaults(db);
    let assets = DefaultAssets::load(db, &defaults);
    let slices = assets.gameplay_slices(db, project_id, &accepted);
    let mut rng = rand::rng();
    let mut created = 0;
    // одна транзакция: сотни отдельных коммитов (каждый с fsync) подвешивали окно
    let tx = db.unchecked_transaction().map_err(err)?;
    {
        let mut renumber = tx.prepare_cached("UPDATE drafts SET part = ?2 WHERE moment_id = ?1").map_err(err)?;
        let mut insert = tx.prepare_cached("INSERT INTO drafts (moment_id, part, style_json, status) VALUES (?1, ?2, ?3, 'queued')").map_err(err)?;
        for (i, (moment_id, start, end)) in accepted.iter().enumerate() {
            let part = i as i64 + 1;
            if have.contains(moment_id) {
                renumber.execute(params![moment_id, part]).map_err(err)?;
                continue;
            }
            let cfg = new_config(&assets, &defaults, end - start, slices.get(moment_id), &mut rng);
            insert.execute(params![moment_id, part, serde_json::to_string(&cfg).map_err(err)?]).map_err(err)?;
            created += 1;
        }
    }
    tx.execute("UPDATE projects SET status = 'drafts' WHERE id = ?1 AND status IN ('imported', 'analyzed')", [project_id]).map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(created)
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DraftDto {
    pub id: i64,
    pub moment_id: i64,
    pub project_id: i64,
    pub part: i64,
    pub title: String,
    pub caption: String,
    pub hashtags: Vec<String>,
    pub start: f64,
    pub end: f64,
    pub climax: f64,
    pub status: String,
    pub proxy_path: Option<String>,
    pub poster_path: Option<String>,
    pub final_path: Option<String>,
    pub error: Option<String>,
    /// Время последнего изменения (unix): по нему интерфейс понимает, что превью перезаписано.
    pub updated_at: i64,
    pub config: DraftConfig,
}

const DRAFT_SELECT: &str = "SELECT d.id, d.moment_id, m.project_id, d.part, m.title, m.caption, m.hashtags, m.start, m.\"end\",
    COALESCE(m.climax_t, (m.start + m.\"end\") / 2), d.status, d.proxy_path, d.poster_path, d.final_path, d.error, d.style_json, d.updated_at
    FROM drafts d JOIN moments m ON m.id = d.moment_id";

fn draft_from_row(r: &rusqlite::Row) -> rusqlite::Result<DraftDto> {
    Ok(DraftDto {
        id: r.get(0)?,
        moment_id: r.get(1)?,
        project_id: r.get(2)?,
        part: r.get(3)?,
        title: r.get(4)?,
        caption: r.get(5)?,
        hashtags: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        start: r.get(7)?,
        end: r.get(8)?,
        climax: r.get(9)?,
        status: r.get(10)?,
        proxy_path: r.get(11)?,
        poster_path: r.get(12)?,
        final_path: r.get(13)?,
        error: r.get(14)?,
        updated_at: r.get(16)?,
        config: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
    })
}

pub(crate) fn list(db: &Connection, project_id: i64) -> rusqlite::Result<Vec<DraftDto>> {
    let mut st = db.prepare(&format!("{DRAFT_SELECT} WHERE m.project_id = ?1 AND m.status = 'accepted' ORDER BY d.part"))?;
    let rows = st.query_map([project_id], draft_from_row)?;
    rows.collect()
}

pub(crate) fn get(db: &Connection, id: i64) -> CmdResult<DraftDto> {
    db.query_row(&format!("{DRAFT_SELECT} WHERE d.id = ?1"), [id], draft_from_row).map_err(|_| "черновик не найден".to_string())
}

fn set_status(db: &Connection, id: i64, status: &str, error: Option<&str>) {
    let _ = db.execute(
        "UPDATE drafts SET status = ?2, error = ?3, updated_at = unixepoch() WHERE id = ?1",
        params![id, status, error],
    );
}

/// Собирает задание на рендер из того, что лежит в базе. Ничего не запускает и не читает медиафайлы,
/// только проверяет, что они на месте.
pub(crate) fn build_job(db: &Connection, draft_id: i64, profile: Profile, encoder: Encoder, out: PathBuf) -> CmdResult<RenderJob> {
    let d = get(db, draft_id)?;
    let (src_path, src_duration, has_audio, src_w, src_h): (String, f64, i64, f64, f64) = db
        .query_row("SELECT path, duration, has_audio, width, height FROM projects WHERE id = ?1", [d.project_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(err)?;
    if !Path::new(&src_path).is_file() {
        return Err(format!("Исходное видео не найдено: {src_path}"));
    }

    let asset = |id: Option<i64>, kind: &str| -> CmdResult<Option<MediaRef>> {
        let Some(a) = id.and_then(|i| load_asset(db, i)).filter(|a| a.kind == kind) else { return Ok(None) };
        if !Path::new(&a.path).is_file() {
            return Err(format!("Файл «{}» не найден: {}", a.name, a.path));
        }
        Ok(Some(MediaRef { path: PathBuf::from(&a.path), duration: a.meta.duration, has_audio: a.meta.has_audio }))
    };
    let gameplay = asset(d.config.background_id, "background")?.ok_or("Не выбран фон: добавь геймплей в библиотеке и выбери его в настройках")?;
    let banner = asset(d.config.banner_id, "banner")?;
    let music = asset(d.config.music_id, "music")?;

    let words: Vec<Word> = db
        .query_row("SELECT words_json FROM analysis WHERE project_id = ?1", [d.project_id], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|j| serde_json::from_str::<Vec<Word>>(&j).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.end > d.start && w.start < d.end)
        .collect();

    Ok(RenderJob {
        decode: Default::default(),
        source: MediaRef { path: PathBuf::from(src_path), duration: src_duration, has_audio: has_audio != 0 },
        source_aspect: if src_h > 0.0 { src_w / src_h } else { 0.0 },
        start: d.start,
        end: d.end,
        banner_at: banner.as_ref().map(|_| (d.climax - d.start).max(0.0)),
        banner,
        gameplay,
        gameplay_offset: d.config.gameplay_offset,
        music,
        music_offset: d.config.music_offset,
        words,
        part: Some(d.part as u32),
        style: d.config.style,
        profile,
        encoder,
        out,
    })
}

// ---------------------------------------------------------------- очередь рендера

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RenderKind {
    Draft,
    Final,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RenderProgress {
    draft_id: i64,
    kind: RenderKind,
    fraction: f64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RenderDone {
    draft_id: i64,
    kind: RenderKind,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RenderFailed {
    draft_id: i64,
    kind: RenderKind,
    message: String,
    cancelled: bool,
}

/// Папка для готовых роликов: из настроек или `~/Videos/Автонарезка`.
pub(crate) fn output_dir(db: &Connection) -> PathBuf {
    kv_get(db, "output_dir").map(PathBuf::from).unwrap_or_else(|| {
        crate::platform::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Videos").join("Автонарезка")
    })
}

/// Включено ли декодирование на видеокарте (NVDEC). По умолчанию выключено: замер на 1080p показал, что оно не быстрее
/// процессора, потому что узкое место — фильтры на процессоре. Включается переменной `AVTO_RENDER_GPU=nvidia`
/// или настройкой `render_gpu = nvidia`.
pub(crate) fn gpu_decode_allowed(db: &Connection) -> bool {
    let env_on = std::env::var("AVTO_RENDER_GPU").is_ok_and(|v| v.eq_ignore_ascii_case("nvidia"));
    env_on || kv_get(db, "render_gpu").as_deref() == Some("nvidia")
}

/// Имя файла без символов, которые ломают пути.
pub(crate) fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' }).collect();
    let t = t.trim().trim_matches('_').to_string();
    if t.is_empty() { "ролик".into() } else { t.chars().take(80).collect() }
}

/// Свободное имя: `имя.mp4`, затем `имя_2.mp4` и так далее.
pub(crate) fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.mp4"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem}_{n}.mp4"))).find(|p| !p.exists()).unwrap()
}

/// Ставит рендер в очередь. Повторный вызов для уже идущего рендера ничего не делает.
pub fn spawn_render(app: AppHandle, draft_id: i64, kind: RenderKind) {
    queue_render(app, draft_id, kind, true);
}

/// Ставит в очередь пачку рендеров. Статус «в очереди» уже записан вызывающим одним запросом,
/// интерфейс сам перечитывает список после ответа команды, поэтому записи в базу и событий
/// на каждый черновик здесь нет: сотни таких пар подряд подвешивали окно.
fn spawn_renders(app: &AppHandle, ids: &[i64], kind: RenderKind) {
    for &id in ids {
        queue_render(app.clone(), id, kind, false);
    }
}

/// `announce`: записать в базу статус «в очереди» и сообщить о нём интерфейсу.
fn queue_render(app: AppHandle, draft_id: i64, kind: RenderKind, announce: bool) {
    let state = app.state::<AppState>();
    let (tx, rx) = watch::channel(false);
    {
        let mut running = state.renders.lock().unwrap();
        if running.contains_key(&draft_id) {
            return;
        }
        running.insert(draft_id, tx);
    }
    if announce {
        let queued = if kind == RenderKind::Draft { "queued" } else { "final_queued" };
        set_status(&state.db.0.lock().unwrap(), draft_id, queued, None);
        let _ = app.emit("render-progress", RenderProgress { draft_id, kind, fraction: 0.0 });
    }

    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let slots = if kind == RenderKind::Draft { state.draft_slots.clone() } else { state.final_slots.clone() };
        let _permit = slots.acquire_owned().await;

        let result = if *rx.borrow() {
            Err(anyhow::anyhow!("рендер отменён"))
        } else {
            let rendering = if kind == RenderKind::Draft { "rendering" } else { "final_rendering" };
            set_status(&state.db.0.lock().unwrap(), draft_id, rendering, None);
            run_one(&app, draft_id, kind, rx.clone()).await
        };
        state.renders.lock().unwrap().remove(&draft_id);

        let cancelled = *rx.borrow();
        match result {
            Ok(()) => {
                let _ = app.emit("render-done", RenderDone { draft_id, kind });
            }
            Err(e) => {
                let message = format!("{e:#}");
                // отменённый чистовик возвращается в «одобрен»; сорвавшийся — в «final_failed», чтобы его можно было повторить;
                // у черновика отмена и сбой одинаково означают «не собрался»
                let status = match (kind, cancelled) {
                    (RenderKind::Final, true) => "approved",
                    (RenderKind::Final, false) => "final_failed",
                    (RenderKind::Draft, _) => "failed",
                };
                set_status(&state.db.0.lock().unwrap(), draft_id, status, (!cancelled || kind == RenderKind::Draft).then_some(message.as_str()));
                let _ = app.emit("render-error", RenderFailed { draft_id, kind, message, cancelled });
            }
        }
    });
}

async fn run_one(app: &AppHandle, draft_id: i64, kind: RenderKind, cancel: watch::Receiver<bool>) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let nvenc = *state.nvenc.get_or_init(|| async { media::check_env().await.nvenc }).await;
    let mut encoder = if nvenc { Encoder::Nvenc } else { Encoder::X264 };
    let cuda = *state.cuda.get_or_init(|| async { media::check_env().await.cuda }).await;
    // декодирование на видеокарте: только вместе с NVENC (значит, карта NVIDIA есть) и если не отключено
    let mut decode = if nvenc && cuda && gpu_decode_allowed(&state.db.0.lock().unwrap()) { Decode::Nvidia } else { Decode::Cpu };
    let profile = if kind == RenderKind::Draft { Profile::Draft } else { Profile::Final };

    let (project_id, title, part) = {
        let d = get(&state.db.0.lock().unwrap(), draft_id).map_err(|e| anyhow::anyhow!(e))?;
        (d.project_id, d.title, d.part)
    };
    let out = match kind {
        RenderKind::Draft => state.data_dir.join("drafts").join(project_id.to_string()).join(format!("draft_{draft_id}.mp4")),
        RenderKind::Final => {
            let db = state.db.0.lock().unwrap();
            let project_title: String = db.query_row("SELECT title FROM projects WHERE id = ?1", [project_id], |r| r.get(0)).unwrap_or_default();
            let dir = output_dir(&db);
            std::fs::create_dir_all(&dir)?;
            free_path(&dir, &format!("{}_часть{part}_{}", safe_name(&project_title), safe_name(&title)))
        }
    };

    let work = state.data_dir.join("tmp");
    let fonts = state.fonts_dir();
    loop {
        let mut job = build_job(&state.db.0.lock().unwrap(), draft_id, profile, encoder, out.clone()).map_err(|e| anyhow::anyhow!(e))?;
        job.decode = decode;
        let (handle, mut last) = (app.clone(), 0.0f64);
        let res = render::render(&job, &work, Some(&fonts), cancel.clone(), move |f| {
            if f - last >= 0.02 || f >= 1.0 {
                last = f;
                let _ = handle.emit("render-progress", RenderProgress { draft_id, kind, fraction: f });
            }
        })
        .await;
        match res {
            Ok(()) => {
                if kind == RenderKind::Draft {
                    let poster = out.with_extension("jpg");
                    let at = job.freeze().map_or(job.clip_len() * 0.4, |(tb, d)| tb + (d * 0.5).min(1.5));
                    let ok = media::make_poster(&out, &poster, at).await.is_ok();
                    // блокировка базы берётся после последнего await
                    state.db.0.lock().unwrap().execute(
                        "UPDATE drafts SET proxy_path = ?2, poster_path = ?3, status = 'ready', error = NULL, updated_at = unixepoch() WHERE id = ?1",
                        params![draft_id, out.display().to_string(), ok.then(|| poster.display().to_string())],
                    )?;
                } else {
                    state.db.0.lock().unwrap().execute(
                        "UPDATE drafts SET final_path = ?2, status = 'final_done', error = NULL, updated_at = unixepoch() WHERE id = ?1",
                        params![draft_id, out.display().to_string()],
                    )?;
                }
                return Ok(());
            }
            // NVDEC мог отказать (нет поддержки в сборке, драйвер): один раз декодируем на процессоре
            Err(e) if decode == Decode::Nvidia && !*cancel.borrow() => {
                tracing::warn!("NVDEC не справился ({e:#}), повторяю с декодированием на процессоре");
                decode = Decode::Cpu;
            }
            // NVENC мог отказать (занят, нет драйвера): один раз повторяем на процессоре
            Err(e) if encoder == Encoder::Nvenc && !*cancel.borrow() => {
                tracing::warn!("NVENC не справился ({e:#}), повторяю на процессоре");
                encoder = Encoder::X264;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Финал без надписи «Часть N» для выкладки через пул аккаунтов. Это отдельный файл, обычный финал не меняется.
/// Надпись рисуется только при номере части, поэтому у копии номер просто не передаётся.
pub(crate) async fn render_pool_copy(app: &AppHandle, draft_id: i64, out: &Path, on_progress: impl FnMut(f64)) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let nvenc = *state.nvenc.get_or_init(|| async { media::check_env().await.nvenc }).await;
    let encoder = if nvenc { Encoder::Nvenc } else { Encoder::X264 };
    let cuda = *state.cuda.get_or_init(|| async { media::check_env().await.cuda }).await;
    let mut job = build_job(&state.db.0.lock().unwrap(), draft_id, Profile::Final, encoder, out.to_path_buf()).map_err(|e| anyhow::anyhow!(e))?;
    if nvenc && cuda && gpu_decode_allowed(&state.db.0.lock().unwrap()) {
        job.decode = Decode::Nvidia;
    }
    job.part = None;
    // отмены у этого рендера нет; сигнал держим живым, пока идёт рендер
    let (_cancel_tx, cancel) = watch::channel(false);
    let work = state.data_dir.join("tmp");
    let fonts = state.fonts_dir();
    render::render(&job, &work, Some(&fonts), cancel, on_progress).await
}

/// Что менять в моменте вместе с настройками ролика.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub start: f64,
    pub end: f64,
    pub climax: f64,
}

/// Допустимо ли вручную перевести черновик в такой статус из текущего.
pub(crate) fn can_set_status(current: &str, new: &str) -> bool {
    ["ready", "approved", "rejected"].contains(&new) && ["ready", "approved", "rejected", "final_failed"].contains(&current)
}

/// `async`: команда идёт не в главном потоке окна, иначе пока она работает, интерфейс стоит.
#[tauri::command(async)]
pub fn drafts_generate(app: AppHandle, project_id: i64) -> CmdResult<usize> {
    generate_and_render(&app, project_id)
}

/// Создаёт черновики из принятых моментов и ставит превью в рендер. Кнопка и пачка вызывают одно и то же.
pub(crate) fn generate_and_render(app: &AppHandle, project_id: i64) -> CmdResult<usize> {
    let state = app.state::<AppState>();
    let (created, ids): (usize, Vec<i64>) = {
        let db = state.db.0.lock().unwrap();
        let created = generate(&db, project_id)?;
        let ids = list(&db, project_id).map_err(err)?.into_iter().filter(|d| d.status == "queued").map(|d| d.id).collect();
        (created, ids)
    };
    spawn_renders(app, &ids, RenderKind::Draft);
    Ok(created)
}

/// Одобряет все готовые черновики проекта одним запросом (вместо сотни вызовов по одному).
pub(crate) fn approve_ready(db: &Connection, project_id: i64) -> rusqlite::Result<usize> {
    db.execute(
        "UPDATE drafts SET status = 'approved', updated_at = unixepoch()
         WHERE status = 'ready' AND moment_id IN (SELECT id FROM moments WHERE project_id = ?1 AND status = 'accepted')",
        [project_id],
    )
}

#[tauri::command(async)]
pub fn drafts_approve_ready(state: State<'_, AppState>, project_id: i64) -> CmdResult<usize> {
    approve_ready(&state.db.0.lock().unwrap(), project_id).map_err(err)
}

#[tauri::command]
pub fn drafts_list(state: State<'_, AppState>, project_id: i64) -> CmdResult<Vec<DraftDto>> {
    list(&state.db.0.lock().unwrap(), project_id).map_err(err)
}

#[tauri::command]
pub fn draft_set_status(state: State<'_, AppState>, id: i64, status: String) -> CmdResult<()> {
    let db = state.db.0.lock().unwrap();
    let cur = get(&db, id)?.status;
    if !can_set_status(&cur, &status) {
        return Err(format!("Из состояния «{cur}» в «{status}» перейти нельзя"));
    }
    set_status(&db, id, &status, None);
    Ok(())
}

/// Сохраняет настройки (и при необходимости границы момента) и пересобирает черновик.
#[tauri::command]
pub fn draft_update(app: AppHandle, state: State<'_, AppState>, id: i64, config: DraftConfig, bounds: Option<Bounds>) -> CmdResult<MomentDto> {
    let moment = {
        let db = state.db.0.lock().unwrap();
        let d = get(&db, id)?;
        let moment = match bounds {
            Some(b) => update_moment(&db, d.moment_id, b.start, b.end, Some(b.climax))?,
            None => update_moment(&db, d.moment_id, d.start, d.end, Some(d.climax))?,
        };
        db.execute("UPDATE drafts SET style_json = ?2 WHERE id = ?1", params![id, serde_json::to_string(&config).map_err(err)?]).map_err(err)?;
        moment
    };
    spawn_render(app, id, RenderKind::Draft);
    Ok(moment)
}

/// Черновик, созданный до того, как в настройках выбрали фон, нельзя собрать. При повторе берём то, что выбрано сейчас:
/// фон (и баннер с музыкой, если они тоже были пустыми). Выбранное вручную не трогаем.
pub(crate) fn fill_missing_assets(db: &Connection, id: i64) -> CmdResult<bool> {
    let mut d = get(db, id)?;
    if d.config.background_id.is_some() {
        return Ok(false);
    }
    let defaults = load_defaults(db);
    let assets = DefaultAssets::load(db, &defaults);
    if assets.backgrounds.is_empty() {
        return Ok(false);
    }
    let slices = assets.gameplay_slices(db, d.project_id, &accepted_moments(db, d.project_id)?);
    let Some(place) = slices.get(&d.moment_id) else { return Ok(false) };
    d.config.background_id = Some(place.asset);
    d.config.gameplay_offset = place.offset;
    if d.config.banner_id.is_none() {
        d.config.banner_id = assets.banner.as_ref().map(|b| b.id);
    }
    if d.config.music_id.is_none() {
        if let Some(m) = &assets.music {
            d.config.music_id = Some(m.id);
            d.config.music_offset = pick_offset(&mut rand::rng(), m.meta.duration, d.end - d.start);
        }
    }
    db.execute("UPDATE drafts SET style_json = ?2 WHERE id = ?1", params![id, serde_json::to_string(&d.config).map_err(err)?]).map_err(err)?;
    Ok(true)
}

#[tauri::command]
pub fn draft_rerender(app: AppHandle, state: State<'_, AppState>, id: i64) {
    let _ = fill_missing_assets(&state.db.0.lock().unwrap(), id);
    spawn_render(app, id, RenderKind::Draft);
}

/// Переставляет фон по таймкодам у черновиков, которые ещё можно менять (готовы, упали или отклонены), и приводит их
/// раскладку к новым умолчаниям, если стояли старые. Одобренные и ушедшие в финал не трогаются.
/// Ручной выбор фона и секунды у этих черновиков сбрасывается. Возвращает id изменённых, они уже в очереди.
pub(crate) fn realign_gameplay(db: &Connection, project_id: i64) -> CmdResult<Vec<i64>> {
    let defaults = load_defaults(db);
    let assets = DefaultAssets::load(db, &defaults);
    if assets.backgrounds.is_empty() {
        return Err("Сначала отметь фон в библиотеке на шаге «Импорт»".into());
    }
    let slices = assets.gameplay_slices(db, project_id, &accepted_moments(db, project_id)?);

    let mut changed = Vec::new();
    for d in list(db, project_id).map_err(err)? {
        if !matches!(d.status.as_str(), "ready" | "failed" | "rejected") {
            continue;
        }
        let Some(place) = slices.get(&d.moment_id) else { continue };
        let mut cfg = d.config.clone();
        cfg.background_id = Some(place.asset);
        cfg.gameplay_offset = place.offset;
        upgrade_layout(&mut cfg.style);

        let (before, after) = (serde_json::to_string(&d.config).map_err(err)?, serde_json::to_string(&cfg).map_err(err)?);
        if before == after {
            continue;
        }
        db.execute("UPDATE drafts SET style_json = ?2 WHERE id = ?1", params![d.id, after]).map_err(err)?;
        set_status(db, d.id, "queued", None);
        changed.push(d.id);
    }
    Ok(changed)
}

#[tauri::command(async)]
pub fn drafts_realign_gameplay(app: AppHandle, state: State<'_, AppState>, project_id: i64) -> CmdResult<usize> {
    let ids = realign_gameplay(&state.db.0.lock().unwrap(), project_id)?;
    spawn_renders(&app, &ids, RenderKind::Draft);
    Ok(ids.len())
}

#[tauri::command]
pub fn draft_cancel(state: State<'_, AppState>, id: i64) {
    if let Some(tx) = state.renders.lock().unwrap().get(&id) {
        let _ = tx.send(true);
    }
}

#[tauri::command]
pub fn defaults_get(state: State<'_, AppState>) -> Defaults {
    load_defaults(&state.db.0.lock().unwrap())
}

#[tauri::command]
pub fn defaults_set(state: State<'_, AppState>, mut defaults: Defaults) -> CmdResult<()> {
    // то, что сохраняет пользователь, уже новой раскладки: старые умолчания второй раз не «обновляются»
    defaults.layout_v = LAYOUT_V;
    kv_set(&state.db.0.lock().unwrap(), "draft_defaults", &serde_json::to_string(&defaults).map_err(err)?).map_err(err)
}

/// Запускает финальный рендер одобренных черновиков.
#[tauri::command(async)]
pub fn finals_start(app: AppHandle, project_id: i64) -> CmdResult<usize> {
    start_finals(&app, project_id)
}

/// Ставит в финал одобренные черновики проекта (и упавшие финалы — повтор). Пачка вызывает то же самое.
pub(crate) fn start_finals(app: &AppHandle, project_id: i64) -> CmdResult<usize> {
    let state = app.state::<AppState>();
    let ids: Vec<i64> = {
        let db = state.db.0.lock().unwrap();
        let ids: Vec<i64> = list(&db, project_id).map_err(err)?.into_iter().filter(|d| d.status == "approved" || d.status == "final_failed").map(|d| d.id).collect();
        if ids.is_empty() {
            return Err("Нет одобренных черновиков".into());
        }
        let tx = db.unchecked_transaction().map_err(err)?;
        for id in &ids {
            set_status(&tx, *id, "final_queued", None);
        }
        tx.commit().map_err(err)?;
        ids
    };
    spawn_renders(app, &ids, RenderKind::Final);
    Ok(ids.len())
}

/// Открывает путь в программе по умолчанию. Не ждёт её завершения.
fn open_in_system(path: &Path) -> CmdResult<()> {
    crate::platform::open_path(path).map_err(|e| format!("Не удалось открыть: {e}{}", crate::platform::OPENER_HINT))
}

/// Файл готового ролика или папка, где он лежит. Путь берётся из базы по id, а не от интерфейса.
pub(crate) fn final_location(db: &Connection, draft_id: i64, folder: bool) -> CmdResult<PathBuf> {
    let d = get(db, draft_id)?;
    let file = PathBuf::from(d.final_path.ok_or("Финальный ролик ещё не готов")?);
    if !file.is_file() {
        return Err(format!("Файл не найден: {}", file.display()));
    }
    Ok(if folder { file.parent().map(Path::to_path_buf).ok_or("Нет папки у файла")? } else { file })
}

#[tauri::command]
pub fn final_open(state: State<'_, AppState>, id: i64, folder: bool) -> CmdResult<()> {
    let path = final_location(&state.db.0.lock().unwrap(), id, folder)?;
    open_in_system(&path)
}

#[tauri::command]
pub fn output_dir_open(state: State<'_, AppState>) -> CmdResult<()> {
    let dir = output_dir(&state.db.0.lock().unwrap());
    std::fs::create_dir_all(&dir).map_err(|e| format!("Не удалось создать папку: {e}"))?;
    open_in_system(&dir)
}

#[tauri::command]
pub fn output_dir_get(state: State<'_, AppState>) -> String {
    output_dir(&state.db.0.lock().unwrap()).display().to_string()
}

#[tauri::command]
pub fn output_dir_set(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    std::fs::create_dir_all(&path).map_err(|e| format!("Не удалось создать папку: {e}"))?;
    kv_set(&state.db.0.lock().unwrap(), "output_dir", &path).map_err(err)
}

/// После перезапуска приложения незавершённые рендеры прерваны.
pub(crate) fn recover_interrupted(db: &Connection) {
    let _ = db.execute("UPDATE drafts SET status = 'failed', error = 'Рендер прерван: приложение было закрыто' WHERE status IN ('queued', 'rendering')", []);
    let _ = db.execute("UPDATE drafts SET status = 'approved' WHERE status IN ('final_queued', 'final_rendering')", []);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пустые файлы-заглушки: `build_job` только проверяет, что они есть.
    struct Env {
        db: Connection,
        dir: PathBuf,
    }

    fn touch(dir: &Path, name: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p.display().to_string()
    }

    fn env(tag: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("avto-drafts-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (1, ?1, 'h', 'Подкаст', 3600, 1920, 1080, 30, 1)",
            [touch(&dir, "source.mp4")],
        )
        .unwrap();
        let words = serde_json::to_string(&vec![
            Word { text: "до".into(), start: 90.0, end: 90.4 },
            Word { text: "внутри".into(), start: 110.0, end: 110.5 },
            Word { text: "после".into(), start: 500.0, end: 500.4 },
        ])
        .unwrap();
        db.execute("INSERT INTO analysis (project_id, signals_json, words_json, sentences_json) VALUES (1, '{}', ?1, '[]')", [words]).unwrap();
        // моменты: 3 принятых (позже по времени идут раньше в таблице), один отклонён, один в очереди
        for (start, end, status) in [(300.0, 340.0, "accepted"), (100.0, 140.0, "accepted"), (200.0, 230.0, "rejected"), (400.0, 440.0, "candidate"), (600.0, 650.0, "accepted")] {
            db.execute(
                "INSERT INTO moments (project_id, start, \"end\", title, climax_t, status) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                params![start, end, format!("м{start}"), start + 20.0, status],
            )
            .unwrap();
        }
        let asset = |kind: &str, name: &str, dur: f64, audio: bool| {
            db.execute(
                "INSERT INTO assets (kind, path, name, meta_json) VALUES (?1, ?2, ?3, ?4)",
                params![kind, touch(&dir, &format!("{name}.mp4")), name, format!("{{\"duration\":{dur},\"hasAudio\":{audio}}}")],
            )
            .unwrap();
        };
        asset("banner", "banner", 4.0, true); // id 1
        asset("background", "parkour", 900.0, false); // id 2
        asset("background", "cs", 30.0, false); // id 3: короче ролика
        asset("music", "music", 200.0, true); // id 4
        Env { db, dir }
    }

    fn set_defaults(e: &Env, d: &Defaults) {
        kv_set(&e.db, "draft_defaults", &serde_json::to_string(d).unwrap()).unwrap();
    }

    fn full_defaults() -> Defaults {
        Defaults { banner_id: Some(1), background_ids: vec![2, 3], music_id: Some(4), ..Default::default() }
    }

    #[test]
    fn offsets_stay_inside_the_file_with_half_second_steps() {
        let mut rng = rand::rng();
        for _ in 0..500 {
            let o = pick_offset(&mut rng, 900.0, 60.0);
            assert!((0.0..=839.0).contains(&o) && (o * 2.0).fract() == 0.0, "{o}");
        }
        assert_eq!(pick_offset(&mut rng, 30.0, 60.0), 0.0, "короткий файл — с нуля");
        assert_eq!(pick_offset(&mut rng, 60.5, 60.0), 0.0);
    }

    #[test]
    fn generate_makes_drafts_only_for_accepted_moments_in_video_order() {
        let e = env("gen");
        set_defaults(&e, &full_defaults());
        assert_eq!(generate(&e.db, 1).unwrap(), 3);
        let drafts = list(&e.db, 1).unwrap();
        assert_eq!(drafts.iter().map(|d| (d.part, d.start)).collect::<Vec<_>>(), vec![(1, 100.0), (2, 300.0), (3, 600.0)]);
        assert!(drafts.iter().all(|d| d.status == "queued"));
        for d in &drafts {
            assert_eq!(d.config.banner_id, Some(1));
            assert_eq!(d.config.music_id, Some(4));
            // фон нарезан по тем же таймкодам, что и основное видео (клипы 100, 300 и 600 с — все в первом фоне)
            assert_eq!(d.config.background_id, Some(2));
            assert_eq!(d.config.gameplay_offset, d.start);
        }
        let st: String = e.db.query_row("SELECT status FROM projects WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "drafts");
    }

    #[test]
    fn retry_fills_in_the_background_chosen_after_the_draft_was_made() {
        let e = env("heal");
        generate(&e.db, 1).unwrap(); // умолчаний ещё нет: фон не выбран
        let id = list(&e.db, 1).unwrap()[0].id;
        assert_eq!(get(&e.db, id).unwrap().config.background_id, None);
        assert!(!fill_missing_assets(&e.db, id).unwrap(), "выбирать пока нечего");

        set_defaults(&e, &full_defaults());
        assert!(fill_missing_assets(&e.db, id).unwrap());
        let c = get(&e.db, id).unwrap().config;
        // момент 100..140: фон берётся с той же секунды
        assert_eq!((c.background_id, c.gameplay_offset), (Some(2), 100.0));
        assert_eq!((c.banner_id, c.music_id), (Some(1), Some(4)));
        assert!(!fill_missing_assets(&e.db, id).unwrap(), "выбранное не перезаписывается");
    }

    fn add_project(e: &Env, id: i64, moments: &[(f64, f64)]) {
        e.db.execute(
            "INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (?1, ?2, ?3, 'Второе', 1800, 1280, 720, 30, 1)",
            params![id, touch(&e.dir, &format!("source{id}.mp4")), format!("h{id}")],
        )
        .unwrap();
        for (start, end) in moments {
            e.db.execute("INSERT INTO moments (project_id, start, \"end\", title, status) VALUES (?1, ?2, ?3, 'м', 'accepted')", params![id, start, end]).unwrap();
        }
    }

    fn gameplay_of(e: &Env, project_id: i64) -> Vec<(i64, Option<i64>, f64)> {
        list(&e.db, project_id).unwrap().iter().map(|d| (d.part, d.config.background_id, d.config.gameplay_offset)).collect()
    }

    #[test]
    fn the_next_video_continues_the_gameplay_where_the_previous_one_ended() {
        let e = env("tape");
        set_defaults(&e, &Defaults { banner_id: Some(1), background_ids: vec![2], ..Default::default() });
        generate(&e.db, 1).unwrap();
        // клипы 100..140, 300..340, 600..650 и баннер 4 с: лента дошла до 654-й секунды фона
        assert_eq!(gameplay_of(&e, 1), vec![(1, Some(2), 100.0), (2, Some(2), 300.0), (3, Some(2), 600.0)]);

        add_project(&e, 2, &[(50.0, 90.0), (120.0, 160.0)]);
        generate(&e.db, 2).unwrap();
        assert_eq!(gameplay_of(&e, 2), vec![(1, Some(2), 704.0), (2, Some(2), 774.0)]);

        // повторный запуск и новые моменты в первом видео ленту второго не сдвигают
        e.db.execute("INSERT INTO moments (project_id, start, \"end\", title, status) VALUES (1, 10, 30, 'раньше', 'accepted')", []).unwrap();
        generate(&e.db, 1).unwrap();
        generate(&e.db, 2).unwrap();
        assert_eq!(gameplay_of(&e, 2), vec![(1, Some(2), 704.0), (2, Some(2), 774.0)]);

        // третье видео идёт сразу за вторым (его последний клип кончается на 774 + 44 = 818-й секунде)
        add_project(&e, 3, &[(0.0, 40.0)]);
        generate(&e.db, 3).unwrap();
        assert_eq!(gameplay_of(&e, 3), vec![(1, Some(2), 818.0)]);
    }

    #[test]
    fn restarting_the_tape_begins_the_next_generation_from_the_start() {
        let e = env("restart");
        set_defaults(&e, &Defaults { background_ids: vec![2], ..Default::default() });
        generate(&e.db, 1).unwrap();
        gameplay::reset(&e.db);
        add_project(&e, 2, &[(50.0, 90.0)]);
        generate(&e.db, 2).unwrap();
        assert_eq!(gameplay_of(&e, 2), vec![(1, Some(2), 50.0)]);
    }

    #[test]
    fn realign_changes_only_drafts_that_can_still_change_and_upgrades_the_old_layout() {
        let e = env("realign");
        set_defaults(&e, &Defaults { banner_id: Some(1), background_ids: vec![2], ..Default::default() });
        generate(&e.db, 1).unwrap();
        let ids: Vec<i64> = list(&e.db, 1).unwrap().iter().map(|d| d.id).collect();

        // как у черновиков, сделанных в 0.1.0: фон с 0,01 с, старая раскладка
        for id in &ids {
            let mut c = get(&e.db, *id).unwrap().config;
            c.gameplay_offset = GAMEPLAY_START;
            c.style.top_fit = TopFit::Fill;
            c.style.top_crop_bottom = 0.2;
            e.db.execute("UPDATE drafts SET style_json = ?2 WHERE id = ?1", params![id, serde_json::to_string(&c).unwrap()]).unwrap();
        }
        e.db.execute("UPDATE drafts SET status = 'ready' WHERE id = ?1", [ids[0]]).unwrap();
        e.db.execute("UPDATE drafts SET status = 'approved' WHERE id = ?1", [ids[1]]).unwrap();
        e.db.execute("UPDATE drafts SET status = 'failed' WHERE id = ?1", [ids[2]]).unwrap();
        // у третьего раскладка настроена вручную: её не трогаем
        let mut c = get(&e.db, ids[2]).unwrap().config;
        c.style.top_crop_bottom = 0.3;
        e.db.execute("UPDATE drafts SET style_json = ?2 WHERE id = ?1", params![ids[2], serde_json::to_string(&c).unwrap()]).unwrap();

        let changed = realign_gameplay(&e.db, 1).unwrap();
        assert_eq!(changed, vec![ids[0], ids[2]], "одобренный не меняется");

        let first = get(&e.db, ids[0]).unwrap();
        assert_eq!((first.config.gameplay_offset, first.config.style.top_fit, first.config.style.top_crop_bottom), (100.0, TopFit::Auto, 0.0));
        assert_eq!(first.status, "queued");
        let approved = get(&e.db, ids[1]).unwrap();
        assert_eq!((approved.config.gameplay_offset, approved.status.as_str()), (GAMEPLAY_START, "approved"));
        let third = get(&e.db, ids[2]).unwrap().config;
        assert_eq!((third.gameplay_offset, third.style.top_fit, third.style.top_crop_bottom), (600.0, TopFit::Fill, 0.3));

        assert!(realign_gameplay(&e.db, 1).unwrap().is_empty(), "повторный пересчёт ничего не меняет");
    }

    #[test]
    fn realign_without_backgrounds_explains_what_to_do() {
        let e = env("realign-none");
        generate(&e.db, 1).unwrap();
        assert!(realign_gameplay(&e.db, 1).unwrap_err().contains("фон"));
    }

    #[test]
    fn saved_defaults_with_the_old_layout_are_upgraded_exactly_once() {
        let e = env("layout");
        let mut old = Defaults::default();
        old.style.top_fit = TopFit::Fill;
        old.style.top_crop_bottom = 0.2;
        set_defaults(&e, &old);
        let d = load_defaults(&e.db);
        assert_eq!((d.style.top_fit, d.style.top_crop_bottom, d.layout_v), (TopFit::Auto, 0.0, LAYOUT_V));

        // человек сам вернул «Заполнить» и срез: после обновления это его выбор
        let mut mine = load_defaults(&e.db);
        mine.style.top_fit = TopFit::Fill;
        mine.style.top_crop_bottom = 0.2;
        set_defaults(&e, &mine);
        let again = load_defaults(&e.db);
        assert_eq!((again.style.top_fit, again.style.top_crop_bottom), (TopFit::Fill, 0.2));

        // ручная раскладка при обновлении не затирается
        let mut custom = Defaults::default();
        custom.style.top_fit = TopFit::Contain;
        custom.style.top_crop_bottom = 0.1;
        set_defaults(&e, &custom);
        assert_eq!(load_defaults(&e.db).style.top_fit, TopFit::Contain);
    }

    #[test]
    fn generate_is_idempotent_and_renumbers_parts_when_a_moment_is_added() {
        let e = env("renum");
        set_defaults(&e, &full_defaults());
        generate(&e.db, 1).unwrap();
        let before: Vec<i64> = list(&e.db, 1).unwrap().iter().map(|d| d.id).collect();
        assert_eq!(generate(&e.db, 1).unwrap(), 0, "повторный запуск ничего не создаёт");

        // принят ещё один момент, который в видео идёт раньше всех
        e.db.execute("INSERT INTO moments (project_id, start, \"end\", title, status) VALUES (1, 10, 50, 'раньше', 'accepted')", []).unwrap();
        assert_eq!(generate(&e.db, 1).unwrap(), 1);
        let drafts = list(&e.db, 1).unwrap();
        assert_eq!(drafts.iter().map(|d| d.part).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert_eq!(drafts[0].start, 10.0);
        // старые черновики сохранили свои настройки и id
        assert!(before.iter().all(|id| drafts.iter().any(|d| d.id == *id)));
    }

    #[test]
    fn rejected_moment_hides_its_draft() {
        let e = env("hide");
        set_defaults(&e, &full_defaults());
        generate(&e.db, 1).unwrap();
        e.db.execute("UPDATE moments SET status = 'rejected' WHERE start = 300", []).unwrap();
        assert_eq!(list(&e.db, 1).unwrap().len(), 2);
    }

    #[test]
    fn generate_without_accepted_moments_is_an_error() {
        let e = env("none");
        e.db.execute("UPDATE moments SET status = 'candidate'", []).unwrap();
        assert!(generate(&e.db, 1).is_err());
    }

    #[test]
    fn drafts_are_created_even_without_assets_but_cannot_render() {
        let e = env("noassets");
        assert_eq!(generate(&e.db, 1).unwrap(), 3);
        let id = list(&e.db, 1).unwrap()[0].id;
        let msg = build_job(&e.db, id, Profile::Draft, Encoder::X264, e.dir.join("o.mp4")).unwrap_err();
        assert!(msg.contains("Не выбран фон"), "{msg}");
    }

    #[test]
    fn job_carries_the_clip_banner_time_part_and_only_words_inside() {
        let e = env("job");
        set_defaults(&e, &Defaults { banner_id: Some(1), background_ids: vec![2], music_id: Some(4), ..Default::default() });
        generate(&e.db, 1).unwrap();
        let d = &list(&e.db, 1).unwrap()[0]; // момент 100..140, баннер на 120
        let job = build_job(&e.db, d.id, Profile::Draft, Encoder::X264, e.dir.join("o.mp4")).unwrap();

        assert_eq!((job.start, job.end), (100.0, 140.0));
        assert_eq!(job.banner_at, Some(20.0), "секунда баннера считается от начала клипа");
        assert_eq!(job.part, Some(1));
        assert_eq!(job.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), vec!["внутри"]);
        assert_eq!(job.banner.as_ref().unwrap().duration, 4.0);
        assert!(job.banner.as_ref().unwrap().has_audio);
        assert!(job.music.is_some() && !job.gameplay.has_audio);
        assert_eq!(job.source.duration, 3600.0);
        assert!((job.source_aspect - 1920.0 / 1080.0).abs() < 1e-9, "форма кадра нужна, чтобы 16:9 показать целиком");
        assert_eq!(job.gameplay_offset, 100.0, "фон идёт с того же таймкода, что и клип");
        assert!((job.out_len() - 44.0).abs() < 1e-9, "клип 40 с + баннер 4 с");
    }

    #[test]
    fn no_banner_means_no_freeze() {
        let e = env("nobanner");
        set_defaults(&e, &Defaults { background_ids: vec![2], ..Default::default() });
        generate(&e.db, 1).unwrap();
        let job = build_job(&e.db, list(&e.db, 1).unwrap()[0].id, Profile::Draft, Encoder::X264, e.dir.join("o.mp4")).unwrap();
        assert!(job.banner.is_none() && job.banner_at.is_none() && job.freeze().is_none());
        assert!((job.out_len() - 40.0).abs() < 1e-9);
    }

    #[test]
    fn missing_asset_file_gives_a_readable_error() {
        let e = env("missing");
        set_defaults(&e, &full_defaults());
        generate(&e.db, 1).unwrap();
        let path: String = e.db.query_row("SELECT path FROM assets WHERE id = 2", [], |r| r.get(0)).unwrap();
        std::fs::remove_file(&path).unwrap();
        // выбран фон 2 или 3; удаляем нужный, чтобы ошибка была наверняка
        for d in list(&e.db, 1).unwrap() {
            if d.config.background_id == Some(2) {
                let msg = build_job(&e.db, d.id, Profile::Draft, Encoder::X264, e.dir.join("o.mp4")).unwrap_err();
                assert!(msg.contains("parkour") && msg.contains("не найден"), "{msg}");
            }
        }
    }

    #[test]
    fn user_edits_to_the_config_reach_the_job() {
        let e = env("edit");
        set_defaults(&e, &full_defaults());
        generate(&e.db, 1).unwrap();
        let d = list(&e.db, 1).unwrap().remove(0);
        let mut cfg = d.config.clone();
        cfg.background_id = Some(2);
        cfg.gameplay_offset = 123.5;
        cfg.style.split = 0.6;
        cfg.style.gameplay_freeze = true;
        cfg.music_id = None;
        e.db.execute("UPDATE drafts SET style_json = ?2 WHERE id = ?1", params![d.id, serde_json::to_string(&cfg).unwrap()]).unwrap();

        let job = build_job(&e.db, d.id, Profile::Final, Encoder::Nvenc, e.dir.join("o.mp4")).unwrap();
        assert_eq!(job.gameplay_offset, 123.5);
        assert_eq!(job.style.split, 0.6);
        assert!(job.style.gameplay_freeze && job.music.is_none());
        assert_eq!((job.profile, job.encoder), (Profile::Final, Encoder::Nvenc));
    }

    #[test]
    fn failed_final_can_be_reviewed_again() {
        assert!(can_set_status("final_failed", "approved"));
        assert!(can_set_status("final_failed", "rejected"));
        assert!(!can_set_status("final_failed", "final_done"));
    }

    #[test]
    fn final_location_comes_from_the_database_and_checks_the_file() {
        let e = env("loc");
        generate(&e.db, 1).unwrap();
        let id = list(&e.db, 1).unwrap()[0].id;
        assert!(final_location(&e.db, id, false).unwrap_err().contains("ещё не готов"));

        let file = touch(&e.dir, "out.mp4");
        e.db.execute("UPDATE drafts SET final_path = ?2, status = 'final_done' WHERE id = ?1", params![id, file]).unwrap();
        assert_eq!(final_location(&e.db, id, false).unwrap(), PathBuf::from(&file));
        assert_eq!(final_location(&e.db, id, true).unwrap(), e.dir);

        std::fs::remove_file(&file).unwrap();
        assert!(final_location(&e.db, id, false).unwrap_err().contains("не найден"));
        assert!(final_location(&e.db, 9999, false).is_err());
    }

    #[test]
    fn status_transitions_are_limited_to_the_review_states() {
        assert!(can_set_status("ready", "approved"));
        assert!(can_set_status("approved", "rejected"));
        assert!(can_set_status("rejected", "ready"));
        assert!(!can_set_status("rendering", "approved"), "нельзя одобрить то, что ещё считается");
        assert!(!can_set_status("failed", "approved"));
        assert!(!can_set_status("ready", "final_done"));
    }

    #[test]
    fn interrupted_renders_are_recovered_on_start() {
        let e = env("recover");
        generate(&e.db, 1).unwrap();
        let ids: Vec<i64> = list(&e.db, 1).unwrap().iter().map(|d| d.id).collect();
        e.db.execute("UPDATE drafts SET status = 'rendering' WHERE id = ?1", [ids[0]]).unwrap();
        e.db.execute("UPDATE drafts SET status = 'final_rendering' WHERE id = ?1", [ids[1]]).unwrap();
        recover_interrupted(&e.db);
        let by = |i: i64| get(&e.db, i).unwrap();
        assert_eq!(by(ids[0]).status, "failed");
        assert!(by(ids[0]).error.unwrap().contains("прерван"));
        assert_eq!(by(ids[1]).status, "approved", "чистовик возвращается в очередь одобренных");
    }

    #[test]
    fn hundreds_of_moments_become_drafts_in_one_pass() {
        let e = env("many");
        set_defaults(&e, &full_defaults());
        {
            let mut ins = e
                .db
                .prepare("INSERT INTO moments (project_id, start, \"end\", title, climax_t, status) VALUES (1, ?1, ?2, 'м', ?3, 'accepted')")
                .unwrap();
            for i in 0..500 {
                let s = 700.0 + i as f64 * 5.0;
                ins.execute(params![s, s + 4.0, s + 2.0]).unwrap();
            }
        }
        let started = std::time::Instant::now();
        assert_eq!(generate(&e.db, 1).unwrap(), 503); // 3 из env() и 500 новых
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());

        let drafts = list(&e.db, 1).unwrap();
        assert_eq!(drafts.len(), 503);
        assert!(drafts.iter().enumerate().all(|(i, d)| d.part == i as i64 + 1 && d.status == "queued"));
        assert!(drafts.iter().all(|d| d.config.banner_id == Some(1) && d.config.music_id == Some(4)));
        // повтор ничего не создаёт и не ломает нумерацию
        assert_eq!(generate(&e.db, 1).unwrap(), 0);
        assert_eq!(list(&e.db, 1).unwrap().len(), 503);
    }

    #[test]
    fn approve_ready_takes_only_ready_drafts_of_the_project() {
        let e = env("approve");
        generate(&e.db, 1).unwrap();
        let ids: Vec<i64> = list(&e.db, 1).unwrap().iter().map(|d| d.id).collect();
        for (id, st) in ids.iter().zip(["ready", "ready", "rejected"]) {
            e.db.execute("UPDATE drafts SET status = ?2 WHERE id = ?1", params![id, st]).unwrap();
        }
        assert_eq!(approve_ready(&e.db, 1).unwrap(), 2);
        let st = |i: i64| get(&e.db, i).unwrap().status;
        assert_eq!((st(ids[0]), st(ids[1]), st(ids[2])), ("approved".into(), "approved".into(), "rejected".into()));
        assert_eq!(approve_ready(&e.db, 1).unwrap(), 0);
        assert_eq!(approve_ready(&e.db, 99).unwrap(), 0, "чужой проект не затрагивается");
    }

    #[test]
    fn file_names_are_safe_and_never_overwrite() {
        assert_eq!(safe_name("Подкаст: «часть» 1/2?"), "Подкаст_ _часть_ 1_2");
        assert_eq!(safe_name("???"), "ролик");
        assert_eq!(safe_name(&"а".repeat(200)).chars().count(), 80);

        let dir = std::env::temp_dir().join(format!("avto-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_path(&dir, "ролик"), dir.join("ролик.mp4"));
        std::fs::write(dir.join("ролик.mp4"), b"x").unwrap();
        assert_eq!(free_path(&dir, "ролик"), dir.join("ролик_2.mp4"));
        std::fs::write(dir.join("ролик_2.mp4"), b"x").unwrap();
        assert_eq!(free_path(&dir, "ролик"), dir.join("ролик_3.mp4"));
    }
}
