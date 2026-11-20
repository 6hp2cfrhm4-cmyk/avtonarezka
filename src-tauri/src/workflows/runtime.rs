//! Хранение сцен и правил, применение плана на этом узле, автопереключение и команды окна.
use std::{sync::atomic::Ordering, time::Duration};

use rusqlite::{params, Connection};
use serde::Serialize;

use super::{
    builtin_scenes, diff, pick_scene, plan, Action, Cond, Context, Flow, Load, NodeKind, NodeState, Rule, Scene,
    PIN_HOURS,
};
use crate::{
    db::{kv_get, kv_set},
    host::{AppHandle, Emitter, Manager, State},
    state::AppState,
};

type CmdResult<T> = Result<T, String>;

const KV_ACTIVE: &str = "scene_active";
const KV_PIN: &str = "scene_pinned_until";
const KV_PEER: &str = "peer_online";
const KV_PLAN: &str = "flows_plan";

pub fn this_node() -> NodeKind {
    if cfg!(feature = "desktop") {
        NodeKind::Pc
    } else {
        NodeKind::Server
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Включён ли поток на этом узле. Потоки автопоиска и live читают это перед тем, как брать новую работу.
pub fn is_enabled(db: &Connection, flow: Flow) -> bool {
    let plan: Vec<(Flow, bool, Option<NodeKind>)> = kv_get(db, KV_PLAN)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    match plan.iter().find(|p| p.0 == flow) {
        Some((_, run, node)) => *run && *node == Some(this_node()),
        // сцена ещё не выбирали: прежнее поведение (всё, что было, работает)
        None => !matches!(flow, Flow::Discovery | Flow::Live | Flow::Offload),
    }
}

// ------------------------------------------------------------------ хранение

pub fn ensure_builtin(db: &Connection) -> rusqlite::Result<()> {
    let n: i64 = db.query_row("SELECT COUNT(*) FROM scenes", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    for s in builtin_scenes() {
        db.execute(
            "INSERT INTO scenes(name,config,builtin) VALUES(?1,?2,1)",
            params![s.name, serde_json::to_string(&s.flows).unwrap()],
        )?;
    }
    let id = |name: &str| -> rusqlite::Result<i64> { db.query_row("SELECT id FROM scenes WHERE name=?1", [name], |r| r.get(0)) };
    let (day, night) = (id("День за ПК")?, id("Ночь на сервере")?);
    for (name, cond, scene) in [
        ("ПК выключен → ночь на сервере", Cond::PcOnline { value: false }, night),
        ("ПК включён → день за ПК", Cond::PcOnline { value: true }, day),
    ] {
        db.execute(
            "INSERT INTO scene_rules(name,cond,scene_id,enabled) VALUES(?1,?2,?3,0)",
            params![name, serde_json::to_string(&cond).unwrap(), scene],
        )?;
    }
    Ok(())
}

pub fn scenes(db: &Connection) -> rusqlite::Result<Vec<Scene>> {
    let mut st = db.prepare("SELECT id,name,config,builtin FROM scenes ORDER BY builtin DESC,id")?;
    let rows = st.query_map([], |r| {
        Ok(Scene {
            id: r.get(0)?,
            name: r.get(1)?,
            flows: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
            builtin: r.get::<_, i64>(3)? != 0,
        })
    })?;
    rows.collect()
}

pub fn rules(db: &Connection) -> rusqlite::Result<Vec<Rule>> {
    let mut st = db.prepare("SELECT id,name,cond,scene_id,enabled FROM scene_rules ORDER BY id")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)? != 0))
    })?;
    Ok(rows
        .flatten()
        .filter_map(|(id, name, cond, scene_id, enabled)| Some(Rule { id, name, cond: serde_json::from_str(&cond).ok()?, scene_id, enabled }))
        .collect())
}

pub fn active_scene(db: &Connection) -> Option<Scene> {
    let all = scenes(db).ok()?;
    let id: i64 = kv_get(db, KV_ACTIVE)?.parse().ok()?;
    all.into_iter().find(|s| s.id == id)
}

fn system_load(dir: &std::path::Path) -> Load {
    let v = crate::node::server::sysinfo(dir);
    Load {
        cpu_psi: v["psi"]["cpu"].as_f64().unwrap_or(0.0),
        mem_psi: v["psi"]["memory"].as_f64().unwrap_or(0.0),
        mem_available_mb: v["mem_available_mb"].as_u64().unwrap_or(0),
        disk_free_mb: v["disk_free_mb"].as_u64().unwrap_or(0),
    }
}

/// Состояние узлов для плана: этот узел и то, что известно о втором.
pub fn node_states(db: &Connection, data_dir: &std::path::Path) -> Vec<NodeState> {
    let me = this_node();
    // ПК видит сервер по успешному ssh; сервер видит ПК по недавним обращениям (их шлёт окно каждые ~90 с)
    let peer_online = if me == NodeKind::Server { crate::node::peer_recent(data_dir) } else { kv_get(db, KV_PEER).as_deref() == Some("1") };
    let mine = NodeState { kind: me, online: true, gpu: me == NodeKind::Pc, browser: me == NodeKind::Pc, load: system_load(data_dir) };
    let other = if me == NodeKind::Pc {
        NodeState { kind: NodeKind::Server, online: peer_online, gpu: false, browser: false, load: Load::default() }
    } else {
        NodeState { kind: NodeKind::Pc, online: peer_online, gpu: true, browser: true, load: Load::default() }
    };
    vec![mine, other]
}

/// Сохраняет план и включает/выключает потоки на этом узле. Работающие задачи не обрываются:
/// выключенный поток просто не берёт новую работу, текущая доходит до границы стадии.
pub fn apply(app: &AppHandle, scene: &Scene, why: &str) -> Vec<Action> {
    let state = app.state::<AppState>();
    let nodes = node_states(&state.db.0.lock().unwrap(), &state.data_dir);
    let new = plan(scene, &nodes);
    let me = this_node();
    let db = state.db.0.lock().unwrap();
    let old: Vec<Action> = kv_get(&db, KV_PLAN)
        .and_then(|j| serde_json::from_str::<Vec<(Flow, bool, Option<NodeKind>)>>(&j).ok())
        .map(|v| {
            v.into_iter()
                .map(|(flow, run, node)| Action { flow, node, run, limit: None, reason: String::new() })
                .collect()
        })
        .unwrap_or_default();
    let (stop, start) = diff(&old, &new);
    let compact: Vec<(Flow, bool, Option<NodeKind>)> = new.iter().map(|a| (a.flow, a.run, a.node)).collect();
    let _ = kv_set(&db, KV_PLAN, &serde_json::to_string(&compact).unwrap());
    for a in &new {
        let here = a.run && a.node == Some(me);
        match a.flow {
            Flow::Monitor => {
                state.monitor.armed.store(here, Ordering::SeqCst);
                state.monitor.wake.notify_one();
            }
            Flow::Publish if !here => {
                let _ = kv_set(&db, "publisher_armed", "0");
            }
            _ => {}
        }
    }
    let _ = db.execute(
        "INSERT INTO scene_log(scene_id,why) VALUES(?1,?2)",
        params![scene.id, format!("{why} (остановлено: {}, запущено: {})", stop.len(), start.len())],
    );
    drop(db);
    let _ = app.emit("scene-changed", ());
    new
}

/// Переключает сцену. Ручное переключение закрепляет её на `PIN_HOURS` часов.
pub fn activate(app: &AppHandle, id: i64, manual: bool, why: &str) -> CmdResult<Vec<Action>> {
    let state = app.state::<AppState>();
    let scene = {
        let db = state.db.0.lock().unwrap();
        let s = scenes(&db).map_err(|e| e.to_string())?.into_iter().find(|s| s.id == id).ok_or("Сцена не найдена")?;
        kv_set(&db, KV_ACTIVE, &id.to_string()).map_err(|e| e.to_string())?;
        if manual {
            kv_set(&db, KV_PIN, &(now() + PIN_HOURS * 3600).to_string()).map_err(|e| e.to_string())?;
        }
        s
    };
    Ok(apply(app, &scene, why))
}

fn context(db: &Connection, data_dir: &std::path::Path) -> Context {
    use chrono::Timelike;
    let nodes = node_states(db, data_dir);
    let me = nodes.iter().find(|n| n.kind == this_node()).cloned();
    let pc = nodes.iter().find(|n| n.kind == NodeKind::Pc).map_or(false, |n| n.online);
    Context {
        hour: chrono::Local::now().hour(),
        pc_online: pc,
        server_busy: me.as_ref().is_some_and(|n| n.kind == NodeKind::Server && n.load.pressure().is_some()),
        disk_low: me.as_ref().is_some_and(|n| n.load.disk_free_mb > 0 && n.load.disk_free_mb < 3072),
        streamer_live: kv_get(db, "streamer_live").as_deref() == Some("1"),
    }
}

/// Раз в 30 секунд проверяет правила и, если нужно, переключает сцену.
pub fn start(app: &AppHandle) {
    {
        let state = app.state::<AppState>();
        let db = state.db.0.lock().unwrap();
        let _ = ensure_builtin(&db);
        // на сервере правила включены сразу: «ПК выключен → ночь» должно работать без человека
        if this_node() == NodeKind::Server && kv_get(&db, "server_rules_default").is_none() {
            let _ = db.execute("UPDATE scene_rules SET enabled=1", []);
            let _ = kv_set(&db, "server_rules_default", "1");
        }
    }
    let app = app.clone();
    crate::host::spawn(async move {
        // восстановить применённую сцену после перезапуска
        let cur = {
            let state = app.state::<AppState>();
            let db = state.db.0.lock().unwrap();
            active_scene(&db)
        };
        if let Some(s) = cur {
            apply(&app, &s, "восстановление после запуска");
        }
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let pick = {
                let state = app.state::<AppState>();
                let db = state.db.0.lock().unwrap();
                let x = context(&db, &state.data_dir);
                let current: i64 = kv_get(&db, KV_ACTIVE).and_then(|s| s.parse().ok()).unwrap_or(0);
                let pinned: i64 = kv_get(&db, KV_PIN).and_then(|s| s.parse().ok()).unwrap_or(0);
                pick_scene(&rules(&db).unwrap_or_default(), &x, current, pinned, now())
            };
            if let Some((id, name)) = pick {
                let _ = activate(&app, id, false, &format!("правило «{name}»"));
            }
        }
    });
}

// ------------------------------------------------------------------ команды окна

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModesState {
    pub node: NodeKind,
    pub scenes: Vec<Scene>,
    pub rules: Vec<Rule>,
    pub active: Option<i64>,
    pub pinned_until: i64,
    pub actions: Vec<Action>,
    pub log: Vec<(i64, String)>,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn modes_state(state: State<'_, AppState>) -> CmdResult<ModesState> {
    let db = state.db.0.lock().unwrap();
    ensure_builtin(&db).map_err(|e| e.to_string())?;
    let all = scenes(&db).map_err(|e| e.to_string())?;
    let active = active_scene(&db);
    let nodes = node_states(&db, &state.data_dir);
    let actions = active.as_ref().map(|s| plan(s, &nodes)).unwrap_or_default();
    let mut st = db.prepare("SELECT at,why FROM scene_log ORDER BY id DESC LIMIT 20").map_err(|e| e.to_string())?;
    let log = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?.flatten().collect();
    Ok(ModesState {
        node: this_node(),
        scenes: all,
        rules: rules(&db).map_err(|e| e.to_string())?,
        active: active.map(|s| s.id),
        pinned_until: kv_get(&db, KV_PIN).and_then(|s| s.parse().ok()).unwrap_or(0),
        actions,
        log,
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn scene_activate(app: AppHandle, id: i64) -> CmdResult<Vec<Action>> {
    activate(&app, id, true, "вручную")
}

/// Снимает закрепление: правила снова могут переключать сцену.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn scene_unpin(state: State<'_, AppState>) {
    let _ = kv_set(&state.db.0.lock().unwrap(), KV_PIN, "0");
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn scene_save(app: AppHandle, scene: Scene) -> CmdResult<Scene> {
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    let cfg = serde_json::to_string(&scene.flows).map_err(|e| e.to_string())?;
    let id = if scene.id == 0 {
        db.execute("INSERT INTO scenes(name,config,builtin) VALUES(?1,?2,0)", params![scene.name.trim(), cfg]).map_err(|e| e.to_string())?;
        db.last_insert_rowid()
    } else {
        if db.execute("UPDATE scenes SET name=?2,config=?3 WHERE id=?1", params![scene.id, scene.name.trim(), cfg]).map_err(|e| e.to_string())? == 0 {
            return Err("Сцена не найдена".into());
        }
        scene.id
    };
    let saved = scenes(&db).map_err(|e| e.to_string())?.into_iter().find(|s| s.id == id).ok_or("Сцена не сохранилась")?;
    let is_active = kv_get(&db, KV_ACTIVE).as_deref() == Some(&id.to_string());
    drop(db);
    if is_active {
        apply(&app, &saved, "сцена изменена");
    }
    let _ = app.emit("scene-changed", ());
    Ok(saved)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn scene_delete(app: AppHandle, id: i64) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    if kv_get(&db, KV_ACTIVE).as_deref() == Some(&id.to_string()) {
        return Err("Активную сцену удалить нельзя: сначала включите другую".into());
    }
    db.execute("DELETE FROM scenes WHERE id=?1 AND builtin=0", [id]).map_err(|e| e.to_string())?;
    drop(db);
    let _ = app.emit("scene-changed", ());
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn rule_save(app: AppHandle, rule: Rule) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let db = state.db.0.lock().unwrap();
    let cond = serde_json::to_string(&rule.cond).map_err(|e| e.to_string())?;
    if rule.id == 0 {
        db.execute("INSERT INTO scene_rules(name,cond,scene_id,enabled) VALUES(?1,?2,?3,?4)", params![rule.name, cond, rule.scene_id, rule.enabled]).map_err(|e| e.to_string())?;
    } else {
        db.execute("UPDATE scene_rules SET name=?2,cond=?3,scene_id=?4,enabled=?5 WHERE id=?1", params![rule.id, rule.name, cond, rule.scene_id, rule.enabled]).map_err(|e| e.to_string())?;
    }
    drop(db);
    let _ = app.emit("scene-changed", ());
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn rule_delete(app: AppHandle, id: i64) -> CmdResult<()> {
    let state = app.state::<AppState>();
    state.db.0.lock().unwrap().execute("DELETE FROM scene_rules WHERE id=?1", [id]).map_err(|e| e.to_string())?;
    let _ = app.emit("scene-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    #[test]
    fn builtin_scenes_and_rules_are_created_once() {
        let c = db();
        ensure_builtin(&c).unwrap();
        ensure_builtin(&c).unwrap();
        assert_eq!(scenes(&c).unwrap().len(), 5);
        let r = rules(&c).unwrap();
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|r| !r.enabled), "автоправила выключены, пока их не включит человек");
        // удаление сцены удаляет её правила
        let night = scenes(&c).unwrap().into_iter().find(|s| s.name == "Ночь на сервере").unwrap().id;
        c.execute("DELETE FROM scenes WHERE id=?1", [night]).unwrap();
        assert_eq!(rules(&c).unwrap().len(), 1);
    }

    #[test]
    fn flows_default_to_legacy_behaviour_until_a_scene_is_chosen() {
        let c = db();
        assert!(is_enabled(&c, Flow::Monitor) && is_enabled(&c, Flow::Studio));
        assert!(!is_enabled(&c, Flow::Live) && !is_enabled(&c, Flow::Discovery));
        let plan = vec![(Flow::Live, true, Some(this_node())), (Flow::Monitor, true, Some(if this_node() == NodeKind::Pc { NodeKind::Server } else { NodeKind::Pc })), (Flow::Studio, false, None)];
        kv_set(&c, KV_PLAN, &serde_json::to_string(&plan).unwrap()).unwrap();
        assert!(is_enabled(&c, Flow::Live));
        assert!(!is_enabled(&c, Flow::Monitor), "поток выбран для другого узла");
        assert!(!is_enabled(&c, Flow::Studio));
    }
}
