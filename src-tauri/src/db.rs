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
    Ok(())
}
