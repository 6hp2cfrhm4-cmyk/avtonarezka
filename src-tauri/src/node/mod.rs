//! Канал «ПК ↔ сервер»: строки JSON по stdio (команда, которую ssh запускает на сервере через ограниченный ключ).
//!
//! Один запрос на строку: `{"id":1,"op":"ping"}`, ответ `{"id":1,"ok":true,"result":{…}}` или
//! `{"id":1,"ok":false,"error":"…"}`. Разбор и ответы — чистые функции без сети и без процесса, чтобы
//! их можно было проверить тестами; бинарник `avtoctl` только читает строки и печатает ответы.
use std::path::{Path, PathBuf};

pub mod cmds;
pub mod import;
pub mod link;
pub mod pack;
pub mod runner;
pub mod server;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Версия протокола канала; клиент и сервер сверяют её в `ping`.
pub const PROTOCOL: u32 = 1;
/// Больше строки не читаем: ограничитель против мусора на входе.
pub const MAX_LINE: usize = 1 << 20;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    fn err(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

/// Что известно узлу о себе. Для запросов нужен только каталог данных.
pub struct NodeCtx {
    pub data_dir: PathBuf,
}

/// Файл «ПК недавно обращался»: по нему демон понимает, что ПК включён и приложение запущено.
pub const PEER_SEEN: &str = "peer_seen";
/// Сколько ПК считается «на связи» после последнего обращения.
pub const PEER_FRESH_SECS: u64 = 240;

fn touch_seen(dir: &Path) {
    let path = dir.join(PEER_SEEN);
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() < 10);
    if !fresh {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = std::fs::write(path, now.to_string());
    }
}

/// ПК обращался к этому узлу в последние минуты.
pub fn peer_recent(dir: &Path) -> bool {
    std::fs::metadata(dir.join(PEER_SEEN)).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() < PEER_FRESH_SECS)
}

/// Обрабатывает одну строку; плохая строка даёт ответ с ошибкой, а не обрыв канала.
pub fn handle_line(ctx: &NodeCtx, line: &str) -> Response {
    if line.len() > MAX_LINE {
        return Response::err(0, "строка слишком длинная");
    }
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return Response::err(0, format!("не разобрать запрос: {e}")),
    };
    touch_seen(&ctx.data_dir);
    match req.op.as_str() {
        "ping" => Response::ok(
            req.id,
            json!({"protocol": PROTOCOL, "version": env!("CARGO_PKG_VERSION"), "node": "server"}),
        ),
        "status" => match status(&ctx.data_dir) {
            Ok(mut v) => {
                v["system"] = server::sysinfo(&ctx.data_dir);
                Response::ok(req.id, v)
            }
            Err(e) => Response::err(req.id, format!("{e:#}")),
        },
        op @ ("have" | "put" | "submit" | "items" | "get" | "cancel" | "ack" | "config_put" | "config_get") => {
            let store = server::Store {
                data_dir: ctx.data_dir.clone(),
            };
            let r = match op {
                "have" => server::have(&store, &req.args),
                "put" => server::put(&store, &req.args),
                "submit" => server::submit(&store, &req.args),
                "items" => server::items(&store, &req.args),
                "get" => server::get(&store, &req.args),
                "cancel" => server::cancel(&store, &req.args),
                "config_put" => server::config_put(&store, &req.args),
                "config_get" => server::config_get(&store, &req.args),
                _ => server::ack(&store, &req.args),
            };
            match r {
                Ok(v) => Response::ok(req.id, v),
                Err(e) => Response::err(req.id, e),
            }
        }
        other => Response::err(req.id, format!("неизвестная операция: {other}")),
    }
}

/// Сводка по очереди: сколько задач на каждой стадии. База открывается только на чтение.
fn status(data_dir: &Path) -> anyhow::Result<Value> {
    let db_path = data_dir.join("avtonarezka.db");
    if !db_path.is_file() {
        return Ok(json!({"db": false, "stages": {}}));
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut stages = serde_json::Map::new();
    let mut stmt = conn.prepare("SELECT stage, COUNT(*) FROM factory_items GROUP BY stage")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (stage, n) = row?;
        stages.insert(stage, json!(n));
    }
    Ok(json!({"db": true, "schema": version, "stages": stages}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> NodeCtx {
        NodeCtx {
            data_dir: dir.to_path_buf(),
        }
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "avn-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn any_request_marks_the_pc_as_recently_seen() {
        let d = tmp();
        assert!(!peer_recent(&d), "до первого обращения ПК не на связи");
        handle_line(&ctx(&d), r#"{"id":1,"op":"ping"}"#);
        assert!(peer_recent(&d));
        // кривая строка обращением не считается
        let d2 = tmp();
        handle_line(&ctx(&d2), "не json");
        assert!(!peer_recent(&d2));
    }

    #[test]
    fn ping_reports_protocol() {
        let r = handle_line(&ctx(&tmp()), r#"{"id":7,"op":"ping"}"#);
        assert!(r.ok && r.id == 7);
        assert_eq!(r.result.unwrap()["protocol"], PROTOCOL);
    }

    #[test]
    fn garbage_and_unknown_ops_are_errors_not_panics() {
        let c = ctx(&tmp());
        assert!(!handle_line(&c, "not json").ok);
        assert!(!handle_line(&c, r#"{"id":1,"op":"rm -rf"}"#).ok);
        assert!(!handle_line(&c, &"x".repeat(MAX_LINE + 1)).ok);
    }

    #[test]
    fn status_without_database_and_with_real_database() {
        let dir = tmp();
        let r = handle_line(&ctx(&dir), r#"{"id":1,"op":"status"}"#);
        assert_eq!(r.result.unwrap()["db"], false);
        let db = crate::db::open(&dir.join("avtonarezka.db")).unwrap();
        drop(db);
        let r = handle_line(&ctx(&dir), r#"{"id":2,"op":"status"}"#);
        let v = r.result.unwrap();
        assert_eq!(v["db"], true);
        assert!(v["schema"].as_i64().unwrap() >= 16);
    }
}

/// Для проверок на двух процессах: открывает (и при необходимости мигрирует) базу узла.
#[doc(hidden)]
pub fn open_database(path: &Path) -> anyhow::Result<std::sync::Mutex<Connection>> {
    Ok(crate::db::open(path)?.0)
}
