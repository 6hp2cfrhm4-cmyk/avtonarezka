//! Клиент канала «ПК → сервер»: запуск `ssh … avtoctl` (или любой другой команды с тем же протоколом),
//! предохранитель от блокировки по неудачным входам, загрузка файлов с докачкой, отправка задач и приём результатов.
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Mutex,
};

use base64::Engine;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    pack::{self, BlobRef, Pack},
    server::CHUNK,
    PROTOCOL,
};

/// Что нужно, чтобы поговорить с сервером. `program` — `ssh`; в проверках — сам `avtoctl`.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: String,
    #[serde(default)]
    pub enabled: bool,
}

pub const KV_CONFIG: &str = "node_server";
pub const KV_FUSE: &str = "node_fuse";

pub fn load_config(db: &Connection) -> Option<ServerConfig> {
    crate::db::kv_get(db, KV_CONFIG).and_then(|j| serde_json::from_str(&j).ok())
}
pub fn save_config(db: &Connection, c: &ServerConfig) -> Result<(), String> {
    crate::db::kv_set(db, KV_CONFIG, &serde_json::to_string(c).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Команда запуска: ssh с ограниченным набором параметров. Приложение не использует агент, пароли и
/// чужие ключи; хост проверяется по собственному файлу known_hosts приложения.
pub fn ssh_command(c: &ServerConfig, known_hosts: &Path) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-p", &c.port.to_string()])
        .args(["-i", &c.key_path])
        .args(["-o", "BatchMode=yes"])
        .args(["-o", "IdentitiesOnly=yes"])
        .args(["-o", "PasswordAuthentication=no"])
        .args(["-o", "ConnectTimeout=15"])
        .args(["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"])
        .args(["-o", "StrictHostKeyChecking=yes"])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .args(["-o", "ForwardAgent=no", "-o", "ClearAllForwardings=yes"])
        .arg(format!("{}@{}", c.user, c.host))
        .arg("serve");
    cmd
}

// ------------------------------------------------------------------ предохранитель

/// После двух неудачных соединений подряд приложение перестаёт пытаться (fail2ban на сервере банит
/// по 5 неудачам за 10 минут: лучше остановиться заранее). Снимается кнопкой или через `COOLDOWN`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Fuse {
    pub failures: u32,
    pub blown_at: Option<i64>,
}

pub const FUSE_LIMIT: u32 = 2;
pub const COOLDOWN: i64 = 30 * 60;

impl Fuse {
    pub fn allowed(&self, now: i64) -> Result<(), String> {
        match self.blown_at {
            Some(t) if now - t < COOLDOWN => Err(format!(
                "Предохранитель: {} неудачных входа подряд. Пробую снова не раньше чем через {} мин (или сбросьте вручную)",
                self.failures,
                (COOLDOWN - (now - t) + 59) / 60
            )),
            _ => Ok(()),
        }
    }
    pub fn record(&mut self, ok: bool, now: i64) {
        if ok {
            *self = Fuse::default();
            return;
        }
        // после остывания счёт начинается заново
        if self.blown_at.is_some_and(|t| now - t >= COOLDOWN) {
            *self = Fuse::default();
        }
        self.failures += 1;
        if self.failures >= FUSE_LIMIT {
            self.blown_at = Some(now);
        }
    }
    pub fn load(db: &Connection) -> Self {
        crate::db::kv_get(db, KV_FUSE)
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, db: &Connection) {
        if let Ok(j) = serde_json::to_string(self) {
            let _ = crate::db::kv_set(db, KV_FUSE, &j);
        }
    }
}

// ------------------------------------------------------------------ канал

pub trait Wire {
    fn call(&mut self, op: &str, args: Value) -> Result<Value, String>;
}

/// Подпроцесс с протоколом строк JSON: ssh на сервер либо локальный `avtoctl`.
pub struct ProcWire {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

/// Ошибка соединения (до первого ответа) отличается от ответа сервера с ошибкой: только первая бьёт по предохранителю.
#[derive(Debug)]
pub struct Unreachable(pub String);

impl ProcWire {
    pub fn spawn(mut cmd: Command) -> Result<Self, Unreachable> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Unreachable(format!("не запустить: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| Unreachable("нет stdin".into()))?;
        let out = BufReader::new(child.stdout.take().ok_or_else(|| Unreachable("нет stdout".into()))?);
        let mut w = Self { child, stdin, out, next: 1 };
        match w.call("ping", json!({})) {
            Ok(v) if v["protocol"] == PROTOCOL => Ok(w),
            Ok(_) => Err(Unreachable("версия протокола на сервере другая: обновите демон".into())),
            Err(e) => {
                let mut stderr = String::new();
                if let Some(mut s) = w.child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut s, &mut stderr);
                }
                let _ = w.child.kill();
                let detail = stderr.lines().last().unwrap_or("").trim().to_string();
                Err(Unreachable(if detail.is_empty() { e } else { format!("{e}: {detail}") }))
            }
        }
    }
}

impl Wire for ProcWire {
    fn call(&mut self, op: &str, args: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        let line = serde_json::to_string(&json!({"id": id, "op": op, "args": args})).map_err(|e| e.to_string())?;
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("канал закрыт: {e}"))?;
        let mut resp = String::new();
        let n = self.out.read_line(&mut resp).map_err(|e| format!("канал оборвался: {e}"))?;
        if n == 0 {
            return Err("канал закрыт сервером".into());
        }
        let v: Value = serde_json::from_str(resp.trim()).map_err(|e| format!("ответ не разобрать: {e}"))?;
        if v["ok"] == true {
            Ok(v["result"].clone())
        } else {
            Err(v["error"].as_str().unwrap_or("ошибка сервера").to_string())
        }
    }
}

impl Drop for ProcWire {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ------------------------------------------------------------------ передача

/// Загружает файлы, которых нет на сервере; обрыв продолжается с места, где остановились.
pub fn upload(
    w: &mut dyn Wire,
    blobs: &[BlobRef],
    paths: &BTreeMap<String, PathBuf>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let total: u64 = blobs.iter().map(|b| b.size).sum();
    let have = w.call("have", json!({"sha256": blobs.iter().map(|b| &b.sha256).collect::<Vec<_>>()}))?;
    let present: Vec<String> = serde_json::from_value(have["present"].clone()).unwrap_or_default();
    let mut done: u64 = blobs
        .iter()
        .filter(|b| present.contains(&b.sha256))
        .map(|b| b.size)
        .sum();
    progress(done, total);
    for b in blobs.iter().filter(|b| !present.contains(&b.sha256)) {
        let path = paths.get(&b.sha256).ok_or_else(|| format!("нет пути для «{}»", b.name))?;
        let mut f = std::fs::File::open(path).map_err(|e| format!("«{}»: {e}", b.name))?;
        let mut offset = have["partial"][&b.sha256].as_u64().unwrap_or(0).min(b.size);
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let base = done;
        let mut buf = vec![0u8; CHUNK];
        while offset < b.size {
            let n = std::io::Read::read(&mut f, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(format!("«{}» изменился во время передачи", b.name));
            }
            let r = w.call(
                "put",
                json!({"sha256": b.sha256, "total": b.size, "offset": offset,
                       "data": base64::engine::general_purpose::STANDARD.encode(&buf[..n])}),
            )?;
            let received = r["received"].as_u64().unwrap_or(0);
            if r["resync"] == true {
                offset = received;
                std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
            } else {
                offset += n as u64;
            }
            progress(base + offset, total);
        }
        done = base + b.size;
    }
    progress(total, total);
    Ok(())
}

/// Отправляет одну задачу целиком: файлы, потом упаковку; затем отдаёт её серверу в базе ПК.
/// База блокируется только на сборку упаковки и на запись результата, а не на время передачи.
pub fn send_item(
    db: &Mutex<Connection>,
    w: &mut dyn Wire,
    item_id: i64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Pack, String> {
    let (pack, paths) = pack::build_with_paths(&db.lock().unwrap(), item_id)?;
    upload(w, &pack.blobs(), &paths, progress)?;
    let r = w.call("submit", json!({"pack": pack}))?;
    let remote = r["remote_id"].as_i64().ok_or("сервер не вернул номер задачи")?;
    pack::mark_handed_over(&db.lock().unwrap(), item_id, &pack, remote, "server")?;
    Ok(pack)
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Pulled {
    pub finished: Vec<i64>,
    pub failed: Vec<i64>,
    pub waiting: usize,
}

/// Проверяет переданные задачи, скачивает готовые ролики в `<data>/from-server/<pack>/`, закрывает задачу на ПК.
pub fn pull_results(db: &Mutex<Connection>, w: &mut dyn Wire, data_dir: &Path) -> Result<Pulled, String> {
    let rows: Vec<(String, i64, i64)> = {
        let g = db.lock().unwrap();
        let mut st = g
            .prepare("SELECT pack_id,local_id,remote_id FROM transfers WHERE direction='out' AND state='accepted'")
            .map_err(|e| e.to_string())?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        v
    };
    let mut out = Pulled::default();
    if rows.is_empty() {
        return Ok(out);
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.2).collect();
    let state = w.call("items", json!({"ids": ids}))?;
    let list = state["items"].as_array().cloned().unwrap_or_default();
    for (pack_id, local, remote) in rows {
        let Some(it) = list.iter().find(|i| i["id"] == remote) else { continue };
        if it["missing"] == true {
            fail(&db.lock().unwrap(), &pack_id, local, "Сервер потерял задачу")?;
            out.failed.push(local);
            continue;
        }
        let stage = it["stage"].as_str().unwrap_or("");
        match stage {
            "completed" => {
                let dir = data_dir.join("from-server").join(&pack_id);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let outputs = it["outputs"].as_array().cloned().unwrap_or_default();
                let mut files = Vec::new();
                for o in &outputs {
                    let n = o["n"].as_u64().unwrap_or(0);
                    let name = o["name"].as_str().unwrap_or("clip.mp4").to_string();
                    let target = dir.join(&name);
                    download(w, remote, n, &target)?;
                    files.push(target.to_string_lossy().into_owned());
                }
                w.call("ack", json!({"id": remote}))?;
                let note = format!("Готово на сервере: {} шт. в {}", files.len(), dir.display());
                let db = db.lock().unwrap();
                db.execute(
                    "UPDATE factory_items SET owner_node='local',stage='completed',error=?2,updated_at=unixepoch() WHERE id=?1",
                    params![local, note],
                )
                .map_err(|e| e.to_string())?;
                db.execute(
                    "UPDATE transfers SET state='done',detail=?2,updated_at=unixepoch() WHERE pack_id=?1",
                    params![pack_id, json!({"files": files}).to_string()],
                )
                .map_err(|e| e.to_string())?;
                // готовые ролики появляются в «Готовых роликах»: дальше их можно выложить как обычные
                let title: String = db.query_row("SELECT title FROM factory_items WHERE id=?1", [local], |r| r.get(0)).unwrap_or_default();
                super::import::import_outputs(&db, local, &pack_id, &title, &files)?;
                db.execute(
                    "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'stage','completed',?2)",
                    params![local, note],
                )
                .map_err(|e| e.to_string())?;
                out.finished.push(local);
            }
            "failed" | "needs_check" => {
                let why = it["error"].as_str().unwrap_or("ошибка на сервере").to_string();
                fail(&db.lock().unwrap(), &pack_id, local, &format!("На сервере: {why}"))?;
                out.failed.push(local);
            }
            _ => out.waiting += 1,
        }
    }
    Ok(out)
}

fn fail(db: &Connection, pack_id: &str, local: i64, why: &str) -> Result<(), String> {
    db.execute(
        "UPDATE factory_items SET owner_node='local',stage='failed',error=?2,updated_at=unixepoch() WHERE id=?1",
        params![local, why],
    )
    .map_err(|e| e.to_string())?;
    db.execute(
        "UPDATE transfers SET state='failed',detail=?2,updated_at=unixepoch() WHERE pack_id=?1",
        params![pack_id, json!({"error": why}).to_string()],
    )
    .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'attention','failed',?2)",
        params![local, why],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn download(w: &mut dyn Wire, id: i64, n: u64, target: &Path) -> Result<(), String> {
    let part = target.with_extension("part");
    let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut offset = 0u64;
    let sha: Option<String>;
    loop {
        let r = w.call("get", json!({"id": id, "n": n, "offset": offset}))?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(r["data"].as_str().unwrap_or(""))
            .map_err(|e| e.to_string())?;
        f.write_all(&data).map_err(|e| e.to_string())?;
        offset += data.len() as u64;
        let size = r["size"].as_u64().unwrap_or(0);
        if offset >= size {
            sha = r["sha256"].as_str().map(String::from);
            break;
        }
        if data.is_empty() {
            return Err("сервер перестал отдавать данные".into());
        }
    }
    f.flush().map_err(|e| e.to_string())?;
    drop(f);
    let (got, _) = pack::sha256_file(&part).map_err(|e| e.to_string())?;
    if sha.as_deref() != Some(got.as_str()) {
        let _ = std::fs::remove_file(&part);
        return Err("контрольная сумма скачанного ролика не сошлась".into());
    }
    std::fs::rename(&part, target).map_err(|e| e.to_string())
}

/// Возвращает задачу на ПК, если на сервере она ещё не начата.
pub fn recall(db: &Mutex<Connection>, w: &mut dyn Wire, item_id: i64) -> Result<(), String> {
    let (pack_id, remote): (String, i64) = db
        .lock()
        .unwrap()
        .query_row(
            "SELECT pack_id,remote_id FROM transfers WHERE direction='out' AND local_id=?1 AND state='accepted'",
            [item_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "Эта задача не передавалась".to_string())?;
    w.call("cancel", json!({"id": remote}))?;
    let db = db.lock().unwrap();
    pack::take_back(&db, item_id)?;
    db.execute("UPDATE transfers SET state='returned' WHERE pack_id=?1", [pack_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{handle_line, NodeCtx};

    /// Проводник в том же процессе: прогоняет запросы через настоящий разбор протокола сервера.
    struct Local(NodeCtx);
    impl Wire for Local {
        fn call(&mut self, op: &str, args: Value) -> Result<Value, String> {
            let line = serde_json::to_string(&json!({"id": 1, "op": op, "args": args})).unwrap();
            let r = handle_line(&self.0, &line);
            if r.ok { Ok(r.result.unwrap()) } else { Err(r.error.unwrap()) }
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("avto-link-{name}-{}-{}", std::process::id(), rand::random::<u32>()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn server_db(dir: &Path) -> Mutex<Connection> {
        crate::db::open(&dir.join("avtonarezka.db")).unwrap().0
    }

    #[test]
    fn fuse_stops_after_two_failures_and_cools_down() {
        let mut f = Fuse::default();
        assert!(f.allowed(0).is_ok());
        f.record(false, 100);
        assert!(f.allowed(101).is_ok(), "после одной неудачи ещё можно");
        f.record(false, 110);
        assert!(f.allowed(111).unwrap_err().contains("Предохранитель"));
        assert!(f.allowed(110 + COOLDOWN - 1).is_err());
        assert!(f.allowed(110 + COOLDOWN).is_ok());
        f.record(true, 110 + COOLDOWN);
        assert_eq!(f, Fuse::default());
        // успех между неудачами обнуляет счёт
        let mut g = Fuse::default();
        g.record(false, 1);
        g.record(true, 2);
        g.record(false, 3);
        assert!(g.allowed(4).is_ok());
    }

    #[test]
    fn ssh_command_is_locked_down() {
        let c = ServerConfig { host: "h".into(), port: 2222, user: "deploy".into(), key_path: "/k".into(), enabled: true };
        let cmd = ssh_command(&c, Path::new("/kh"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        for need in ["BatchMode=yes", "IdentitiesOnly=yes", "StrictHostKeyChecking=yes", "ForwardAgent=no", "UserKnownHostsFile=/kh", "deploy@h"] {
            assert!(args.iter().any(|a| a == need), "нет {need}");
        }
        assert_eq!(args.last().unwrap(), "serve");
    }

    #[test]
    fn upload_resumes_and_rejects_corruption() {
        let dir = tmp("upload");
        let sdir = tmp("upload-srv");
        let mut w = Local(NodeCtx { data_dir: sdir.clone() });
        let data: Vec<u8> = (0..(CHUNK * 2 + 123)).map(|i| (i % 251) as u8).collect();
        let f = dir.join("big.bin");
        std::fs::write(&f, &data).unwrap();
        let (sha, size) = pack::sha256_file(&f).unwrap();
        let blob = BlobRef { sha256: sha.clone(), size, name: "big.bin".into() };
        let paths = BTreeMap::from([(sha.clone(), f.clone())]);
        // первая половина «долетела», потом обрыв
        let r = w.call("put", json!({"sha256": sha, "total": size, "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(&data[..CHUNK])})).unwrap();
        assert_eq!(r["received"], CHUNK as u64);
        let mut calls = 0;
        upload(&mut w, &[blob.clone()], &paths, &mut |_, _| calls += 1).unwrap();
        assert!(pack::blob_path(&sdir.join("blobs"), &sha).is_file());
        assert_eq!(std::fs::read(pack::blob_path(&sdir.join("blobs"), &sha)).unwrap(), data);
        // повтор ничего не передаёт
        upload(&mut w, &[blob], &paths, &mut |_, _| {}).unwrap();
        // порча: хэш не сходится
        let bad = "a".repeat(64);
        let r = w.call("put", json!({"sha256": bad, "total": 3, "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(b"abc")}));
        assert!(r.unwrap_err().contains("контрольная сумма"));
        assert!(w.call("put", json!({"sha256": "../etc", "total": 1, "offset": 0, "data": ""})).is_err());
    }

    #[test]
    fn item_goes_out_runs_there_and_results_come_back() {
        let pc_dir = tmp("pc");
        let srv_dir = tmp("srv");
        let pc = server_db(&pc_dir);
        let srv = server_db(&srv_dir);
        pc.lock().unwrap().execute("INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,'link','https://youtu.be/abc','Тест','1080',0,'queued')", []).unwrap();
        let item = pc.lock().unwrap().last_insert_rowid();
        let mut w = Local(NodeCtx { data_dir: srv_dir.clone() });
        send_item(&pc, &mut w, item, &mut |_, _| {}).unwrap();
        let owner: String = pc.lock().unwrap().query_row("SELECT owner_node FROM factory_items WHERE id=?1", [item], |r| r.get(0)).unwrap();
        assert_eq!(owner, "server");
        let remote: i64 = srv.lock().unwrap().query_row("SELECT id FROM factory_items", [], |r| r.get(0)).unwrap();

        // пока работа идёт, на ПК ждём
        assert_eq!(pull_results(&pc, &mut w, &pc_dir).unwrap().waiting, 1);

        // «сервер» отрендерил ролик
        let clip = srv_dir.join("final.mp4");
        std::fs::write(&clip, vec![7u8; CHUNK + 5]).unwrap();
        srv.lock().unwrap().execute("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio) VALUES(1,'p','h','t',10,1920,1080,30,1)", []).unwrap();
        srv.lock().unwrap().execute("INSERT INTO moments(id,project_id,start,\"end\") VALUES(1,1,0,30)", []).unwrap();
        srv.lock().unwrap().execute("INSERT INTO drafts(id,moment_id,status,final_path) VALUES(1,1,'final_done',?1)", [clip.to_string_lossy()]).unwrap();
        srv.lock().unwrap().execute("UPDATE factory_items SET project_id=1,stage='completed' WHERE id=?1", [remote]).unwrap();

        let got = pull_results(&pc, &mut w, &pc_dir).unwrap();
        assert_eq!(got.finished, vec![item]);
        let (stage, owner, note): (String, String, Option<String>) = pc
            .lock().unwrap()
            .query_row("SELECT stage,owner_node,error FROM factory_items WHERE id=?1", [item], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!((stage.as_str(), owner.as_str()), ("completed", "local"));
        assert!(note.unwrap().contains("Готово на сервере"));
        let pack_id: String = pc.lock().unwrap().query_row("SELECT pack_id FROM transfers", [], |r| r.get(0)).unwrap();
        let file = pc_dir.join("from-server").join(pack_id).join("final.mp4");
        assert_eq!(std::fs::read(file).unwrap().len(), CHUNK + 5);
        assert!(!clip.exists(), "после подтверждения сервер освобождает место");
        // повторный опрос ничего не делает
        assert_eq!(pull_results(&pc, &mut w, &pc_dir).unwrap(), Pulled::default());
    }

    #[test]
    fn recall_works_only_before_the_server_starts() {
        let (pc_dir, srv_dir) = (tmp("rc-pc"), tmp("rc-srv"));
        let pc = server_db(&pc_dir);
        let srv = server_db(&srv_dir);
        pc.lock().unwrap().execute("INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,'link','https://youtu.be/q','Т','1080',0,'queued')", []).unwrap();
        let item = pc.lock().unwrap().last_insert_rowid();
        let mut w = Local(NodeCtx { data_dir: srv_dir });
        send_item(&pc, &mut w, item, &mut |_, _| {}).unwrap();
        srv.lock().unwrap().execute("UPDATE factory_items SET stage='scouting'", []).unwrap();
        assert!(recall(&pc, &mut w, item).unwrap_err().contains("уже идёт работа"));
        srv.lock().unwrap().execute("UPDATE factory_items SET stage='queued'", []).unwrap();
        recall(&pc, &mut w, item).unwrap();
        let owner: String = pc.lock().unwrap().query_row("SELECT owner_node FROM factory_items WHERE id=?1", [item], |r| r.get(0)).unwrap();
        assert_eq!(owner, "local");
        let n: i64 = srv.lock().unwrap().query_row("SELECT COUNT(*) FROM factory_items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
