//! SQLite: проекты, моменты, черновики, библиотека ассетов, настройки.

use std::{path::Path, sync::Mutex};

use rusqlite::Connection;

pub struct Db(pub Mutex<Connection>);

const SCHEMA_V1: &str = r#"
CREATE TABLE projects (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL,
    hash        TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    duration    REAL NOT NULL,
    width       INTEGER NOT NULL,
    height      INTEGER NOT NULL,
    fps         REAL NOT NULL,
    has_audio   INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'imported',
    created_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE moments (
    id            INTEGER PRIMARY KEY,
    project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    start         REAL NOT NULL,
    "end"         REAL NOT NULL,
    score         REAL NOT NULL DEFAULT 0,
    signal_score  REAL NOT NULL DEFAULT 0,
    llm_score     REAL NOT NULL DEFAULT 0,
    title         TEXT NOT NULL DEFAULT '',
    caption       TEXT NOT NULL DEFAULT '',
    hashtags      TEXT NOT NULL DEFAULT '[]',
    climax_t      REAL,
    transcript    TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'candidate',
    manual        INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE drafts (
    id          INTEGER PRIMARY KEY,
    moment_id   INTEGER NOT NULL REFERENCES moments(id) ON DELETE CASCADE,
    part        INTEGER NOT NULL DEFAULT 1,
    style_json  TEXT NOT NULL DEFAULT '{}',
    status      TEXT NOT NULL DEFAULT 'queued',
    proxy_path  TEXT,
    final_path  TEXT,
    error       TEXT,
    updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE assets (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    path       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    meta_json  TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

const SCHEMA_V2: &str = r#"
CREATE TABLE analysis (
    project_id     INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    signals_json   TEXT NOT NULL,
    words_json     TEXT NOT NULL,
    sentences_json TEXT NOT NULL,
    notes_json     TEXT NOT NULL DEFAULT '[]',
    created_at     INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX moments_by_project ON moments(project_id, status);
"#;

const SCHEMA_V3: &str = r#"
ALTER TABLE drafts ADD COLUMN poster_path TEXT;
CREATE INDEX drafts_by_moment ON drafts(moment_id);
"#;

const SCHEMA_V4: &str = r#"
CREATE TABLE posts (
    id         INTEGER PRIMARY KEY,
    draft_id   INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    method     TEXT NOT NULL,
    mode       TEXT NOT NULL,
    status     TEXT NOT NULL,
    detail     TEXT NOT NULL DEFAULT '',
    publish_id TEXT,
    error      TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX posts_by_draft ON posts(draft_id, id);
"#;

/// Видео уже размечено: анализ режет его целиком подряд, а не выбирает лучшие места.
const SCHEMA_V5: &str = r#"
ALTER TABLE projects ADD COLUMN tight INTEGER NOT NULL DEFAULT 0;
"#;

/// Автовыкладка: на каждый готовый ролик одна задача с его описанием и хэштегами.
const SCHEMA_V6: &str = r#"
CREATE TABLE publish_jobs (
    id         INTEGER PRIMARY KEY,
    draft_id   INTEGER NOT NULL UNIQUE REFERENCES drafts(id) ON DELETE CASCADE,
    caption    TEXT NOT NULL DEFAULT '',
    hashtags   TEXT NOT NULL DEFAULT '[]',
    state      TEXT NOT NULL DEFAULT 'drafting',
    error      TEXT,
    attempts   INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER NOT NULL DEFAULT 0,
    sent_at    INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX publish_jobs_by_state ON publish_jobs(state, id);
"#;

/// Пул аккаунтов TikTok для автовыкладки. У каждого свой профиль браузера; первый (`position` 0) — основной,
/// он унаследовал прежний вход. Выкладки помнят свой аккаунт и время выхода: по ним считается суточный лимит.
const SCHEMA_V7: &str = r#"
CREATE TABLE accounts (
    id         INTEGER PRIMARY KEY,
    label      TEXT NOT NULL,
    profile    TEXT NOT NULL DEFAULT '',
    enabled    INTEGER NOT NULL DEFAULT 1,
    position   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
INSERT INTO accounts (label, profile, enabled, position) VALUES ('Основной', 'tiktok-browser', 1, 0);
ALTER TABLE publish_jobs ADD COLUMN account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL;
ALTER TABLE posts ADD COLUMN account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL;
ALTER TABLE posts ADD COLUMN sent_at INTEGER;
-- выкладки, которые уже прошли до пула, тоже считаются в суточном лимите основного аккаунта
UPDATE posts SET sent_at = updated_at
 WHERE status = 'done' AND (detail LIKE 'Опубликовано%' OR detail LIKE 'Нажата кнопка%' OR detail LIKE 'Выложено%');
CREATE INDEX posts_by_sent ON posts(sent_at);
"#;

pub fn open(path: &Path) -> anyhow::Result<Db> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    migrate(&conn)?;
    Ok(Db(Mutex::new(conn)))
}

pub(crate) fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        conn.execute_batch(SCHEMA_V2)?;
        conn.pragma_update(None, "user_version", 2)?;
    }
    if version < 3 {
        conn.execute_batch(SCHEMA_V3)?;
        conn.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        conn.execute_batch(SCHEMA_V4)?;
        conn.pragma_update(None, "user_version", 4)?;
    }
    if version < 5 {
        conn.execute_batch(SCHEMA_V5)?;
        conn.pragma_update(None, "user_version", 5)?;
    }
    if version < 6 {
        conn.execute_batch(SCHEMA_V6)?;
        conn.pragma_update(None, "user_version", 6)?;
    }
    if version < 7 {
        conn.execute_batch(SCHEMA_V7)?;
        conn.pragma_update(None, "user_version", 7)?;
    }
    Ok(())
}
