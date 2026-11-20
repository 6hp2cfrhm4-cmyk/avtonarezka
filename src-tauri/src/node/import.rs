//! Результаты с сервера → «Готовые ролики» на ПК: проект-контейнер, строки `ready_videos` и задачи публикации.
//! Идемпотентно: повторный разбор того же пакета ничего не дублирует.
use std::{path::Path, process::Command};

use rusqlite::{params, Connection};

/// Длительность и размер кадра через ffprobe; без него или при сбое — нули (ролик всё равно добавляется).
fn probe(path: &Path) -> (f64, i64, i64) {
    let Some(tool) = crate::platform::find_tool("ffprobe") else { return (0.0, 0, 0) };
    let Ok(out) = Command::new(tool)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height:format=duration", "-of", "csv=p=0:s=,"])
        .arg(path)
        .output()
    else {
        return (0.0, 0, 0);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let wh: Vec<i64> = lines.next().unwrap_or("").split(',').filter_map(|v| v.trim().parse().ok()).collect();
    let dur: f64 = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0.0);
    (dur, wh.first().copied().unwrap_or(0), wh.get(1).copied().unwrap_or(0))
}

/// Возвращает id проекта-контейнера (или `None`, если файлов нет).
pub fn import_outputs(db: &Connection, local_item: i64, pack_id: &str, title: &str, files: &[String]) -> Result<Option<i64>, String> {
    let Some(first) = files.first() else { return Ok(None) };
    let e = |e: rusqlite::Error| e.to_string();
    let tx = db.unchecked_transaction().map_err(e)?;
    let hash = format!("server:{pack_id}");
    let name = if title.trim().is_empty() { "Ролики с сервера".to_string() } else { format!("{} (с сервера)", title.trim()) };
    tx.execute(
        "INSERT INTO projects (path, hash, title, duration, width, height, fps, has_audio, status) VALUES (?1, ?2, ?3, 0, 0, 0, 30, 1, 'imported') ON CONFLICT(hash) DO NOTHING",
        params![first, hash, name],
    )
    .map_err(e)?;
    let project: i64 = tx.query_row("SELECT id FROM projects WHERE hash=?1", [&hash], |r| r.get(0)).map_err(e)?;
    for f in files {
        let (dur, w, h) = probe(Path::new(f));
        let fname = Path::new(f).file_name().map_or_else(|| f.clone(), |n| n.to_string_lossy().into_owned());
        tx.execute(
            "INSERT INTO ready_videos (project_id, path, name, duration, width, height) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(project_id, path) DO NOTHING",
            params![project, f, fname, dur, w, h],
        )
        .map_err(e)?;
        let rid: i64 = tx.query_row("SELECT id FROM ready_videos WHERE project_id=?1 AND path=?2", params![project, f], |r| r.get(0)).map_err(e)?;
        tx.execute("INSERT INTO publications (ready_id, state) VALUES (?1, 'ready') ON CONFLICT(ready_id) WHERE legacy=1 DO NOTHING", [rid]).map_err(e)?;
    }
    tx.execute("UPDATE factory_items SET project_id=?2 WHERE id=?1", params![local_item, project]).map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(Some(project))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_once_and_links_item() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO factory_items(id,position,kind,source,stage) VALUES(5,1,'link','x','completed')", []).unwrap();
        let files = vec!["/data/from-server/pk/a.mp4".to_string(), "/data/from-server/pk/b.mp4".to_string()];
        let p1 = import_outputs(&db, 5, "pk", "Запись", &files).unwrap().unwrap();
        let p2 = import_outputs(&db, 5, "pk", "Запись", &files).unwrap().unwrap();
        assert_eq!(p1, p2, "повтор не создаёт второй проект");
        let n = |sql: &str| db.query_row::<i64, _, _>(sql, [], |r| r.get(0)).unwrap();
        assert_eq!(n("SELECT COUNT(*) FROM ready_videos"), 2);
        assert_eq!(n("SELECT COUNT(*) FROM publications WHERE state='ready'"), 2);
        assert_eq!(n("SELECT project_id FROM factory_items WHERE id=5"), p1);
        assert_eq!(db.query_row::<String, _, _>("SELECT title FROM projects WHERE id=?1", [p1], |r| r.get(0)).unwrap(), "Запись (с сервера)");
        assert_eq!(import_outputs(&db, 5, "pk2", "", &[]).unwrap(), None);
    }
}
