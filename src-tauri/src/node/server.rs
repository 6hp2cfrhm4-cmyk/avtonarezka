//! Операции принимающего узла: хранилище файлов по хэшу, приём упаковок, состояние задач, выдача результатов.
//! Всё работает на обычном соединении SQLite, без `AppHandle`: ими пользуется и `avtoctl`.
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use base64::Engine;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};


use super::pack::{self, Dirs, Pack};

/// Размер куска файла в одном запросе (после base64 строка остаётся меньше `MAX_LINE`).
pub const CHUNK: usize = 512 * 1024;
const MAX_BLOB: u64 = 2_000_000_000;

pub struct Store {
    pub data_dir: PathBuf,
}

impl Store {
    pub fn blobs(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }
    fn kit(&self) -> PathBuf {
        self.data_dir.join("kit")
    }
    fn sources(&self) -> PathBuf {
        self.data_dir.join("sources")
    }
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("avtonarezka.db")
    }
    fn conn(&self) -> Result<Connection, String> {
        if !self.db_path().is_file() {
            return Err("База ещё не создана: демон не запускался".into());
        }
        let c = Connection::open_with_flags(
            self.db_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        c.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| e.to_string())?;
        Ok(c)
    }
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn part_path(store: &Store, sha: &str) -> PathBuf {
    store.blobs().join(format!(".part-{sha}"))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .ok_or_else(|| format!("нет параметра «{key}»"))
}

/// Какие файлы уже есть целиком и сколько байт лежит в недокачанных (для докачки).
pub fn have(store: &Store, args: &Value) -> Result<Value, String> {
    let mut present = Vec::new();
    let mut partial = serde_json::Map::new();
    for s in args["sha256"].as_array().into_iter().flatten() {
        let Some(sha) = s.as_str().filter(|s| valid_sha(s)) else {
            continue;
        };
        if pack::blob_path(&store.blobs(), sha).is_file() {
            present.push(sha.to_string());
        } else if let Ok(m) = std::fs::metadata(part_path(store, sha)) {
            partial.insert(sha.to_string(), json!(m.len()));
        }
    }
    Ok(json!({"present": present, "partial": partial}))
}

/// Принимает кусок файла. Кусок должен начинаться ровно там, где закончился предыдущий; иначе возвращаем,
/// сколько уже принято, и клиент продолжает оттуда. Когда файл целый, он проверяется по хэшу.
pub fn put(store: &Store, args: &Value) -> Result<Value, String> {
    let sha = arg_str(args, "sha256")?;
    if !valid_sha(sha) {
        return Err("неверный хэш".into());
    }
    let total = args["total"].as_u64().ok_or("нет размера")?;
    if total > MAX_BLOB {
        return Err("файл слишком большой".into());
    }
    let offset = args["offset"].as_u64().ok_or("нет смещения")?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(arg_str(args, "data")?)
        .map_err(|e| format!("данные не разобрать: {e}"))?;
    if data.len() > CHUNK {
        return Err("кусок слишком большой".into());
    }
    std::fs::create_dir_all(store.blobs()).map_err(|e| e.to_string())?;
    let final_path = pack::blob_path(&store.blobs(), sha);
    if final_path.is_file() {
        return Ok(json!({"received": total, "done": true}));
    }
    let part = part_path(store, sha);
    let have_len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if offset != have_len {
        return Ok(json!({"received": have_len, "done": false, "resync": true}));
    }
    if have_len + data.len() as u64 > total {
        return Err("данных больше заявленного размера".into());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .map_err(|e| e.to_string())?;
    f.write_all(&data).map_err(|e| e.to_string())?;
    f.flush().map_err(|e| e.to_string())?;
    let received = have_len + data.len() as u64;
    if received < total {
        return Ok(json!({"received": received, "done": false}));
    }
    let (got, _) = pack::sha256_file(&part).map_err(|e| e.to_string())?;
    if got != sha {
        let _ = std::fs::remove_file(&part);
        return Err("контрольная сумма не сошлась, файл отброшен".into());
    }
    std::fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
    Ok(json!({"received": received, "done": true}))
}

pub fn submit(store: &Store, args: &Value) -> Result<Value, String> {
    let pack: Pack = serde_json::from_value(args["pack"].clone())
        .map_err(|e| format!("упаковка не разобрана: {e}"))?;
    let (kit, sources, blobs) = (store.kit(), store.sources(), store.blobs());
    let db = store.conn()?;
    let id = pack::apply(
        &db,
        &pack,
        &Dirs {
            blobs: &blobs,
            kit: &kit,
            sources: &sources,
        },
    )?;
    Ok(json!({"remote_id": id}))
}

/// Готовые ролики задачи: `[(путь, имя)]`.
fn finals(db: &Connection, item_id: i64) -> Vec<(PathBuf, String)> {
    let mut st = match db.prepare(
        "SELECT d.final_path FROM drafts d JOIN moments m ON m.id=d.moment_id
         JOIN factory_items f ON f.project_id=m.project_id
         WHERE f.id=?1 AND d.status='final_done' AND d.final_path IS NOT NULL ORDER BY d.id",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    st.query_map([item_id], |r| r.get::<_, String>(0))
        .map(|rows| {
            rows.flatten()
                .map(|p| {
                    let name = Path::new(&p)
                        .file_name()
                        .map_or("clip.mp4".into(), |n| n.to_string_lossy().into_owned());
                    (PathBuf::from(p), name)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Состояние переданных задач. Готовые ролики отдаются списком с размерами; забирает их `get`.
pub fn items(store: &Store, args: &Value) -> Result<Value, String> {
    let db = store.conn()?;
    let mut out = Vec::new();
    for id in args["ids"].as_array().into_iter().flatten() {
        let Some(id) = id.as_i64() else { continue };
        let row = db
            .query_row(
                "SELECT stage,error,title FROM factory_items WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((stage, error, title)) = row else {
            out.push(json!({"id": id, "missing": true}));
            continue;
        };
        let outputs: Vec<Value> = finals(&db, id)
            .into_iter()
            .enumerate()
            .map(|(n, (p, name))| {
                json!({"n": n, "name": name, "size": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)})
            })
            .collect();
        out.push(json!({"id": id, "stage": stage, "error": error, "title": title, "outputs": outputs}));
    }
    Ok(json!({"items": out}))
}

/// Кусок готового ролика. Отдаются только файлы, которые числятся финалами этой задачи.
pub fn get(store: &Store, args: &Value) -> Result<Value, String> {
    let id = args["id"].as_i64().ok_or("нет id")?;
    let n = args["n"].as_u64().ok_or("нет номера")? as usize;
    let offset = args["offset"].as_u64().ok_or("нет смещения")?;
    let db = store.conn()?;
    let (path, _) = finals(&db, id)
        .into_iter()
        .nth(n)
        .ok_or("такого файла нет")?;
    let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; CHUNK];
    let read = f.read(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(read);
    Ok(json!({
        "size": size,
        "offset": offset,
        "data": base64::engine::general_purpose::STANDARD.encode(&buf),
        "sha256": if offset + read as u64 >= size { Some(pack::sha256_file(&path).map_err(|e| e.to_string())?.0) } else { None },
    }))
}

/// Снимает задачу, которая на сервере ещё не начата (возврат на ПК).
pub fn cancel(store: &Store, args: &Value) -> Result<Value, String> {
    let id = args["id"].as_i64().ok_or("нет id")?;
    let db = store.conn()?;
    let stage: Option<String> = db
        .query_row("SELECT stage FROM factory_items WHERE id=?1 AND owner_node='local'", [id], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    match stage.as_deref() {
        None => Err("такой задачи нет".into()),
        Some("queued") => {
            db.execute("DELETE FROM factory_items WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            db.execute(
                "UPDATE transfers SET state='returned',updated_at=unixepoch() WHERE direction='in' AND local_id=?1",
                [id],
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({"cancelled": true}))
        }
        Some(s) => Err(format!("на сервере уже идёт работа (стадия {s}), вернуть нельзя")),
    }
}

/// Отметка «ПК забрал результаты»: сервер может освободить место.
pub fn ack(store: &Store, args: &Value) -> Result<Value, String> {
    let id = args["id"].as_i64().ok_or("нет id")?;
    let db = store.conn()?;
    let files = finals(&db, id);
    db.execute(
        "UPDATE transfers SET state='done',updated_at=unixepoch() WHERE direction='in' AND local_id=?1",
        [id],
    )
    .map_err(|e| e.to_string())?;
    let mut freed = 0u64;
    for (p, _) in files {
        freed += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&p);
    }
    Ok(json!({"freed": freed}))
}

/// Нагрузка и место: из /proc и statvfs без внешних команд.
pub fn sysinfo(data_dir: &Path) -> Value {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let load: Vec<f64> = read("/proc/loadavg")
        .split_whitespace()
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();
    let mem_available_kb = read("/proc/meminfo")
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok());
    let some10 = |text: String| -> Option<f64> {
        text.lines().find(|l| l.starts_with("some"))?.split_whitespace().find_map(|p| p.strip_prefix("avg10=")?.parse().ok())
    };
    // Давление считаем у чужих служб (system.slice, user.slice), а не у всей машины: наши задачи сами создают
    // давление, когда упираются в свои лимиты, и по общему счётчику регулятор тормозил бы сам себя.
    // Без cgroup v2 — общий счётчик.
    let psi = |name: &str| -> Option<f64> {
        let others: Vec<f64> = ["system.slice", "user.slice"].iter().filter_map(|s| some10(read(&format!("/sys/fs/cgroup/{s}/{name}.pressure")))).collect();
        if others.is_empty() {
            some10(read(&format!("/proc/pressure/{name}")))
        } else {
            others.into_iter().reduce(f64::max)
        }
    };
    json!({
        "load": load,
        "mem_available_mb": mem_available_kb.map(|k| k / 1024),
        "psi": {"cpu": psi("cpu"), "memory": psi("memory"), "io": psi("io")},
        "disk_free_mb": disk_free_mb(data_dir),
        "data_bytes": dir_size(data_dir),
    })
}

fn disk_free_mb(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .args(["-Pk"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb / 1024)
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                total += m.len();
            }
        }
    }
    total
}



// ------------------------------------------------------------------ настройки и ключи интеграций

/// Что разрешено передавать на сервер. Остальное сервер не примет, даже по правильному ключу.
pub const SECRET_NAMES: &[&str] = &["factory_twitch_app", "discovery_youtube_key", "discovery_kick_token", "notify_bot_token"];
pub const KV_NAMES: &[&str] = &["discovery_config", "notify_config"];

fn secrets_of(store: &Store) -> crate::tiktok::secrets::Secrets {
    crate::tiktok::secrets::Secrets::detect_headless(&store.data_dir)
}

/// `{"secrets": {имя: значение|null}, "kv": {имя: json-строка}}`: пустое значение секрета удаляет его.
pub fn config_put(store: &Store, args: &Value) -> Result<Value, String> {
    let secrets = secrets_of(store);
    let mut done = Vec::new();
    if let Some(map) = args["secrets"].as_object() {
        for (name, v) in map {
            if !SECRET_NAMES.contains(&name.as_str()) {
                return Err(format!("секрет «{name}» не разрешён"));
            }
            match v.as_str().filter(|s| !s.is_empty()) {
                Some(val) if val.len() <= 2048 => secrets.set_sync(name, val).map_err(|e| format!("{e:#}"))?,
                Some(_) => return Err(format!("секрет «{name}» слишком длинный")),
                None => secrets.delete_sync(name).map_err(|e| format!("{e:#}"))?,
            }
            done.push(name.clone());
        }
    }
    if let Some(map) = args["kv"].as_object() {
        let db = store.conn()?;
        for (name, v) in map {
            if !KV_NAMES.contains(&name.as_str()) {
                return Err(format!("настройка «{name}» не разрешена"));
            }
            let text = v.as_str().ok_or("настройка должна быть строкой JSON")?;
            if text.len() > 16 * 1024 || serde_json::from_str::<Value>(text).is_err() {
                return Err(format!("настройка «{name}»: неверный JSON"));
            }
            crate::db::kv_set(&db, name, text).map_err(|e| e.to_string())?;
            done.push(name.clone());
        }
    }
    Ok(json!({"applied": done}))
}

/// Что настроено на сервере: наличие секретов (без значений) и сами настройки.
pub fn config_get(store: &Store, _args: &Value) -> Result<Value, String> {
    let secrets = secrets_of(store);
    let have: serde_json::Map<String, Value> = SECRET_NAMES.iter().map(|n| (n.to_string(), json!(secrets.get_sync(n).ok().flatten().is_some()))).collect();
    let db_path = store.data_dir.join("avtonarezka.db");
    let mut kv = serde_json::Map::new();
    if db_path.is_file() {
        if let Ok(c) = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) {
            for n in KV_NAMES {
                if let Some(v) = crate::db::kv_get(&c, n) {
                    kv.insert(n.to_string(), json!(v));
                }
            }
        }
    }
    Ok(json!({"secrets": have, "kv": kv, "keyStore": !matches!(secrets, crate::tiktok::secrets::Secrets::Memory(_))}))
}
