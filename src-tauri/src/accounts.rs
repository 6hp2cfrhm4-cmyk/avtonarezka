//! Пул аккаунтов TikTok для автовыкладки. У каждого аккаунта свой профиль Chromium, в котором человек входит сам:
//! пароли программа не видит и не хранит. В базе только название, профиль и участие в кругу.
//!
//! Основной аккаунт (первый по `position`) унаследовал прежний вход. Через него идут ручные выкладки из диалога и всё,
//! что ушло до появления пула: в суточном счёте такие выкладки записаны без аккаунта.

use std::{collections::HashSet, path::{Path, PathBuf}};

use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::State;

use crate::{state::AppState, tiktok::browser};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// Название аккаунта не длиннее этого (знаков).
const LABEL_MAX: usize = 60;

/// Аккаунт, за которым числятся выкладки без своего аккаунта: первый по порядку.
const MAIN_SQL: &str = "(SELECT id FROM accounts ORDER BY position, id LIMIT 1)";

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountDto {
    pub id: i64,
    pub label: String,
    pub enabled: bool,
    /// Основной аккаунт: см. описание модуля.
    pub main: bool,
    /// Выложено за текущие местные сутки.
    pub sent_today: i64,
}

pub(crate) fn list(db: &Connection, day_start: i64) -> rusqlite::Result<Vec<AccountDto>> {
    let mut st = db.prepare(&format!(
        "SELECT a.id, a.label, a.enabled, a.id = {MAIN_SQL},
                (SELECT count(*) FROM posts p WHERE p.sent_at >= ?1 AND COALESCE(p.account_id, {MAIN_SQL}) = a.id)
         FROM accounts a ORDER BY a.position, a.id"
    ))?;
    let rows = st.query_map([day_start], |r| {
        Ok(AccountDto { id: r.get(0)?, label: r.get(1)?, enabled: r.get::<_, i64>(2)? != 0, main: r.get::<_, i64>(3)? != 0, sent_today: r.get(4)? })
    })?;
    rows.collect()
}

/// Включённые аккаунты в порядке круга.
pub(crate) fn enabled_ids(db: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut st = db.prepare("SELECT id FROM accounts WHERE enabled = 1 ORDER BY position, id")?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

pub(crate) fn main_id(db: &Connection) -> Option<i64> {
    db.query_row(&format!("SELECT {MAIN_SQL}"), [], |r| r.get(0)).ok()
}

/// Выложенные с `from` времена по аккаунтам (выкладки без аккаунта — основному).
pub(crate) fn sent_since(db: &Connection, from: i64) -> rusqlite::Result<Vec<(i64, i64)>> {
    let mut st = db.prepare(&format!("SELECT COALESCE(account_id, {MAIN_SQL}), sent_at FROM posts WHERE sent_at >= ?1"))?;
    let rows = st.query_map([from], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter_map(|(a, t)| a.map(|a| (a, t))).collect())
}

/// Аккаунт, который выложил последний ролик (любой проекта): с него очередь продолжает круг.
pub(crate) fn last_sender(db: &Connection) -> Option<i64> {
    db.query_row(&format!("SELECT COALESCE(account_id, {MAIN_SQL}) FROM posts WHERE sent_at IS NOT NULL ORDER BY sent_at DESC, id DESC LIMIT 1"), [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .ok()
    .flatten()
}

/// Папка профиля аккаунта. Без единого аккаунта — прежняя папка основного входа.
pub(crate) fn profile_dir(db: &Connection, data_dir: &Path, id: Option<i64>) -> Result<PathBuf, String> {
    let Some(id) = id else { return Ok(data_dir.join(LEGACY_PROFILE)) };
    let rel: String = db
        .query_row("SELECT profile FROM accounts WHERE id = ?1", [id], |r| r.get(0))
        .map_err(|_| "Аккаунт не найден: возможно, его уже убрали из списка".to_string())?;
    Ok(data_dir.join(rel))
}

/// Прежняя папка профиля: сюда писал один-единственный вход до появления пула.
pub(crate) const LEGACY_PROFILE: &str = "tiktok-browser";

/// Добавляет аккаунты по названиям. Пустые строки и повторы пропускаются. Возвращает, сколько добавлено.
pub(crate) fn add_bulk(db: &Connection, labels: &[String]) -> Result<usize, String> {
    let mut known: HashSet<String> = {
        let mut st = db.prepare("SELECT lower(label) FROM accounts").map_err(err)?;
        let rows = st.query_map([], |r| r.get(0)).map_err(err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(err)?
    };
    let tx = db.unchecked_transaction().map_err(err)?;
    let mut position: i64 = tx.query_row("SELECT COALESCE(MAX(position), 0) FROM accounts", [], |r| r.get(0)).map_err(err)?;
    let mut added = 0;
    for raw in labels {
        let label: String = raw.trim().chars().take(LABEL_MAX).collect();
        if label.is_empty() || !known.insert(label.to_lowercase()) {
            continue;
        }
        position += 1;
        tx.execute("INSERT INTO accounts (label, profile, position) VALUES (?1, '', ?2)", params![label, position]).map_err(err)?;
        // профиль зависит от номера, поэтому пишем его после вставки
        let id = tx.last_insert_rowid();
        tx.execute("UPDATE accounts SET profile = ?2 WHERE id = ?1", params![id, format!("tiktok-accounts/{id}")]).map_err(err)?;
        added += 1;
    }
    tx.commit().map_err(err)?;
    Ok(added)
}

#[tauri::command]
pub fn accounts_add(state: State<'_, AppState>, labels: Vec<String>) -> CmdResult<usize> {
    add_bulk(&state.db.0.lock().unwrap(), &labels)
}

#[tauri::command]
pub fn accounts_set_enabled(state: State<'_, AppState>, id: i64, enabled: bool) -> CmdResult<()> {
    let n = state.db.0.lock().unwrap().execute("UPDATE accounts SET enabled = ?2 WHERE id = ?1", params![id, enabled as i64]).map_err(err)?;
    if n == 0 {
        return Err("Аккаунт не найден".into());
    }
    Ok(())
}

#[tauri::command]
pub fn accounts_remove(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    // аккаунт, который сейчас выкладывает ролик, убирать нельзя: его запись нужна, чтобы закончить отметку выкладки
    if state.publisher.lock().unwrap().is_some() {
        return Err("Сначала останови очередь: аккаунт может выкладывать ролик прямо сейчас".into());
    }
    let n = state.db.0.lock().unwrap().execute("DELETE FROM accounts WHERE id = ?1", [id]).map_err(err)?;
    if n == 0 {
        return Err("Аккаунт не найден".into());
    }
    Ok(())
}

/// Открывает окно Chromium с профилем аккаунта на странице TikTok: человек входит сам. Ждать входа не нужно,
/// окно остаётся открытым, а очередь потом пользуется тем же профилем.
#[tauri::command]
pub async fn accounts_open_login(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let profile = profile_dir(&state.db.0.lock().unwrap(), &state.data_dir, Some(id))?;
    let chromium = browser::find_chromium().ok_or("Не найден Chromium или Chrome: открыть окно входа нечем")?;
    browser::open_login_window(&chromium, &profile).await.map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    #[test]
    fn the_old_login_becomes_the_main_account() {
        let c = db();
        assert_eq!(main_id(&c), Some(1));
        assert_eq!(profile_dir(&c, Path::new("/d"), Some(1)).unwrap(), Path::new("/d").join("tiktok-browser"));
        assert_eq!(profile_dir(&c, Path::new("/d"), None).unwrap(), Path::new("/d").join("tiktok-browser"), "без аккаунтов — прежняя папка");
        let all = list(&c, 0).unwrap();
        assert_eq!(all, vec![AccountDto { id: 1, label: "Основной".into(), enabled: true, main: true, sent_today: 0 }]);
    }

    #[test]
    fn a_pasted_list_adds_each_account_once_with_its_own_profile() {
        let c = db();
        let labels: Vec<String> = ["  anna ", "", "Boris", "ANNA", "x".repeat(80).as_str()].iter().map(|s| s.to_string()).collect();
        assert_eq!(add_bulk(&c, &labels).unwrap(), 3, "пустая строка и повтор «ANNA» пропускаются, остальные три добавлены");
        let all = list(&c, 0).unwrap();
        assert_eq!(all.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), vec!["Основной", "anna", "Boris", &"x".repeat(60)[..]]);
        assert!(all.iter().skip(1).all(|a| !a.main && a.enabled));
        let anna = all[1].id;
        assert_eq!(profile_dir(&c, Path::new("/d"), Some(anna)).unwrap(), Path::new("/d").join("tiktok-accounts").join(anna.to_string()));
        assert_eq!(enabled_ids(&c).unwrap().len(), 4);
    }

    #[test]
    fn posts_without_an_account_count_for_the_main_one() {
        let c = db();
        add_bulk(&c, &["anna".to_string()]).unwrap();
        let anna = list(&c, 0).unwrap()[1].id;
        c.execute("INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (1, '/v', 'h', 'v', 100, 1, 1, 30, 1)", []).unwrap();
        c.execute("INSERT INTO moments (id, project_id, start, \"end\", status) VALUES (1, 1, 0, 30, 'accepted')", []).unwrap();
        c.execute("INSERT INTO drafts (id, moment_id, part) VALUES (1, 1, 1)", []).unwrap();
        c.execute("INSERT INTO posts (draft_id, method, mode, status, sent_at, account_id) VALUES (1, 'browser', 'browser', 'done', 1000, NULL)", []).unwrap();
        c.execute("INSERT INTO posts (draft_id, method, mode, status, sent_at, account_id) VALUES (1, 'browser', 'browser', 'done', 2000, ?1)", [anna]).unwrap();

        assert_eq!(sent_since(&c, 0).unwrap(), vec![(1, 1000), (anna, 2000)]);
        assert_eq!(last_sender(&c), Some(anna));
        let today = list(&c, 1500).unwrap();
        assert_eq!(today.iter().map(|a| a.sent_today).collect::<Vec<_>>(), vec![0, 1], "вчерашняя выкладка основного не в счёт");
    }
}
