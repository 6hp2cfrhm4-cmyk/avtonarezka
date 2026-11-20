use crate::host::{AppHandle, Emitter, Manager, State};
use crate::{drafts, factory, state::AppState};
use serde::Serialize;
use std::{
    sync::{atomic::Ordering, Mutex},
    time::Duration,
};

#[derive(Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub active: bool,
    pub needed: bool,
    pub reason: String,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct PowerState(pub Mutex<Status>);

/// Пауза и Inbox не требуют удерживать ПК включённым. Мониторинг и назначенные
/// публикации требуют: иначе приложение пропустит следующий опрос/слот.
fn database_work(
    db: &rusqlite::Connection,
    monitor_armed: bool,
    publisher_armed: bool,
) -> Result<Option<&'static str>, String> {
    let paused = drafts::kv_get(db, "factory_paused").as_deref() == Some("1");
    if !paused {
        let runnable: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM factory_items f LEFT JOIN channels c ON c.id=f.channel_id WHERE f.paused=0 AND COALESCE(c.enabled,1)=1 AND COALESCE(c.rights_paused,0)=0 AND f.stage IN ('queued','fetching','scouting','selecting','drafting','qc','auto_approved','rendering_final','captioning'))", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if runnable {
            return Ok(Some("Обработка источников и роликов"));
        }
        let monitoring: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM channels WHERE enabled=1 AND rights_paused=0 AND COALESCE(json_extract(monitor_options,'$.enabled'),0)=1)", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if monitor_armed && monitoring {
            return Ok(Some("Мониторинг новых записей"));
        }
    }
    if publisher_armed {
        let legacy_or_sending: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM publications WHERE state='sending' OR (legacy=1 AND state='queued'))", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        // Отклонённый/пересобираемый черновик сохраняет слот, но ожидание ручной
        // проверки само по себе не требует держать ПК включённым. Идущий рендер
        // учитывается отдельно в demand, готовые будущие слоты — в next_due.
        let pending = legacy_or_sending
            || !matches!(
                crate::publish::calendar::next_due(db, 0)?,
                crate::publish_queue::Action::Idle
            );
        if pending {
            return Ok(Some("Ожидание слота публикации"));
        }
    }
    Ok(None)
}

fn demand(state: &AppState) -> Result<Option<&'static str>, String> {
    if state.source_use.try_write().is_err() {
        return Ok(Some("Обработка файлов фабрики"));
    }
    if !state.analyses.lock().unwrap().is_empty()
        || !state.renders.lock().unwrap().is_empty()
        || !state.downloads.lock().unwrap().is_empty()
        || !state.posting.lock().unwrap().is_empty()
        || state.camera_scan.lock().unwrap().is_some()
        || state.drafting.lock().unwrap().is_some()
        || state.ml_gate.available_permits() == 0
        || state.models_downloading.load(Ordering::SeqCst)
    {
        return Ok(Some("Анализ, загрузка или рендер"));
    }
    let db = state.db.0.lock().unwrap();
    database_work(
        &db,
        state.monitor.armed.load(Ordering::SeqCst),
        drafts::kv_get(&db, "publisher_armed").as_deref() == Some("1"),
    )
}

#[cfg(target_os = "linux")]
struct Guard(tokio::process::Child);
#[cfg(target_os = "linux")]
impl Guard {
    async fn acquire_at(path: &std::path::Path) -> Result<Self, String> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut cmd = crate::platform::command_at(path);
        // Канал stdin живёт ровно столько, сколько родитель. Даже SIGKILL приложения
        // закрывает его; cat завершается и logind освобождает inhibitor.
        cmd.args([
            "--what=sleep:idle",
            "--mode=block",
            "--who=Автонарезка",
            "--why=Обработка и расписание фабрики контента",
            "--",
            "/bin/sh",
            "-c",
            "printf 'AVTO_INHIBITED\\n'; exec /bin/cat >/dev/null",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Не удалось запустить systemd-inhibit: {e}"))?;
        let mut out = BufReader::new(child.stdout.take().ok_or("Нет ответа systemd-inhibit")?);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), out.read_line(&mut line))
            .await
            .map_err(|_| "logind не подтвердил блокировку сна")?
            .map_err(|e| e.to_string())?;
        if line.trim() != "AVTO_INHIBITED" {
            return Err("logind отклонил блокировку сна. Проверь права systemd-inhibit".into());
        }
        Ok(Self(child))
    }
    async fn acquire() -> Result<Self, String> {
        let path = crate::platform::find_tool("systemd-inhibit")
            .ok_or("Не найдена установленная systemd-inhibit")?;
        Self::acquire_at(&path).await
    }
    fn alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}
#[cfg(windows)]
struct Guard(std::sync::mpsc::Sender<()>);
#[cfg(windows)]
impl Guard {
    async fn acquire() -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready, wait) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            #[link(name = "kernel32")]
            extern "system" {
                fn SetThreadExecutionState(flags: u32) -> u32;
            }
            // SetThreadExecutionState привязан к потоку: установка и снятие в одном.
            let ok = unsafe { SetThreadExecutionState(0x80000001) } != 0;
            let _ = ready.send(ok);
            if ok {
                let _ = rx.recv();
                unsafe {
                    SetThreadExecutionState(0x80000000);
                }
            }
        });
        if wait.await.unwrap_or(false) {
            Ok(Self(tx))
        } else {
            Err("Windows отклонила блокировку сна".into())
        }
    }
    fn alive(&mut self) -> bool {
        true
    }
}
#[cfg(not(any(target_os = "linux", windows)))]
struct Guard;
#[cfg(not(any(target_os = "linux", windows)))]
impl Guard {
    async fn acquire() -> Result<Self, String> {
        Err("Блокировка сна этой ОС не подключена".into())
    }
    fn alive(&mut self) -> bool {
        false
    }
}

pub(crate) fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        let mut guard: Option<Guard> = None;
        let mut retry = std::time::Instant::now();
        loop {
            let state = app.state::<AppState>();
            let enabled = factory::resources::load(&state.db.0.lock().unwrap()).inhibit_sleep;
            let wanted = if enabled { demand(&state) } else { Ok(None) };
            let mut status = Status {
                needed: wanted.as_ref().is_ok_and(Option::is_some),
                reason: wanted
                    .as_ref()
                    .ok()
                    .and_then(|s| *s)
                    .unwrap_or(if enabled {
                        "Фабрика простаивает"
                    } else {
                        "Блокировка сна выключена"
                    })
                    .into(),
                error: wanted.err(),
                active: false,
            };
            if guard.as_mut().is_some_and(|g| !g.alive()) {
                guard = None;
                retry = std::time::Instant::now();
            }
            if !status.needed {
                guard = None;
            } else if guard.is_none() && std::time::Instant::now() >= retry {
                match Guard::acquire().await {
                    Ok(g) => guard = Some(g),
                    Err(e) => {
                        status.error = Some(e);
                        retry = std::time::Instant::now() + Duration::from_secs(60);
                    }
                }
            } else if guard.is_none() {
                status.error = state.power.0.lock().unwrap().error.clone();
            }
            status.active = guard.is_some();
            let changed = {
                let mut old = state.power.0.lock().unwrap();
                if *old == status {
                    false
                } else {
                    *old = status.clone();
                    true
                }
            };
            if changed {
                if let Some(error) = &status.error {
                    tracing::warn!("Защита от сна: {error}");
                    let _ = state.db.0.lock().unwrap().execute(
                        "INSERT INTO events(kind,message) VALUES('attention',?1)",
                        [format!("Защита от сна: {error}")],
                    );
                }
                let _ = app.emit("power-changed", &status);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn power_status(state: State<'_, AppState>) -> Status {
    state.power.0.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inbox_pause_disabled_channels_and_unarmed_publisher_allow_sleep() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let mut c = factory::profiles::Channel {
            name: "Проверка сна".into(),
            source_kind: "folder".into(),
            source: "/tmp".into(),
            ..Default::default()
        };
        c.monitor.enabled = true;
        let c = factory::profiles::save_channel(&db, c).unwrap();
        assert!(database_work(&db, false, false).unwrap().is_none());
        assert_eq!(
            database_work(&db, true, false).unwrap(),
            Some("Мониторинг новых записей")
        );
        drafts::kv_set(&db, "factory_paused", "1").unwrap();
        assert!(database_work(&db, true, false).unwrap().is_none());
        drafts::kv_set(&db, "factory_paused", "0").unwrap();
        db.execute("UPDATE channels SET rights_paused=1 WHERE id=?1", [c.id])
            .unwrap();
        assert!(database_work(&db, true, false).unwrap().is_none());
        db.execute("INSERT INTO factory_items(kind,source,title,position,stage) VALUES('file','/tmp/test','test',1,'awaiting_approval')",[]).unwrap();
        assert!(database_work(&db, false, false).unwrap().is_none());
        db.execute("UPDATE factory_items SET stage='rendering_final'", [])
            .unwrap();
        assert!(database_work(&db, false, false).unwrap().is_some());
        db.execute("UPDATE factory_items SET paused=1", []).unwrap();
        assert!(database_work(&db, false, false).unwrap().is_none());
    }
    #[test]
    fn scheduled_clip_waiting_for_approval_does_not_hold_sleep_but_ready_slot_does() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'v','h','Ролик',30,640,360,25,1); INSERT INTO moments(id,project_id,start,\"end\")VALUES(1,1,0,30); INSERT INTO drafts(id,moment_id,status,final_path)VALUES(1,1,'ready','final.mp4'); INSERT INTO accounts(id,label,platform,external_ref)VALUES(2,'Бот','telegram','@test'); INSERT INTO publications(draft_id,platform,legacy,target_account_id,state,not_before)VALUES(1,'telegram',0,2,'scheduled',10000);").unwrap();
        assert!(database_work(&db, false, true).unwrap().is_none());
        db.execute("UPDATE drafts SET status='final_done'", [])
            .unwrap();
        assert_eq!(
            database_work(&db, false, true).unwrap(),
            Some("Ожидание слота публикации")
        );
        db.execute("UPDATE accounts SET enabled=0 WHERE id=2", [])
            .unwrap();
        assert!(database_work(&db, false, true).unwrap().is_none());
        db.execute("UPDATE publications SET state='sending'", [])
            .unwrap();
        assert!(database_work(&db, false, true).unwrap().is_some());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn handshake_failure_and_parent_pipe_lifetime() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("avto-power-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("inhibit");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'AVTO_INHIBITED\\n'\nexec /bin/cat >/dev/null\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut g = Guard::acquire_at(&script).await.unwrap();
        assert!(g.alive());
        let stdin = g.0.stdin.take();
        drop(stdin);
        assert!(tokio::time::timeout(Duration::from_secs(3), g.0.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(Guard::acquire_at(&script).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
