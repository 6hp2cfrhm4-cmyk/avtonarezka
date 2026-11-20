//! Структурированный журнал без исходных сообщений и переносимый JSON-пакет.
//! Локальные подробности остаются в events/обычном stderr; экспорт не читает секреты.
use crate::host::{AppHandle, Manager};
use crate::{drafts, state::AppState};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tracing::{
    field::{Field, Visit},
    Event, Subscriber,
};
use tracing_subscriber::{layer::Context, Layer};

const MAX_RECORDS: usize = 2000;
const MAX_LOG_BYTES: u64 = 512 * 1024;
static RECORDER: OnceLock<Arc<Recorder>> = OnceLock::new();

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Failure {
    #[default]
    Other,
    Memory,
    Gpu,
    Authorization,
    Quota,
    Captcha,
    Network,
    MissingFile,
    Cancelled,
}
fn classify(text: &str) -> Failure {
    let s = text.to_lowercase();
    if [
        "out of memory",
        "out_of_memory",
        "недостаточно памяти",
        "cannot allocate",
    ]
    .iter()
    .any(|v| s.contains(v))
    {
        Failure::Memory
    } else if ["captcha", "капч"].iter().any(|v| s.contains(v)) {
        Failure::Captcha
    } else if ["quota", "429", "квот", "rate limit"]
        .iter()
        .any(|v| s.contains(v))
    {
        Failure::Quota
    } else if ["unauthorized", "401", "403", "авторизац", "токен"]
        .iter()
        .any(|v| s.contains(v))
    {
        Failure::Authorization
    } else if ["vulkan", "cuda", "nvenc", "gpu", "device lost"]
        .iter()
        .any(|v| s.contains(v))
    {
        Failure::Gpu
    } else if ["no such file", "не найден", "not found"]
        .iter()
        .any(|v| s.contains(v))
    {
        Failure::MissingFile
    } else if ["timeout", "timed out", "connection", "соединени"]
        .iter()
        .any(|v| s.contains(v))
    {
        Failure::Network
    } else if ["cancel", "отмен", "остановлен"]
        .iter()
        .any(|v| s.contains(v))
    {
        Failure::Cancelled
    } else {
        Failure::Other
    }
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Component {
    Analysis,
    Render,
    Publish,
    Sources,
    Factory,
    Operations,
    App,
    Dependency,
}
fn component(target: &str) -> Component {
    if !target.starts_with("avtonarezka_lib") {
        Component::Dependency
    } else if target.contains("analyze")
        || target.contains("analysis")
        || target.contains("caption")
    {
        Component::Analysis
    } else if target.contains("render") || target.contains("drafts") {
        Component::Render
    } else if target.contains("publish") || target.contains("tiktok") {
        Component::Publish
    } else if target.contains("sources") || target.contains("youtube") {
        Component::Sources
    } else if target.contains("factory") {
        Component::Factory
    } else if target.contains("operations") {
        Component::Operations
    } else {
        Component::App
    }
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    at: i64,
    level: Level,
    component: Component,
    class: Failure,
    fingerprint: String,
}
#[derive(Default)]
struct Buffer {
    records: VecDeque<Record>,
    dir: Option<PathBuf>,
    bytes: u64,
    error: bool,
}
#[derive(Default)]
pub struct Recorder(Mutex<Buffer>);
pub struct JournalLayer(Arc<Recorder>);
#[derive(Default)]
struct Visitor {
    hash: Sha256,
    class: Failure,
}
impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        self.hash.update(field.name());
        self.hash.update([0]);
        self.hash.update(text.as_bytes());
        let class = classify(&text);
        if !matches!(class, Failure::Other) {
            self.class = class;
        }
    }
}
impl<S: Subscriber> Layer<S> for JournalLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut v = Visitor::default();
        event.record(&mut v);
        let m = event.metadata();
        let level = match *m.level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            tracing::Level::DEBUG => Level::Debug,
            _ => Level::Trace,
        };
        self.0.push(Record {
            at: chrono::Utc::now().timestamp(),
            level,
            component: component(m.target()),
            class: v.class,
            fingerprint: hex::encode(v.hash.finalize()),
        });
    }
}
impl Recorder {
    fn push(&self, r: Record) {
        let Ok(mut b) = self.0.lock() else { return };
        if let Some(dir) = b.dir.clone() {
            let path = dir.join("runtime.ndjson");
            let result = (|| -> std::io::Result<()> {
                let line = serde_json::to_vec(&r)?;
                if b.bytes + line.len() as u64 + 1 > MAX_LOG_BYTES {
                    let prev = dir.join("runtime.previous.ndjson");
                    if prev.exists() {
                        std::fs::remove_file(&prev)?;
                    }
                    std::fs::rename(&path, prev)?;
                    b.bytes = 0;
                }
                let mut out = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                out.write_all(&line)?;
                out.write_all(b"\n")?;
                b.bytes += line.len() as u64 + 1;
                Ok(())
            })();
            if result.is_err() {
                b.error = true;
            }
        }
        b.records.push_back(r);
        while b.records.len() > MAX_RECORDS {
            b.records.pop_front();
        }
    }
    pub fn bind(&self, data: &Path) -> std::io::Result<()> {
        let result = self.bind_inner(data);
        if result.is_err() {
            self.0.lock().unwrap().error = true;
        }
        result
    }
    fn bind_inner(&self, data: &Path) -> std::io::Result<()> {
        let dir = data.join("logs");
        std::fs::create_dir_all(&dir)?;
        let mut records = VecDeque::new();
        for name in ["runtime.previous.ndjson", "runtime.ndjson"] {
            if let Ok(file) = std::fs::File::open(dir.join(name)) {
                let mut raw = String::new();
                file.take(MAX_LOG_BYTES + 1).read_to_string(&mut raw)?;
                for line in raw.lines() {
                    if let Ok(r) = serde_json::from_str::<Record>(line) {
                        if r.fingerprint.len() == 64
                            && r.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
                        {
                            records.push_back(r);
                        }
                    }
                }
            }
        }
        let mut b = self.0.lock().unwrap();
        records.append(&mut b.records);
        while records.len() > MAX_RECORDS {
            records.pop_front();
        }
        b.records = records;
        b.bytes = std::fs::metadata(dir.join("runtime.ndjson"))
            .map(|m| m.len())
            .unwrap_or(0);
        b.dir = Some(dir);
        Ok(())
    }
}
pub fn recorder() -> Arc<Recorder> {
    RECORDER
        .get_or_init(|| Arc::new(Recorder::default()))
        .clone()
}
pub fn layer() -> JournalLayer {
    JournalLayer(recorder())
}

fn groups(db: &Connection, sql: &str, allowed: &[&str]) -> Result<BTreeMap<String, i64>, String> {
    let mut out = BTreeMap::new();
    let mut st = db.prepare(sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (k, n) = row.map_err(|e| e.to_string())?;
        *out.entry(if allowed.contains(&k.as_str()) {
            k
        } else {
            "other".into()
        })
        .or_default() += n;
    }
    Ok(out)
}
fn database(db: &Connection) -> Result<serde_json::Value, String> {
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut counts = BTreeMap::new();
    for table in [
        "projects",
        "channels",
        "templates",
        "source_items",
        "factory_items",
        "drafts",
        "ready_videos",
        "publications",
        "campaigns",
        "metrics",
    ] {
        let n: i64 = tx
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        counts.insert(table, n);
    }
    let stages = groups(
        &tx,
        "SELECT stage,count(*) FROM factory_items GROUP BY stage",
        &[
            "discovered",
            "queued",
            "fetching",
            "scouting",
            "selecting",
            "drafting",
            "qc",
            "awaiting_approval",
            "auto_approved",
            "rendering_final",
            "captioning",
            "scheduled",
            "publishing",
            "published",
            "measured",
            "completed",
            "failed",
            "rejected",
            "paused",
        ],
    )?;
    let publications = groups(
        &tx,
        "SELECT state,count(*) FROM publications GROUP BY state",
        &[
            "drafting",
            "ready",
            "scheduled",
            "queued",
            "sending",
            "done",
            "needs_check",
            "failed",
            "cancelled",
        ],
    )?;
    let mut events = vec![];
    let mut st=tx.prepare("SELECT id,created_at,item_id,channel_id,stage,kind,message FROM events ORDER BY id DESC LIMIT 500").map_err(|e|e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (id, at, item, channel, stage, kind, message) = row.map_err(|e| e.to_string())?;
        // Ни stage, ни kind из повреждённой БД не должны стать свободным текстом экспорта.
        let stage = stage.filter(|s| stages.contains_key(s));
        let kind = if [
            "stage",
            "error",
            "attention",
            "storage",
            "rights",
            "metrics",
            "notification",
            "publish",
            "recovered",
            "approval",
            "info",
        ]
        .contains(&kind.as_str())
        {
            kind
        } else {
            "other".into()
        };
        events.push(serde_json::json!({"id":id,"at":at,"itemId":item,"channelId":channel,"stage":stage,"kind":kind,"class":classify(&message),"fingerprint":hex::encode(Sha256::digest(message.as_bytes()))}));
    }
    drop(st);
    let foreign_keys = {
        let mut st = tx
            .prepare("PRAGMA foreign_key_check")
            .map_err(|e| e.to_string())?;
        let n = st
            .query_map([], |_| Ok(()))
            .map_err(|e| e.to_string())?
            .count();
        n
    };
    let result = serde_json::json!({"schema":tx.pragma_query_value(None,"user_version",|r|r.get::<_,i64>(0)).map_err(|e|e.to_string())?,"foreignKeyViolations":foreign_keys,"counts":counts,"factoryStages":stages,"publicationStates":publications,"factorySettings":crate::factory::resources::load(&tx),"factoryPaused":drafts::kv_get(&tx,"factory_paused").as_deref()==Some("1"),"publisherArmed":drafts::kv_get(&tx,"publisher_armed").as_deref()==Some("1"),"events":events});
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn diagnostics_collect(app: AppHandle) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    let db = database(&state.db.0.lock().unwrap())?;
    let log = recorder();
    let (runtime, error) = {
        let b = log.0.lock().unwrap();
        (b.records.iter().cloned().collect::<Vec<_>>(), b.error)
    };
    let power = state.power.0.lock().unwrap().clone();
    // Свободные reason/error могут содержать путь ОС, поэтому только логические поля.
    Ok(
        serde_json::json!({"format":"avtonarezka-diagnostics-v1","createdAt":chrono::Utc::now().to_rfc3339(),"app":{"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"debug":cfg!(debug_assertions)},"database":db,"runtimeLog":runtime,"logWriteFailed":error,"resources":{"analyses":state.analyses.lock().unwrap().len(),"renders":state.renders.lock().unwrap().len(),"downloads":state.downloads.lock().unwrap().len(),"mlBusy":state.ml_gate.available_permits()==0,"sourceBusy":state.source_use.try_write().is_err(),"inhibitNeeded":power.needed,"inhibitActive":power.active,"inhibitFailed":power.error.is_some()},"models":{"coreReady":crate::model_download::ready(&state.models_dir()),"face":crate::model_download::face_ready(&state.models_dir())},"tools":{"ffmpeg":crate::platform::find_tool("ffmpeg").is_some(),"ffprobe":crate::platform::find_tool("ffprobe").is_some(),"ytdlp":crate::youtube::engine::find_ytdlp().is_some(),"inhibit":crate::platform::find_tool("systemd-inhibit").is_some()},"privacy":"Тексты сообщений и подписей, имена файлов/каналов/аккаунтов, URL, секреты и копия БД не включены. Отпечатки SHA-256 позволяют сопоставить повторные ошибки; подробности остаются в локальном журнале приложения."}),
    )
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn diagnostics_export(app: AppHandle, path: String) -> Result<(), String> {
    let bundle = diagnostics_collect(app).await?;
    let target = PathBuf::from(path);
    if !target.is_absolute() || target.extension().is_none_or(|v| v != "json") {
        return Err("Выбери абсолютный путь JSON-файла диагностики".into());
    }
    tokio::task::spawn_blocking(move || write_bundle(&target, &bundle))
        .await
        .map_err(|e| e.to_string())?
}
fn write_bundle(target: &Path, bundle: &serde_json::Value) -> Result<(), String> {
    let part = target.with_file_name(format!(".diagnostics-{}.tmp", rand::random::<u64>()));
    let result = (|| -> Result<(), String> {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut out, bundle).map_err(|e| e.to_string())?;
        out.write_all(b"\n").map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        drop(out);
        std::fs::rename(&part, target).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(part);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;
    #[test]
    fn journal_persists_only_typed_metadata_and_rolls_over() {
        let dir = std::env::temp_dir().join(format!("avto-diagnostic-{}", rand::random::<u64>()));
        let r = Arc::new(Recorder::default());
        r.bind(&dir).unwrap();
        let subscriber = tracing_subscriber::registry().with(JournalLayer(r.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(
                token = "SENSITIVE_TOKEN",
                path = "/private/source.mp4",
                "Vulkan out of memory Bearer secret"
            );
        });
        let log = std::fs::read_to_string(dir.join("logs/runtime.ndjson")).unwrap();
        assert!(!log.contains("SENSITIVE_TOKEN"));
        assert!(!log.contains("private"));
        assert!(!log.contains("Bearer"));
        assert!(log.contains("memory"));
        let r2 = Recorder::default();
        r2.bind(&dir).unwrap();
        assert_eq!(r2.0.lock().unwrap().records.len(), 1);
        r2.0.lock().unwrap().bytes = MAX_LOG_BYTES;
        r2.push(Record {
            at: 0,
            level: Level::Info,
            component: Component::App,
            class: Failure::Other,
            fingerprint: "a".repeat(64),
        });
        assert!(dir.join("logs/runtime.previous.ndjson").is_file());
        let r3 = Recorder::default();
        r3.bind(&dir).unwrap();
        assert_eq!(r3.0.lock().unwrap().records.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn database_bundle_omits_arbitrary_data_and_keeps_counts_and_failures() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO kv(key,value)VALUES('SECRET_SETTING','SENSITIVE_TOKEN')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO events(kind,stage,message)VALUES('SENSITIVE_TOKEN','/private/path','401 access_token=SENSITIVE_TOKEN')",[]).unwrap();
        let bundle = database(&db).unwrap();
        let raw = serde_json::to_string(&bundle).unwrap();
        assert!(!raw.contains("SENSITIVE_TOKEN"));
        assert!(!raw.contains("private"));
        assert!(!raw.contains("SECRET_SETTING"));
        assert!(raw.contains("authorization"));
        assert_eq!(bundle["counts"]["projects"], 0);
        assert_eq!(bundle["foreignKeyViolations"], 0);
        let dir = std::env::temp_dir().join(format!("avto-bundle-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diagnostic.json");
        write_bundle(&path, &bundle).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).unwrap())
                .unwrap(),
            bundle
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
