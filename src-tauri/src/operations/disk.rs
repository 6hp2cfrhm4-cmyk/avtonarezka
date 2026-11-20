//! Очистка только зарегистрированных загрузок. Импортированные файлы и финалы сохраняются.
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::{drafts, state::AppState};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Policy {
    pub automatic: bool,
    pub keep_days: u32,
    pub max_gib: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            automatic: false,
            keep_days: 14,
            max_gib: 100,
        }
    }
}
fn load(db: &Connection) -> Policy {
    drafts::kv_get(db, "disk_policy")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn validate(p: &Policy) -> Result<(), String> {
    if !(1..=3650).contains(&p.keep_days) || p.max_gib > 100_000 {
        Err("Хранение: 1–3650 дней; лимит: 0–100000 ГБ (0 — без лимита)".into())
    } else {
        Ok(())
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub path: String,
    pub channel: String,
    pub kind: String,
    pub bytes: u64,
    pub created_at: i64,
    pub reason: Option<String>,
    pub selected: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelUsage {
    pub id: i64,
    pub name: String,
    pub bytes: u64,
    pub protected_bytes: u64,
    pub reclaimable_bytes: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub kind: String,
    pub bytes: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub policy: Policy,
    pub files: Vec<File>,
    pub channels: Vec<ChannelUsage>,
    pub categories: Vec<Usage>,
    pub managed_bytes: u64,
    pub reclaimable_bytes: u64,
    pub selected_bytes: u64,
    pub missing: usize,
    pub limit_unreachable: bool,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cleanup {
    pub deleted: usize,
    pub bytes: u64,
    pub errors: Vec<String>,
}

/// Не удаляем через симлинк, даже если он указывает обратно внутрь sources.
fn owned(root: &Path, path: &Path) -> bool {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    if std::fs::symlink_metadata(root).map_or(true, |m| m.file_type().is_symlink() || !m.is_dir()) {
        return false;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut p = root.to_path_buf();
    for c in rel.components() {
        p.push(c);
        if std::fs::symlink_metadata(&p).map_or(true, |m| m.file_type().is_symlink()) {
            return false;
        }
    }
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}
fn protection(
    db: &Connection,
    path: &str,
    project: Option<i64>,
    item: Option<i64>,
) -> Result<Option<String>, String> {
    let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM factory_items f LEFT JOIN projects p ON p.id=f.project_id WHERE (f.id=?3 OR f.project_id=?2 OR p.path=?1) AND f.stage NOT IN ('published','measured','completed'))",params![path,project,item],|r|r.get(0)).map_err(|e|e.to_string())?;
    if active {
        return Ok(Some("Источник ещё нужен очереди или Inbox".into()));
    }
    let draft:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM drafts d JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id WHERE (p.id=?2 OR p.path=?1) AND d.status NOT IN ('final_done','rejected'))",params![path,project],|r|r.get(0)).map_err(|e|e.to_string())?;
    if draft {
        return Ok(Some("Остались незавершённые черновики".into()));
    }
    let publication:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM publications j JOIN drafts d ON d.id=j.draft_id JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id WHERE (p.id=?2 OR p.path=?1) AND j.state NOT IN ('done','cancelled'))",params![path,project],|r|r.get(0)).map_err(|e|e.to_string())?;
    if publication {
        return Ok(Some("Публикация ещё не завершена".into()));
    }
    let used:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM assets WHERE path=?1 UNION ALL SELECT 1 FROM ready_videos WHERE path=?1 UNION ALL SELECT 1 FROM drafts WHERE final_path=?1 OR proxy_path=?1 OR poster_path=?1)",[path],|r|r.get(0)).map_err(|e|e.to_string())?;
    Ok(used.then(|| "Файл используется библиотекой или готовым роликом".into()))
}
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| tree_bytes(&e.path())).sum())
        .unwrap_or(0)
}

fn snapshot(db: &Connection, data: &Path, now: i64) -> Result<Snapshot, String> {
    let policy = load(db);
    validate(&policy)?;
    let root = data.join("sources");
    let mut st=db.prepare("SELECT f.path,f.project_id,f.item_id,f.kind,f.created_at,COALESCE(c.id,0),COALESCE(c.name,'Без канала') FROM source_files f LEFT JOIN channels c ON c.id=f.channel_id ORDER BY f.created_at,f.path").map_err(|e|e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut files = vec![];
    let mut channels = BTreeMap::<i64, ChannelUsage>::new();
    let mut missing = 0;
    for row in rows {
        let (path, project, item, kind, created_at, id, name) = row.map_err(|e| e.to_string())?;
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing += 1;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let reason = if !owned(&root, Path::new(&path)) {
            Some("Вне папки загрузок или символическая ссылка".into())
        } else if !["original", "proxy", "section", "chat"].contains(&kind.as_str()) {
            Some("Неизвестный тип файла".into())
        } else {
            protection(db, &path, project, item)?
        };
        let bytes = meta.len();
        let c = channels.entry(id).or_insert_with(|| ChannelUsage {
            id,
            name: name.clone(),
            bytes: 0,
            protected_bytes: 0,
            reclaimable_bytes: 0,
        });
        c.bytes += bytes;
        if reason.is_some() {
            c.protected_bytes += bytes;
        } else {
            c.reclaimable_bytes += bytes;
        }
        files.push(File {
            path,
            channel: name,
            kind,
            bytes,
            created_at,
            reason,
            selected: false,
        });
    }
    let managed_bytes = files.iter().map(|f| f.bytes).sum::<u64>();
    let mut remaining = managed_bytes;
    let max = u64::from(policy.max_gib) * 1024 * 1024 * 1024;
    for file in &mut files {
        if file.reason.is_none()
            && (now.saturating_sub(file.created_at) >= i64::from(policy.keep_days) * 86400
                || policy.max_gib > 0 && remaining > max)
        {
            file.selected = true;
            remaining = remaining.saturating_sub(file.bytes);
        }
    }
    let reclaimable_bytes = files
        .iter()
        .filter(|f| f.reason.is_none())
        .map(|f| f.bytes)
        .sum();
    let selected_bytes = files.iter().filter(|f| f.selected).map(|f| f.bytes).sum();
    let mut categories = vec![];
    for (label, dir) in [
        ("Загрузки и прокси", "sources"),
        ("Превью черновиков", "drafts"),
        ("Варианты для площадок", "output-variants"),
        ("Модели", "models"),
        ("Временные файлы", "tmp"),
    ] {
        categories.push(Usage {
            kind: label.into(),
            bytes: tree_bytes(&data.join(dir)),
        });
    }
    let mut outputs = HashSet::new();
    let mut st = db
        .prepare("SELECT final_path FROM drafts WHERE final_path IS NOT NULL")
        .map_err(|e| e.to_string())?;
    for p in st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
    {
        outputs.insert(p.map_err(|e| e.to_string())?);
    }
    categories.push(Usage {
        kind: "Готовые финалы".into(),
        bytes: outputs
            .into_iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum(),
    });
    Ok(Snapshot {
        policy,
        files,
        channels: channels.into_values().collect(),
        categories,
        managed_bytes,
        reclaimable_bytes,
        selected_bytes,
        missing,
        limit_unreachable: max > 0 && remaining > max,
    })
}

fn clean(db: &Connection, data: &Path, now: i64) -> Result<Cleanup, String> {
    let plan = snapshot(db, data, now)?;
    let root = data.join("sources");
    let mut result = Cleanup::default();
    for file in plan.files.into_iter().filter(|f| f.selected) {
        if !owned(&root, Path::new(&file.path)) {
            result
                .errors
                .push("Файл изменился после проверки, очистка пропущена".into());
            continue;
        }
        match std::fs::remove_file(&file.path) {
            Ok(()) => {
                result.deleted += 1;
                result.bytes += file.bytes;
                db.execute("DELETE FROM source_segments WHERE path=?1", [&file.path])
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => result.errors.push(format!(
                "{}: {e}",
                Path::new(&file.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            )),
        }
    }
    if result.deleted > 0 || !result.errors.is_empty() {
        db.execute(
            "INSERT INTO events(kind,message) VALUES('storage',?1)",
            [format!(
                "Очистка загрузок: {} файлов, {} байт; ошибок {}",
                result.deleted,
                result.bytes,
                result.errors.len()
            )],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(result)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn disk_snapshot(app: AppHandle) -> Result<Snapshot, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let result = snapshot(
            &state.db.0.lock().unwrap(),
            &state.data_dir,
            chrono::Utc::now().timestamp(),
        );
        result
    })
    .await
    .map_err(|e| e.to_string())?
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn disk_policy_save(state: State<'_, AppState>, policy: Policy) -> Result<Policy, String> {
    validate(&policy)?;
    drafts::kv_set(
        &state.db.0.lock().unwrap(),
        "disk_policy",
        &serde_json::to_string(&policy).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(policy)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn disk_cleanup(app: AppHandle) -> Result<Cleanup, String> {
    // spawn_blocking продолжает выполняться при отмене вызывающей async-задачи.
    // Блокировка должна жить внутри него до окончания удаления файлов.
    let a = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let state = a.state::<AppState>();
        let _exclusive = state.source_use.blocking_write();
        let result = clean(
            &state.db.0.lock().unwrap(),
            &state.data_dir,
            chrono::Utc::now().timestamp(),
        );
        result
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = app.emit("factory-changed", ());
    Ok(result)
}
pub(crate) fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let automatic = {
                let state = app.state::<AppState>();
                let p = load(&state.db.0.lock().unwrap());
                p.automatic
            };
            if automatic {
                if let Err(e) = disk_cleanup(app.clone()).await {
                    tracing::error!("Очистка диска: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_publication_shared_drafts_and_final_paths_survive_cleanup() {
        let dir = std::env::temp_dir().join(format!("avto-disk-shared-{}", rand::random::<u64>()));
        let sources = dir.join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let path = sources.join("original");
        std::fs::write(&path, [1u8; 12]).unwrap();
        db.execute("INSERT INTO projects(path,hash,title,duration,width,height,fps,has_audio)VALUES(?1,'original','source',10,160,90,30,1)",[path.to_string_lossy()]).unwrap();
        let project = db.last_insert_rowid();
        db.execute(
            "INSERT INTO moments(project_id,start,\"end\")VALUES(?1,0,10)",
            [project],
        )
        .unwrap();
        let moment = db.last_insert_rowid();
        db.execute(
            "INSERT INTO drafts(moment_id,status)VALUES(?1,'approved')",
            [moment],
        )
        .unwrap();
        let draft = db.last_insert_rowid();
        // Старые регистрации могут не содержать project_id; общий path всё равно защищён.
        db.execute(
            "INSERT INTO source_files(path,kind,created_at)VALUES(?1,'original',0)",
            [path.to_string_lossy()],
        )
        .unwrap();
        assert_eq!(snapshot(&db, &dir, 20 * 86400).unwrap().selected_bytes, 0);
        db.execute("UPDATE drafts SET status='final_done' WHERE id=?1", [draft])
            .unwrap();
        db.execute(
            "INSERT INTO publications(draft_id,state)VALUES(?1,'needs_check')",
            [draft],
        )
        .unwrap();
        assert_eq!(clean(&db, &dir, 20 * 86400).unwrap().deleted, 0);
        db.execute("UPDATE publications SET state='done' ", [])
            .unwrap();
        db.execute(
            "UPDATE drafts SET final_path=?1 WHERE id=?2",
            params![path.to_string_lossy(), draft],
        )
        .unwrap();
        assert_eq!(clean(&db, &dir, 20 * 86400).unwrap().deleted, 0);
        db.execute("UPDATE drafts SET final_path=NULL WHERE id=?1", [draft])
            .unwrap();
        db.execute("INSERT INTO source_segments(project_id,start,\"end\",path,duration,width,height,has_audio)VALUES(?1,0,10,?2,10,160,90,1)",params![project,path.to_string_lossy()]).unwrap();
        assert_eq!(clean(&db, &dir, 20 * 86400).unwrap().deleted, 1);
        assert_eq!(
            db.query_row("SELECT count(*) FROM source_segments", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM publications", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM source_files", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn size_limit_uses_oldest_eligible_files_and_reports_protected_overflow() {
        let dir = std::env::temp_dir().join(format!("avto-disk-limit-{}", rand::random::<u64>()));
        let sources = dir.join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let gib = 1024u64 * 1024 * 1024;
        drafts::kv_set(
            &db,
            "disk_policy",
            r#"{"automatic":false,"keepDays":14,"maxGib":1}"#,
        )
        .unwrap();
        for i in 0..3 {
            let path = sources.join(i.to_string());
            std::fs::File::create(&path)
                .unwrap()
                .set_len(gib * 3 / 5)
                .unwrap();
            db.execute(
                "INSERT INTO source_files(path,kind,created_at)VALUES(?1,'proxy',?2)",
                params![path.to_string_lossy(), i * 10],
            )
            .unwrap();
        }
        let s = snapshot(&db, &dir, 30).unwrap();
        assert_eq!(
            s.files
                .iter()
                .filter(|f| f.selected)
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            vec![
                sources.join("0").to_string_lossy().to_string(),
                sources.join("1").to_string_lossy().to_string()
            ]
        );
        assert!(!s.limit_unreachable);
        db.execute(
            "INSERT INTO assets(path,kind,name)VALUES(?1,'gameplay','protected')",
            [sources.join("0").to_string_lossy()],
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(sources.join("0"))
            .unwrap()
            .set_len(gib * 2)
            .unwrap();
        let s = snapshot(&db, &dir, 30).unwrap();
        assert!(s.limit_unreachable);
        assert!(!s.files[0].selected);
        assert!(s.files[1].selected && s.files[2].selected);
        drafts::kv_set(
            &db,
            "disk_policy",
            r#"{"automatic":false,"keepDays":14,"maxGib":0}"#,
        )
        .unwrap();
        assert_eq!(snapshot(&db, &dir, 30).unwrap().selected_bytes, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn retention_protects_active_shared_originals_assets_symlinks_and_finals() {
        let dir = std::env::temp_dir().join(format!("avto-disk-{}", rand::random::<u64>()));
        let sources = dir.join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        for (name, stage) in [
            ("done", "published"),
            ("inbox", "awaiting_approval"),
            ("busy", "rendering_final"),
        ] {
            let path = sources.join(name);
            std::fs::write(&path, [1u8; 10]).unwrap();
            db.execute("INSERT INTO factory_items(kind,source,title,position,stage)VALUES('link','https://example.com',?1,1,?2)",params![name,stage]).unwrap();
            let item = db.last_insert_rowid();
            db.execute(
                "INSERT INTO source_files(path,kind,item_id,created_at)VALUES(?1,'original',?2,0)",
                params![path.to_string_lossy(), item],
            )
            .unwrap();
        }
        let outside = dir.join("outside");
        std::fs::write(&outside, [2u8; 20]).unwrap();
        db.execute(
            "INSERT INTO source_files(path,kind,created_at)VALUES(?1,'original',0)",
            [outside.to_string_lossy()],
        )
        .unwrap();
        let asset = sources.join("asset");
        std::fs::write(&asset, [3u8; 30]).unwrap();
        db.execute(
            "INSERT INTO assets(path,kind,name)VALUES(?1,'banner','banner')",
            [asset.to_string_lossy()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO source_files(path,kind,created_at)VALUES(?1,'proxy',0)",
            [asset.to_string_lossy()],
        )
        .unwrap();
        #[cfg(unix)]
        {
            let link = sources.join("link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            db.execute(
                "INSERT INTO source_files(path,kind,created_at)VALUES(?1,'original',0)",
                [link.to_string_lossy()],
            )
            .unwrap();
        }
        let plan = snapshot(&db, &dir, 20 * 86400).unwrap();
        assert_eq!(plan.selected_bytes, 10);
        assert!(sources.join("done").exists());
        let r = clean(&db, &dir, 20 * 86400).unwrap();
        assert_eq!((r.deleted, r.bytes), (1, 10));
        assert!(!sources.join("done").exists());
        assert!(sources.join("inbox").exists());
        assert!(sources.join("busy").exists());
        assert!(asset.exists());
        assert!(outside.exists());
        assert_eq!(clean(&db, &dir, 20 * 86400).unwrap().deleted, 0);
        assert_eq!(snapshot(&db, &dir, 20 * 86400).unwrap().missing, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
