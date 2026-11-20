//! Команды окна для связи с сервером: настройки, проверка, передача задач, возврат результатов.
//! Сетевая работа идёт в `spawn_blocking`, база блокируется только короткими запросами.
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{json, Value};

use super::{
    link::{self, Fuse, ProcWire, ServerConfig, Wire},
    pack,
};
use crate::{
    host::{AppHandle, Emitter, Manager, State},
    state::AppState,
};

type CmdResult<T> = Result<T, String>;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn known_hosts(state: &AppState) -> PathBuf {
    state.data_dir.join("server_known_hosts")
}

/// Открывает канал с учётом предохранителя и записывает результат попытки.
fn open_wire(app: &AppHandle) -> CmdResult<ProcWire> {
    let state = app.state::<AppState>();
    let cfg = {
        let db = state.db.0.lock().unwrap();
        let cfg = link::load_config(&db).ok_or("Сервер не настроен")?;
        if !cfg.enabled {
            return Err("Связь с сервером выключена".into());
        }
        Fuse::load(&db).allowed(now())?;
        cfg
    };
    if cfg.host.is_empty() || cfg.user.is_empty() || cfg.key_path.is_empty() {
        return Err("Заполните адрес, пользователя и ключ".into());
    }
    let result = ProcWire::spawn(link::ssh_command(&cfg, &known_hosts(&state)));
    let db = state.db.0.lock().unwrap();
    let mut fuse = Fuse::load(&db);
    fuse.record(result.is_ok(), now());
    fuse.save(&db);
    let _ = crate::db::kv_set(&db, "peer_online", if result.is_ok() { "1" } else { "0" });
    result.map_err(|e| e.0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FuseDto {
    pub failures: u32,
    pub blocked: bool,
    pub message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub configured: bool,
    pub enabled: bool,
    pub config: Option<ServerConfig>,
    pub fuse: FuseDto,
    pub online: bool,
    pub error: Option<String>,
    pub remote: Option<Value>,
}

fn fuse_dto(db: &rusqlite::Connection) -> FuseDto {
    let f = Fuse::load(db);
    let msg = f.allowed(now()).err();
    FuseDto {
        failures: f.failures,
        blocked: msg.is_some(),
        message: msg,
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_config_get(state: State<'_, AppState>) -> Option<ServerConfig> {
    link::load_config(&state.db.0.lock().unwrap())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_config_set(state: State<'_, AppState>, config: ServerConfig) -> CmdResult<()> {
    if config.port == 0 {
        return Err("Порт не задан".into());
    }
    link::save_config(&state.db.0.lock().unwrap(), &config)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_fuse_reset(state: State<'_, AppState>) {
    Fuse::default().save(&state.db.0.lock().unwrap());
}

/// Состояние без обращения к серверу (быстро): настройки и предохранитель.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_overview(state: State<'_, AppState>) -> NodeStatus {
    let db = state.db.0.lock().unwrap();
    let cfg = link::load_config(&db);
    NodeStatus {
        configured: cfg.is_some(),
        enabled: cfg.as_ref().is_some_and(|c| c.enabled),
        config: cfg,
        fuse: fuse_dto(&db),
        online: false,
        error: None,
        remote: None,
    }
}

/// Проверка связи: ping и состояние сервера (нагрузка, место, задачи).
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_check(app: AppHandle) -> CmdResult<NodeStatus> {
    let a = app.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut w = open_wire(&a)?;
        w.call("status", json!({}))
    })
    .await
    .map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    let cfg = link::load_config(&db);
    let (online, error, remote) = match res {
        Ok(v) => (true, None, Some(v)),
        Err(e) => (false, Some(e), None),
    };
    Ok(NodeStatus {
        configured: cfg.is_some(),
        enabled: cfg.as_ref().is_some_and(|c| c.enabled),
        config: cfg,
        fuse: fuse_dto(&db),
        online,
        error,
        remote,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKey {
    pub fingerprint: String,
    pub line: String,
}

/// Шаг 1 доверия к серверу: читаем ключ хоста и показываем отпечаток. Ничего не сохраняется.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_hostkey_scan(state: State<'_, AppState>) -> CmdResult<HostKey> {
    let cfg = link::load_config(&state.db.0.lock().unwrap()).ok_or("Сервер не настроен")?;
    tokio::task::spawn_blocking(move || {
        let out = std::process::Command::new("ssh-keyscan")
            .args(["-T", "10", "-t", "ed25519", "-p", &cfg.port.to_string(), &cfg.host])
            .output()
            .map_err(|e| format!("ssh-keyscan не запустился: {e}"))?;
        let line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(str::to_string)
            .ok_or("Сервер не отдал ключ хоста")?;
        let mut child = std::process::Command::new("ssh-keygen")
            .args(["-lf", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), format!("{line}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let fp = child.wait_with_output().map_err(|e| e.to_string())?;
        Ok(HostKey {
            fingerprint: String::from_utf8_lossy(&fp.stdout).trim().to_string(),
            line,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Шаг 2: пользователь сверил отпечаток и подтвердил — ключ записывается в файл приложения.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_hostkey_accept(state: State<'_, AppState>, line: String) -> CmdResult<()> {
    if line.lines().count() != 1 || !line.contains("ssh-ed25519") {
        return Err("Ожидается одна строка с ключом ed25519".into());
    }
    std::fs::write(known_hosts(&state), format!("{}\n", line.trim())).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub stage: String,
    pub can_move: bool,
    pub reason: Option<String>,
    pub owner_node: String,
}

/// Что из очереди можно передать и почему остальное нельзя.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_candidates(state: State<'_, AppState>) -> CmdResult<Vec<Candidate>> {
    let db = state.db.0.lock().unwrap();
    let mut st = db
        .prepare("SELECT id,title,source,stage,owner_node,paused FROM factory_items WHERE stage NOT IN ('completed','failed','cancelled') OR owner_node<>'local' ORDER BY priority DESC,position,id")
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? != 0,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (id, title, source, stage, owner, paused) = row.map_err(|e| e.to_string())?;
        let verdict = pack::movability(&stage, &owner, paused);
        out.push(Candidate {
            id,
            title,
            source,
            stage,
            can_move: verdict.is_ok(),
            reason: verdict.err(),
            owner_node: owner,
        });
    }
    Ok(out)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SendReport {
    pub sent: Vec<i64>,
    pub failed: Vec<(i64, String)>,
}

fn send_all(app: &AppHandle, ids: Vec<i64>) -> Result<SendReport, String> {
    let mut w = open_wire(app)?;
    let state = app.state::<AppState>();
    let mut report = SendReport {
        sent: vec![],
        failed: vec![],
    };
    for id in ids {
        let app2 = app.clone();
        let mut progress = move |done: u64, total: u64| {
            let _ = app2.emit("node-transfer", json!({"id": id, "done": done, "total": total}));
        };
        match link::send_item(&state.db.0, &mut w, id, &mut progress) {
            Ok(_) => report.sent.push(id),
            Err(e) => report.failed.push((id, e)),
        }
    }
    let _ = app.emit("factory-changed", ());
    Ok(report)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_send(app: AppHandle, ids: Vec<i64>) -> CmdResult<SendReport> {
    tokio::task::spawn_blocking(move || send_all(&app, ids))
        .await
        .map_err(|e| e.to_string())?
}

/// Забирает готовые результаты и возвращает сводку.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_pull(app: AppHandle) -> CmdResult<link::Pulled> {
    tokio::task::spawn_blocking(move || {
        let mut w = open_wire(&app)?;
        let state = app.state::<AppState>();
        let r = link::pull_results(&state.db.0, &mut w, &state.data_dir)?;
        let _ = app.emit("factory-changed", ());
        Ok(r)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_recall(app: AppHandle, id: i64) -> CmdResult<()> {
    tokio::task::spawn_blocking(move || {
        let mut w = open_wire(&app)?;
        let state = app.state::<AppState>();
        link::recall(&state.db.0, &mut w, id)?;
        let _ = app.emit("factory-changed", ());
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverReport {
    pub sent: Vec<i64>,
    pub failed: Vec<(i64, String)>,
    /// Всё принято: можно выключать. Иначе ПК остаётся включённым.
    pub safe_to_power_off: bool,
    pub stays_here: Vec<i64>,
}

/// «Передать и выключить ПК», часть 1: передача. Выключение — отдельной командой после обратного отсчёта.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_handover(app: AppHandle, ids: Vec<i64>) -> CmdResult<HandoverReport> {
    tokio::task::spawn_blocking(move || {
        let report = send_all(&app, ids.clone())?;
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        // ещё остаётся работа на этом ПК: выключать нельзя без предупреждения
        let mut st = db
            .prepare("SELECT id FROM factory_items WHERE owner_node='local' AND stage NOT IN ('completed','failed','cancelled','awaiting_approval','awaiting_selection','needs_check') AND paused=0")
            .map_err(|e| e.to_string())?;
        let stays: Vec<i64> = st
            .query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        let safe = report.failed.is_empty() && stays.is_empty();
        if safe {
            let _ = crate::db::kv_set(&db, "node_handover_ok_at", &now().to_string());
        }
        Ok(HandoverReport {
            sent: report.sent,
            failed: report.failed,
            safe_to_power_off: safe,
            stays_here: stays,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Часть 2: выключение. Только если передача недавно (≤ 10 мин) закончилась полностью успешно.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn node_poweroff(state: State<'_, AppState>) -> CmdResult<()> {
    let ok_at: i64 = crate::db::kv_get(&state.db.0.lock().unwrap(), "node_handover_ok_at")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if now() - ok_at > 600 {
        return Err("Передача не подтверждена: ПК не выключаю".into());
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl")
            .arg("poweroff")
            .spawn()
            .map_err(|e| format!("не удалось выключить: {e}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    Err("Автовыключение поддерживается только в Linux".into())
}

// ------------------------------------------------------------------ ключи и настройки интеграций на сервере

/// Группы настроек, которые можно отправить на сервер. Значения секретов уходят только по защищённому
/// каналу ssh и только из этого списка (сервер проверяет его сам).
const PUSH_GROUPS: &[(&str, &[&str], &[&str])] = &[
    ("twitch", &["factory_twitch_app"], &[]),
    ("youtube", &["discovery_youtube_key"], &[]),
    ("kick", &["discovery_kick_token"], &[]),
    ("notify", &["notify_bot_token"], &["notify_config"]),
    ("discovery", &[], &["discovery_config"]),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushReport {
    pub applied: Vec<String>,
    pub skipped: Vec<String>,
}

/// Отправляет выбранные ключи и настройки на сервер: он сохраняет их в своём зашифрованном хранилище.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_config_push(app: AppHandle, groups: Vec<String>) -> CmdResult<PushReport> {
    let state = app.state::<AppState>();
    let mut secrets = serde_json::Map::new();
    let mut kv = serde_json::Map::new();
    let mut skipped = Vec::new();
    for g in &groups {
        let Some((_, sec, cfg)) = PUSH_GROUPS.iter().find(|(n, _, _)| n == g) else {
            return Err(format!("Неизвестная группа настроек: {g}"));
        };
        for name in *sec {
            match state.secrets.get(name).await.map_err(|e| e.to_string())? {
                Some(v) => {
                    secrets.insert((*name).into(), json!(v));
                }
                None => skipped.push(format!("{g}: ключ не задан на ПК")),
            }
        }
        let db = state.db.0.lock().unwrap();
        for name in *cfg {
            if let Some(v) = crate::db::kv_get(&db, name) {
                kv.insert((*name).into(), json!(v));
            }
        }
    }
    if secrets.is_empty() && kv.is_empty() {
        return Err(if skipped.is_empty() { "Нечего отправлять".into() } else { skipped.join("; ") });
    }
    let a = app.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut w = open_wire(&a)?;
        w.call("config_put", json!({"secrets": secrets, "kv": kv}))
    })
    .await
    .map_err(|e| e.to_string())??;
    let applied = res["applied"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    Ok(PushReport { applied, skipped })
}

/// Что настроено на сервере (без значений секретов).
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_config_remote(app: AppHandle) -> CmdResult<Value> {
    tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut w = open_wire(&app)?;
        w.call("config_get", json!({}))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------------ свой сервер: ключ проекта

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerKey {
    pub key_path: String,
    pub public_key: String,
    pub version: String,
    pub created: bool,
}

/// Создаёт отдельный ключ проекта для своего сервера (ed25519, без пароля, права 0600) или возвращает уже созданный.
/// Закрытая часть остаётся только в папке данных приложения; наружу уходит публичная.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn node_keygen(app: AppHandle) -> CmdResult<ServerKey> {
    let state = app.state::<AppState>();
    let path = state.data_dir.join("server_key");
    let pubpath = state.data_dir.join("server_key.pub");
    let version = env!("CARGO_PKG_VERSION").to_string();
    let mut created = false;
    if !path.is_file() || !pubpath.is_file() {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&pubpath);
        let p = path.clone();
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-C", "avtonarezka"]).arg("-f").arg(&p).output()
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "Не найден ssh-keygen: установите пакет openssh".to_string())?;
        if !out.status.success() {
            return Err(format!("ssh-keygen: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        created = true;
    }
    let public_key = std::fs::read_to_string(&pubpath).map_err(|e| e.to_string())?.trim().to_string();
    Ok(ServerKey { key_path: path.to_string_lossy().into_owned(), public_key, version, created })
}
