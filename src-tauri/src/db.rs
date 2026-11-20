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

/// Пачка: ссылки и файлы, которые раннер по очереди скачивает, нарезает и доводит до черновиков.
/// `stage` — где элемент сейчас; `project_id` появляется, когда видео добавлено в проекты.
const SCHEMA_V8: &str = r#"
CREATE TABLE batch_items (
    id         INTEGER PRIMARY KEY,
    position   INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    source     TEXT NOT NULL,
    title      TEXT NOT NULL DEFAULT '',
    preset     TEXT NOT NULL DEFAULT '1080',
    precut     INTEGER NOT NULL DEFAULT 0,
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    stage      TEXT NOT NULL DEFAULT 'queued',
    error      TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX batch_items_by_stage ON batch_items(stage, position);
"#;

/// Готовые ролики из папки: уже отрендеренные 1080×1920, которые выкладываются без рендера.
/// Задача и выкладка теперь могут относиться к ролику, а не к черновику: поэтому `draft_id` и в `publish_jobs`, и в `posts`
/// становится необязательным (таблицы пересобираются, у SQLite нельзя снять NOT NULL через ALTER).
/// `wanted_account_id` — аккаунт, который человек выбрал для ролика вручную (`NULL` — по кругу).
const SCHEMA_V9: &str = r#"
CREATE TABLE ready_videos (
    id         INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    name       TEXT NOT NULL DEFAULT '',
    duration   REAL NOT NULL DEFAULT 0,
    width      INTEGER NOT NULL DEFAULT 0,
    height     INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (project_id, path)
);
CREATE TABLE publish_jobs_v9 (
    id                INTEGER PRIMARY KEY,
    draft_id          INTEGER UNIQUE REFERENCES drafts(id) ON DELETE CASCADE,
    ready_id          INTEGER UNIQUE REFERENCES ready_videos(id) ON DELETE CASCADE,
    caption           TEXT NOT NULL DEFAULT '',
    hashtags          TEXT NOT NULL DEFAULT '[]',
    state             TEXT NOT NULL DEFAULT 'drafting',
    error             TEXT,
    attempts          INTEGER NOT NULL DEFAULT 0,
    not_before        INTEGER NOT NULL DEFAULT 0,
    sent_at           INTEGER,
    created_at        INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at        INTEGER NOT NULL DEFAULT (unixepoch()),
    account_id        INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    wanted_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    CHECK (draft_id IS NOT NULL OR ready_id IS NOT NULL)
);
INSERT INTO publish_jobs_v9 (id, draft_id, caption, hashtags, state, error, attempts, not_before, sent_at, created_at, updated_at, account_id)
    SELECT id, draft_id, caption, hashtags, state, error, attempts, not_before, sent_at, created_at, updated_at, account_id FROM publish_jobs;
DROP TABLE publish_jobs;
ALTER TABLE publish_jobs_v9 RENAME TO publish_jobs;
CREATE INDEX publish_jobs_by_state ON publish_jobs(state, id);
CREATE TABLE posts_v9 (
    id         INTEGER PRIMARY KEY,
    draft_id   INTEGER REFERENCES drafts(id) ON DELETE CASCADE,
    ready_id   INTEGER REFERENCES ready_videos(id) ON DELETE CASCADE,
    method     TEXT NOT NULL,
    mode       TEXT NOT NULL,
    status     TEXT NOT NULL,
    detail     TEXT NOT NULL DEFAULT '',
    publish_id TEXT,
    error      TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    sent_at    INTEGER
);
INSERT INTO posts_v9 (id, draft_id, method, mode, status, detail, publish_id, error, created_at, updated_at, account_id, sent_at)
    SELECT id, draft_id, method, mode, status, detail, publish_id, error, created_at, updated_at, account_id, sent_at FROM posts;
DROP TABLE posts;
ALTER TABLE posts_v9 RENAME TO posts;
CREATE INDEX posts_by_draft ON posts(draft_id, id);
CREATE INDEX posts_by_sent ON posts(sent_at);
"#;

/// Профили и единая очередь фабрики. Сохраняем id и связи прежней пачки.
const SCHEMA_V10: &str = r#"
CREATE TABLE templates (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    layout TEXT NOT NULL DEFAULT 'gameplay_stack',
    config TEXT NOT NULL DEFAULT '{}',
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE UNIQUE INDEX templates_default ON templates(is_default) WHERE is_default = 1;
CREATE TABLE channels (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    source_kind TEXT NOT NULL DEFAULT 'file',
    source TEXT NOT NULL DEFAULT '',
    template_id INTEGER REFERENCES templates(id) ON DELETE SET NULL,
    mode TEXT NOT NULL DEFAULT 'review' CHECK(mode IN ('auto','review','manual')),
    enabled INTEGER NOT NULL DEFAULT 1,
    analysis TEXT NOT NULL DEFAULT '{}',
    score_threshold REAL NOT NULL DEFAULT 0.5 CHECK(score_threshold BETWEEN 0 AND 1),
    max_clips INTEGER NOT NULL DEFAULT 5 CHECK(max_clips BETWEEN 1 AND 60),
    permission TEXT NOT NULL DEFAULT 'unspecified',
    credit TEXT NOT NULL DEFAULT '',
    targets TEXT NOT NULL DEFAULT '[]',
    campaign_ids TEXT NOT NULL DEFAULT '[]',
    cam_rect TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE source_items (
    id INTEGER PRIMARY KEY,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    external_id TEXT NOT NULL,
    source TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    duration REAL,
    discovered_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(channel_id, external_id)
);
ALTER TABLE batch_items RENAME TO factory_items;
ALTER TABLE factory_items ADD COLUMN channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL;
ALTER TABLE factory_items ADD COLUMN source_item_id INTEGER REFERENCES source_items(id) ON DELETE SET NULL;
ALTER TABLE factory_items ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
ALTER TABLE factory_items ADD COLUMN qc_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE factory_items ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
ALTER TABLE factory_items ADD COLUMN profile_json TEXT;
UPDATE factory_items SET stage = CASE stage
    WHEN 'downloading' THEN 'fetching' WHEN 'importing' THEN 'fetching'
    WHEN 'analyzing' THEN 'scouting' WHEN 'rendering' THEN 'rendering_final'
    WHEN 'done' THEN 'completed' ELSE stage END;
DROP INDEX batch_items_by_stage;
CREATE INDEX factory_items_by_stage ON factory_items(stage, priority DESC, position, id);
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    item_id INTEGER REFERENCES factory_items(id) ON DELETE SET NULL,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    stage TEXT,
    message TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX events_recent ON events(id DESC);
"#;

const SCHEMA_V11: &str = r#"
ALTER TABLE channels ADD COLUMN monitor_options TEXT NOT NULL DEFAULT '{}';
ALTER TABLE channels ADD COLUMN last_checked_at INTEGER;
ALTER TABLE channels ADD COLUMN next_check_at INTEGER;
ALTER TABLE channels ADD COLUMN source_error TEXT;
ALTER TABLE source_items ADD COLUMN state TEXT NOT NULL DEFAULT 'discovered';
ALTER TABLE source_items ADD COLUMN filter_reason TEXT;
ALTER TABLE source_items ADD COLUMN published_at INTEGER;
CREATE UNIQUE INDEX factory_source_item_unique ON factory_items(source_item_id) WHERE source_item_id IS NOT NULL;
CREATE TABLE source_segments (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    start REAL NOT NULL,
    "end" REAL NOT NULL,
    path TEXT NOT NULL,
    duration REAL NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    has_audio INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(project_id,start,"end"),
    CHECK(start>=0 AND "end">start)
);
CREATE TABLE source_files (
    path TEXT PRIMARY KEY,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    item_id INTEGER REFERENCES factory_items(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
"#;

const SCHEMA_V12: &str = r#"
CREATE TABLE analysis_sources (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    chat_json TEXT,
    clips_json TEXT,
    chat_attempted INTEGER NOT NULL DEFAULT 0,
    chat_error TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
"#;

/// Единственное хранилище очереди. `legacy` отделяет прежнюю студийную задачу
/// с ротацией аккаунтов от публикаций с явно назначенной целью. История попыток
/// сохраняет старые id; её строки не исполняются как отдельные задачи.
const SCHEMA_V13: &str = r#"
ALTER TABLE accounts ADD COLUMN platform TEXT NOT NULL DEFAULT 'tiktok';
ALTER TABLE accounts ADD COLUMN external_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN schedule_json TEXT NOT NULL DEFAULT '{}';
CREATE TABLE publications (
    id INTEGER PRIMARY KEY,
    draft_id INTEGER REFERENCES drafts(id) ON DELETE CASCADE,
    ready_id INTEGER REFERENCES ready_videos(id) ON DELETE CASCADE,
    platform TEXT NOT NULL DEFAULT 'tiktok' CHECK(platform IN ('tiktok','telegram','youtube','instagram','vk')),
    legacy INTEGER NOT NULL DEFAULT 1 CHECK(legacy IN (0,1)),
    target_account_id INTEGER REFERENCES accounts(id) ON DELETE RESTRICT,
    caption TEXT NOT NULL DEFAULT '',
    hashtags TEXT NOT NULL DEFAULT '[]',
    state TEXT NOT NULL DEFAULT 'drafting',
    error TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER NOT NULL DEFAULT 0,
    sent_at INTEGER,
    external_id TEXT,
    url TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    wanted_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    CHECK ((draft_id IS NOT NULL) != (ready_id IS NOT NULL)),
    CHECK (legacy = 1 OR target_account_id IS NOT NULL),
    CHECK (legacy = 0 OR platform = 'tiktok')
);
CREATE UNIQUE INDEX publications_legacy_draft ON publications(draft_id) WHERE legacy=1;
CREATE UNIQUE INDEX publications_legacy_ready ON publications(ready_id) WHERE legacy=1;
CREATE UNIQUE INDEX publications_target_draft ON publications(draft_id,platform,target_account_id) WHERE legacy=0;
CREATE UNIQUE INDEX publications_target_ready ON publications(ready_id,platform,target_account_id) WHERE legacy=0;
CREATE UNIQUE INDEX publications_external ON publications(platform,target_account_id,external_id) WHERE external_id IS NOT NULL AND legacy=0;
CREATE INDEX publications_due ON publications(state,not_before,id);
INSERT INTO publications(id,draft_id,ready_id,caption,hashtags,state,error,attempts,not_before,sent_at,created_at,updated_at,account_id,wanted_account_id)
 SELECT id,draft_id,ready_id,caption,hashtags,state,error,attempts,not_before,sent_at,created_at,updated_at,account_id,wanted_account_id FROM publish_jobs;
DROP TABLE publish_jobs;
ALTER TABLE posts RENAME TO publication_attempts;
ALTER TABLE publication_attempts ADD COLUMN publication_id INTEGER REFERENCES publications(id) ON DELETE SET NULL;
ALTER TABLE publication_attempts ADD COLUMN platform TEXT NOT NULL DEFAULT 'tiktok';
-- Переносим и ручные отправки, которых не было в очереди. Не теряем даже
-- неопределённые результаты: они требуют проверки и не могут уйти повторно.
INSERT INTO publications(draft_id,ready_id,state,error,sent_at,external_id,created_at,updated_at,account_id)
 SELECT p.draft_id,p.ready_id,
 CASE WHEN p.sent_at IS NOT NULL THEN 'done'
      WHEN p.status='uploading' OR p.status='failed' THEN 'needs_check' ELSE 'ready' END,
 p.error,p.sent_at,p.publish_id,p.created_at,p.updated_at,p.account_id
 FROM publication_attempts p
 WHERE (p.draft_id IS NOT NULL OR p.ready_id IS NOT NULL)
 AND p.id=(SELECT MAX(h.id) FROM publication_attempts h WHERE h.draft_id=p.draft_id OR h.ready_id=p.ready_id)
 AND NOT EXISTS(SELECT 1 FROM publications j WHERE j.draft_id=p.draft_id OR j.ready_id=p.ready_id);
UPDATE publication_attempts SET publication_id=(SELECT j.id FROM publications j WHERE j.legacy=1 AND (j.draft_id=publication_attempts.draft_id OR j.ready_id=publication_attempts.ready_id));
UPDATE publications SET external_id=(SELECT h.publish_id FROM publication_attempts h WHERE h.publication_id=publications.id AND h.publish_id IS NOT NULL ORDER BY h.id DESC LIMIT 1);
CREATE INDEX publication_attempts_by_publication ON publication_attempts(publication_id,id);
"#;

const SCHEMA_V14: &str = r#"
ALTER TABLE channels ADD COLUMN complaint_limit INTEGER NOT NULL DEFAULT 3 CHECK(complaint_limit BETWEEN 1 AND 100);
ALTER TABLE channels ADD COLUMN rights_paused INTEGER NOT NULL DEFAULT 0 CHECK(rights_paused IN (0,1));
CREATE TABLE channel_incidents (
    id INTEGER PRIMARY KEY,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    publication_id INTEGER REFERENCES publications(id) ON DELETE SET NULL,
    kind TEXT NOT NULL CHECK(kind IN ('complaint','restriction')),
    note TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    resolved_at INTEGER
);
CREATE INDEX channel_incidents_active ON channel_incidents(channel_id,resolved_at,id);
"#;

const SCHEMA_V15: &str = r#"
CREATE TABLE campaigns (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    advertiser TEXT NOT NULL,
    config TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE campaign_banners (
    campaign_id INTEGER NOT NULL REFERENCES campaigns(id) ON DELETE CASCADE,
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE RESTRICT,
    PRIMARY KEY(campaign_id,asset_id)
);
CREATE TABLE clip_campaigns (
    draft_id INTEGER PRIMARY KEY REFERENCES drafts(id) ON DELETE CASCADE,
    campaign_id INTEGER NOT NULL REFERENCES campaigns(id) ON DELETE RESTRICT,
    banner_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE RESTRICT,
    snapshot TEXT NOT NULL,
    assigned_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX clip_campaigns_history ON clip_campaigns(campaign_id,assigned_at,draft_id);
"#;

const SCHEMA_V16: &str = r#"
CREATE TABLE metrics (
    id INTEGER PRIMARY KEY,
    publication_id INTEGER NOT NULL REFERENCES publications(id) ON DELETE CASCADE,
    observed_at INTEGER NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('api','manual')),
    views INTEGER CHECK(views>=0), likes INTEGER CHECK(likes>=0),
    comments INTEGER CHECK(comments>=0), shares INTEGER CHECK(shares>=0),
    CHECK(views IS NOT NULL OR likes IS NOT NULL OR comments IS NOT NULL OR shares IS NOT NULL),
    UNIQUE(publication_id,observed_at,source)
);
CREATE INDEX metrics_publication_time ON metrics(publication_id,observed_at DESC,id DESC);
CREATE TABLE metric_sync (
    publication_id INTEGER PRIMARY KEY REFERENCES publications(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL, next_at INTEGER NOT NULL, error TEXT
);
CREATE TABLE moment_features (
    draft_id INTEGER PRIMARY KEY REFERENCES drafts(id) ON DELETE CASCADE,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    template_id INTEGER REFERENCES templates(id) ON DELETE SET NULL,
    features TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
"#;

/// Узлы и передача задач между ПК и сервером. `owner_node='local'` — задачей занимается этот узел;
/// любое другое значение — она передана и здесь не обрабатывается (у задачи ровно один владелец).
const SCHEMA_V17: &str = r#"
CREATE TABLE nodes (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    last_seen INTEGER,
    info TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE transfers (
    pack_id TEXT PRIMARY KEY,
    direction TEXT NOT NULL CHECK(direction IN ('out','in')),
    kind TEXT NOT NULL CHECK(kind IN ('item')),
    local_id INTEGER,
    remote_id INTEGER,
    state TEXT NOT NULL DEFAULT 'prepared',
    detail TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX transfers_by_state ON transfers(direction, state);
ALTER TABLE factory_items ADD COLUMN owner_node TEXT NOT NULL DEFAULT 'local';
ALTER TABLE factory_items ADD COLUMN origin_pack TEXT;
ALTER TABLE publications ADD COLUMN owner_node TEXT NOT NULL DEFAULT 'local';
CREATE INDEX factory_items_by_owner ON factory_items(owner_node, stage);
"#;

/// Режимы: сцены (наборы потоков), правила автопереключения и журнал переключений.
const SCHEMA_V18: &str = r#"
CREATE TABLE scenes (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK(length(trim(name)) > 0),
    config TEXT NOT NULL,
    builtin INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE scene_rules (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    cond TEXT NOT NULL,
    scene_id INTEGER NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE scene_log (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL DEFAULT (unixepoch()),
    scene_id INTEGER,
    why TEXT NOT NULL
);
"#;

/// Автопоиск: снимки популярности для расчёта скорости роста и найденные кандидаты.
const SCHEMA_V19: &str = r#"
CREATE TABLE discovery_snapshots (
    key TEXT NOT NULL,
    taken_at INTEGER NOT NULL,
    metric REAL NOT NULL,
    PRIMARY KEY(key, taken_at)
);
CREATE TABLE discovery_candidates (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('live','vod','clip')),
    platform TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    category TEXT NOT NULL DEFAULT '',
    language TEXT NOT NULL DEFAULT '',
    channel_name TEXT NOT NULL DEFAULT '',
    metric REAL NOT NULL DEFAULT 0,
    previous REAL NOT NULL DEFAULT 0,
    window_min REAL NOT NULL DEFAULT 0,
    duration_min REAL,
    score REAL NOT NULL DEFAULT 0,
    why TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'new' CHECK(state IN ('new','accepted','rejected','watch')),
    auto INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER NOT NULL DEFAULT (unixepoch()),
    last_seen INTEGER NOT NULL DEFAULT (unixepoch()),
    decided_at INTEGER,
    source_item_id INTEGER
);
CREATE INDEX discovery_candidates_by_state ON discovery_candidates(state, score DESC);
"#;

/// Live-нарезка: слежение за эфирами и найденные моменты.
const SCHEMA_V20: &str = r#"
CREATE TABLE live_sessions (
    id INTEGER PRIMARY KEY,
    platform TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL,
    channel TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'starting' CHECK(state IN ('starting','live','ended','stopped','failed')),
    manual INTEGER NOT NULL DEFAULT 0,
    chat INTEGER NOT NULL DEFAULT 0,
    started_at INTEGER NOT NULL DEFAULT (unixepoch()),
    ended_at INTEGER,
    note TEXT NOT NULL DEFAULT '',
    moments INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE live_moments (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES live_sessions(id) ON DELETE CASCADE,
    start_t REAL NOT NULL,
    end_t REAL NOT NULL,
    peak_t REAL NOT NULL,
    score REAL NOT NULL DEFAULT 0,
    manual INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'cut' CHECK(state IN ('cut','failed','ignored')),
    file TEXT,
    item_id INTEGER,
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX live_moments_by_session ON live_moments(session_id, id);
"#;

const SCHEMAS: &[&str] = &[
    SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
    SCHEMA_V9, SCHEMA_V10, SCHEMA_V11, SCHEMA_V12, SCHEMA_V13, SCHEMA_V14, SCHEMA_V15, SCHEMA_V16,
    SCHEMA_V17, SCHEMA_V18, SCHEMA_V19, SCHEMA_V20,
];

/// Сколько резервных копий базы хранить перед миграциями.
const KEEP_BACKUPS: usize = 3;

pub fn open(path: &Path) -> anyhow::Result<Db> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    backup_before_migrate(&conn, path)?;
    migrate(&conn)?;
    Ok(Db(Mutex::new(conn)))
}

/// Перед обновлением схемы существующей базы делаем согласованную копию рядом с ней:
/// `<имя>.v<версия>.bak`. Ошибка копии останавливает миграцию, чтобы не потерять данные.
/// Хранятся несколько последних копий.
fn backup_before_migrate(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 || version as usize >= SCHEMAS.len() {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("некорректный путь базы"))?;
    let target = path.with_file_name(format!("{name}.v{version}.bak"));
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    // VACUUM INTO даёт целостный снимок с учётом WAL, не требуя отдельной возможности rusqlite.
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
        .map_err(|e| {
            anyhow::anyhow!("не удалось сделать резервную копию базы перед миграцией: {e}")
        })?;
    prune_backups(path, name);
    Ok(())
}

fn prune_backups(path: &Path, name: &str) {
    let Some(dir) = path.parent() else { return };
    let prefix = format!("{name}.v");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<_> = entries
        .flatten()
        .filter(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            file.starts_with(&prefix) && file.ends_with(".bak")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, old) in backups.into_iter().skip(KEEP_BACKUPS) {
        let _ = std::fs::remove_file(old);
    }
}

pub(crate) fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (index, schema) in SCHEMAS.iter().enumerate() {
        let next = index as i64 + 1;
        if version < next {
            apply_migration(conn, next, schema)?;
        }
    }
    Ok(())
}

/// Версия и изменение схемы фиксируются вместе. При ошибке Drop откатывает весь шаг,
/// включая CREATE/ALTER/DROP и перенос строк (особенно важно для V9).
fn apply_migration(conn: &Connection, version: i64, sql: &str) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(sql)?;
    if version == 10 {
        let defaults = crate::drafts::load_defaults(&tx);
        tx.execute(
            "INSERT INTO templates (name,config,is_default) VALUES ('По умолчанию',?1,1)",
            [serde_json::to_string(&defaults)?],
        )?;
    }
    tx.pragma_update(None, "user_version", version)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_backs_up_old_database_before_migrating() {
        let dir =
            std::env::temp_dir().join(format!("avto-db-backup-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("avtonarezka.db");
        {
            let c = Connection::open(&path).unwrap();
            for (i, schema) in SCHEMAS.iter().take(9).enumerate() {
                apply_migration(&c, i as i64 + 1, schema).unwrap();
            }
            c.execute("INSERT INTO kv (key,value) VALUES ('probe','1')", [])
                .ok();
        }
        let db = open(&path).unwrap();
        let latest: i64 =
            db.0.lock()
                .unwrap()
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
        assert_eq!(latest as usize, SCHEMAS.len());
        let bak = dir.join("avtonarezka.db.v9.bak");
        assert!(bak.is_file(), "резервная копия v9 должна существовать");
        let b = Connection::open(&bak).unwrap();
        assert_eq!(
            b.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            9
        );
        drop(db);
        // Повторный запуск на актуальной базе новых копий не создаёт.
        let before = std::fs::read_dir(&dir).unwrap().count();
        drop(open(&path).unwrap());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs real migrations without starting Tauri, workers, monitoring or publishers.
    /// The input must be a closed SQLite online backup in the project's scratch tree.
    #[test]
    #[ignore = "requires an explicitly prepared private v9 database backup"]
    fn real_v9_backup_migrates_without_losing_data() {
        use rusqlite::types::Value;
        use sha2::{Digest, Sha256};
        use std::io::Write;

        fn quoted(name: &str) -> String {
            format!("\"{}\"", name.replace('"', "\"\""))
        }
        fn snapshot(
            c: &Connection,
            table: &str,
            columns: &[String],
            filter: &str,
        ) -> Vec<Vec<Value>> {
            let sql = format!(
                "SELECT {} FROM {} {filter} ORDER BY rowid",
                columns
                    .iter()
                    .map(|s| quoted(s))
                    .collect::<Vec<_>>()
                    .join(","),
                quoted(table)
            );
            let mut st = c.prepare(&sql).unwrap();
            let rows = st
                .query_map([], |row| (0..columns.len()).map(|i| row.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows
        }
        fn tables(c: &Connection) -> Vec<String> {
            c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
                .unwrap().query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        }
        fn columns(c: &Connection, table: &str) -> Vec<String> {
            c.prepare(&format!("PRAGMA table_info({})", quoted(table)))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        }

        let started = std::time::Instant::now();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".factory-tests")
            .canonicalize()
            .unwrap();
        let input = std::path::PathBuf::from(
            std::env::var_os("AVTO_MIGRATION_BACKUP")
                .expect("set AVTO_MIGRATION_BACKUP to a closed v9 online backup"),
        )
        .canonicalize()
        .unwrap();
        assert!(
            input.starts_with(&root) && input.is_file(),
            "input must be an isolated project backup"
        );
        for suffix in ["-wal", "-journal"] {
            let mut sidecar = input.as_os_str().to_os_string();
            sidecar.push(suffix);
            assert!(
                !Path::new(&sidecar).exists(),
                "input must have no pending journal"
            );
        }
        let baseline = std::fs::read(&input).unwrap();
        let baseline_hash = hex::encode(Sha256::digest(&baseline));
        let output = input.parent().unwrap().join(format!(
            "migrated-{}-{}.db",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .unwrap();
        file.write_all(&baseline).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let c = Connection::open(&output).unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        assert_eq!(version(&c), 9);
        assert_eq!(
            c.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap(),
            "ok"
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
        let old: Vec<_> = tables(&c)
            .into_iter()
            .map(|table| {
                let cols = columns(&c, &table);
                let rows = snapshot(&c, &table, &cols, "");
                (table, cols, rows)
            })
            .collect();
        let saved_defaults = crate::drafts::kv_get(&c, "draft_defaults");
        let mut defaults: crate::drafts::Defaults = saved_defaults
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        let defaults_upgrade = defaults.layout_v < 1;
        crate::drafts::upgrade_defaults(&mut defaults);

        migrate(&c).unwrap();
        assert_eq!(version(&c), 20);
        for (table, cols, original) in &old {
            let target = match table.as_str() {
                "batch_items" => "factory_items",
                "publish_jobs" => "publications",
                "posts" => "publication_attempts",
                other => other,
            };
            let mut expected = original.clone();
            if table == "batch_items" {
                let i = cols.iter().position(|s| s == "stage").unwrap();
                for row in &mut expected {
                    if let Value::Text(stage) = &mut row[i] {
                        *stage = match stage.as_str() {
                            "downloading" | "importing" => "fetching",
                            "analyzing" => "scouting",
                            "rendering" => "rendering_final",
                            "done" => "completed",
                            s => s,
                        }
                        .to_owned();
                    }
                }
            }
            if table == "kv" && saved_defaults.is_some() && defaults_upgrade {
                let key = cols.iter().position(|s| s == "key").unwrap();
                let value = cols.iter().position(|s| s == "value").unwrap();
                for row in &mut expected {
                    if row[key] == Value::Text("draft_defaults".into()) {
                        row[value] = Value::Text(serde_json::to_string(&defaults).unwrap());
                    }
                }
            }
            // V13 also creates publications for manual history. Compare original jobs by id.
            let filter = if table == "publish_jobs" {
                let id = cols.iter().position(|s| s == "id").unwrap();
                let ids: Vec<_> = original
                    .iter()
                    .map(|row| match row[id] {
                        Value::Integer(i) => i.to_string(),
                        _ => panic!("invalid job id"),
                    })
                    .collect();
                format!(
                    "WHERE legacy=1 AND id IN ({})",
                    if ids.is_empty() {
                        "NULL".into()
                    } else {
                        ids.join(",")
                    }
                )
            } else {
                String::new()
            };
            assert!(
                snapshot(&c, target, cols, &filter) == expected,
                "legacy records changed in {table}"
            );
        }
        let template: String = c
            .query_row("SELECT config FROM templates WHERE is_default=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&template).unwrap()
                == serde_json::to_value(&defaults).unwrap(),
            "default template differs from upgraded legacy defaults"
        );
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM channels", [], |r| r.get(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM events", [], |r| r.get(0))
                .unwrap(),
            0
        );

        // Exercise the readers used by Studio, instead of only comparing raw SQL.
        let projects = c
            .prepare(&format!(
                "SELECT {} FROM projects",
                crate::commands::PROJECT_COLS
            ))
            .unwrap()
            .query_map([], crate::commands::project_from_row)
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let draft_ids = c
            .prepare("SELECT id FROM drafts ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for id in &draft_ids {
            let draft = crate::drafts::get(&c, *id).unwrap();
            let raw: String = c
                .query_row("SELECT style_json FROM drafts WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(
                serde_json::from_str::<crate::drafts::DraftConfig>(&raw).is_ok(),
                "legacy draft config cannot be read"
            );
            assert_eq!(
                draft.config.style.layout,
                crate::render::Layout::GameplayStack
            );
        }
        let mut analyses = c
            .prepare("SELECT signals_json,words_json,sentences_json,notes_json FROM analysis")
            .unwrap();
        let analysis_rows = analyses
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        drop(analyses);
        for (signals, words, sentences, notes) in &analysis_rows {
            assert!(
                serde_json::from_str::<crate::analyze::Signals>(signals).is_ok(),
                "legacy signals cannot be read"
            );
            assert!(
                serde_json::from_str::<Vec<crate::render::Word>>(words).is_ok(),
                "legacy words cannot be read"
            );
            assert!(
                serde_json::from_str::<Vec<crate::analyze::Sentence>>(sentences).is_ok(),
                "legacy sentences cannot be read"
            );
            assert!(
                serde_json::from_str::<Vec<String>>(notes).is_ok(),
                "legacy notes cannot be read"
            );
        }
        let after: Vec<_> = tables(&c)
            .into_iter()
            .map(|table| {
                let cols = columns(&c, &table);
                let rows = snapshot(&c, &table, &cols, "");
                (table, cols, rows)
            })
            .collect();
        migrate(&c).unwrap();
        for (table, cols, rows) in &after {
            assert!(
                snapshot(&c, table, cols, "") == *rows,
                "repeated migration changed {table}"
            );
        }
        assert_eq!(
            c.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap(),
            "ok"
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
        c.close().unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(std::fs::read(&input).unwrap())),
            baseline_hash
        );
        let report = serde_json::json!({"state":"complete","fromVersion":9,"toVersion":16,
            "baselineSha256":baseline_hash,"baselineUnchanged":true,"projectsRead":projects.len(),
            "draftsRead":draft_ids.len(),"analysesRead":analysis_rows.len(),"oldTablesVerified":old.len(),
            "integrity":"ok","foreignKeyErrors":0,"repeatedMigrationUnchanged":true,
            "backgroundWorkersStarted":false,"seconds":started.elapsed().as_secs_f64(),
            "migratedCopy":output.file_name().unwrap().to_str().unwrap()});
        std::fs::write(
            input.parent().unwrap().join("migration-result.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!("{report}");
    }

    #[test]
    fn metrics_v16_rolls_back_and_preserves_history() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (i, s) in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
            SCHEMA_V9, SCHEMA_V10, SCHEMA_V11, SCHEMA_V12, SCHEMA_V13, SCHEMA_V14, SCHEMA_V15,
        ]
        .iter()
        .enumerate()
        {
            apply_migration(&c, i as i64 + 1, s).unwrap();
        }
        c.execute("INSERT INTO channels(name)VALUES('Прежний канал')", [])
            .unwrap();
        assert!(apply_migration(
            &c,
            16,
            &format!("{SCHEMA_V16} INSERT INTO missing VALUES(1);")
        )
        .is_err());
        assert_eq!(version(&c), 15);
        assert!(c.prepare("SELECT * FROM metrics").is_err());
        assert!(c.prepare("SELECT * FROM moment_features").is_err());
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        assert_eq!(version(&c), 20);
        assert_eq!(
            c.query_row::<String, _, _>("SELECT name FROM channels", [], |r| r.get(0))
                .unwrap(),
            "Прежний канал"
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    #[test]
    fn campaigns_v15_is_atomic_preserves_legacy_data_and_repeats() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (i, schema) in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
            SCHEMA_V9, SCHEMA_V10, SCHEMA_V11, SCHEMA_V12, SCHEMA_V13, SCHEMA_V14,
        ]
        .iter()
        .enumerate()
        {
            apply_migration(&c, i as i64 + 1, schema).unwrap();
        }
        c.execute("INSERT INTO channels(name)VALUES('Существующий')", [])
            .unwrap();
        assert!(apply_migration(
            &c,
            15,
            &format!("{SCHEMA_V15} INSERT INTO missing VALUES(1);")
        )
        .is_err());
        assert_eq!(version(&c), 14);
        assert!(c.prepare("SELECT * FROM campaigns").is_err());
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        assert_eq!(version(&c), 20);
        assert_eq!(
            crate::factory::profiles::channels(&c).unwrap()[0].name,
            "Существующий"
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    #[test]
    fn rights_v14_rollback_and_defaults_preserve_existing_profiles_and_publications() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (i, schema) in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
            SCHEMA_V9, SCHEMA_V10, SCHEMA_V11, SCHEMA_V12, SCHEMA_V13,
        ]
        .iter()
        .enumerate()
        {
            apply_migration(&c, i as i64 + 1, schema).unwrap();
        }
        c.execute_batch("INSERT INTO channels(id,name,permission,credit,targets) VALUES(7,'Прежний','agreed','Автор','[{\"platform\":\"telegram\",\"accountId\":null}]');").unwrap();
        assert!(apply_migration(
            &c,
            14,
            &format!("{SCHEMA_V14} INSERT INTO missing VALUES(1);")
        )
        .is_err());
        assert_eq!(version(&c), 13);
        assert!(c.prepare("SELECT rights_paused FROM channels").is_err());
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let old = crate::factory::profiles::channels(&c).unwrap().remove(0);
        assert_eq!(
            (
                old.id,
                old.permission.as_str(),
                old.credit.as_str(),
                old.complaint_limit,
                old.rights_paused,
                old.active_incidents
            ),
            (7, "agreed", "Автор", 3, false, 0)
        );
        assert_eq!(old.targets.len(), 1);
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    fn version(c: &Connection) -> i64 {
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn v17_adds_node_ownership_without_touching_existing_rows() {
        let c = Connection::open_in_memory().unwrap();
        for (i, schema) in SCHEMAS.iter().take(16).enumerate() {
            apply_migration(&c, i as i64 + 1, schema).unwrap();
        }
        c.execute(
            "INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,'link','https://x','t','1080',0,'queued')",
            [],
        )
        .unwrap();
        assert!(apply_migration(&c, 17, &format!("{SCHEMA_V17} INSERT INTO missing VALUES(1);")).is_err());
        assert_eq!(version(&c), 16);
        assert!(c.prepare("SELECT * FROM transfers").is_err());
        migrate(&c).unwrap();
        assert_eq!(version(&c), 20);
        let owner: String = c
            .query_row("SELECT owner_node FROM factory_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, "local");
    }

    fn v8() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        for (i, sql) in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
        ]
        .iter()
        .enumerate()
        {
            apply_migration(&c, i as i64 + 1, sql).unwrap();
        }
        c.execute_batch(r#"
            INSERT INTO projects (id,path,hash,title,duration,width,height,fps,has_audio) VALUES (1,'/v','h','Старый проект',60,1920,1080,30,1);
            INSERT INTO moments (id,project_id,start,"end") VALUES (7,1,0,30);
            INSERT INTO drafts (id,moment_id,final_path) VALUES (8,7,'/final.mp4');
            INSERT INTO publish_jobs (id,draft_id,caption,state,account_id,attempts,not_before) VALUES (9,8,'Описание','needs_check',1,2,12345);
            INSERT INTO posts (id,draft_id,method,mode,status,publish_id,account_id,sent_at) VALUES (10,8,'api','direct','done','external-123',1,12300);
            INSERT INTO kv VALUES ('defaults','{"bannerId":42}');
        "#).unwrap();
        c
    }

    #[test]
    fn fresh_database_and_repeat_migration_are_safe() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        assert_eq!(version(&c), 20);
        migrate(&c).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM accounts", [], |r| r.get(0))
                .unwrap(),
            1
        );
        assert!(c.is_autocommit());
    }

    #[test]
    fn v8_to_v9_preserves_ids_publication_history_and_relationships() {
        let c = v8();
        migrate(&c).unwrap();
        let job: (i64, String, String, i64, i64, i64) = c.query_row(
            "SELECT draft_id,caption,state,account_id,attempts,not_before FROM publications WHERE id=9", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
        assert_eq!(
            job,
            (8, "Описание".into(), "needs_check".into(), 1, 2, 12345)
        );
        assert_eq!(
            c.query_row::<String, _, _>(
                "SELECT publish_id FROM publication_attempts WHERE id=10",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            "external-123"
        );
        assert_eq!(
            c.query_row::<String, _, _>("SELECT value FROM kv WHERE key='defaults'", [], |r| r
                .get(0))
                .unwrap(),
            "{\"bannerId\":42}"
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
        c.execute("DELETE FROM projects WHERE id=1", []).unwrap();
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM publications", [], |r| r.get(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row::<i64, _, _>("SELECT count(*) FROM publication_attempts", [], |r| r
                .get(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn v9_factory_migration_keeps_queue_ids_order_state_and_upgrades_defaults() {
        let c = v8();
        apply_migration(&c, 9, SCHEMA_V9).unwrap();
        c.execute_batch(r#"
            INSERT INTO batch_items(id,position,kind,source,title,preset,precut,project_id,stage,error)
                VALUES(41,7,'file','/old.mp4','Старое видео','720',1,1,'rendering','Заметка'),
                      (42,8,'link','https://youtu.be/test','Запись','1080',0,NULL,'downloading',NULL);
            INSERT INTO kv(key,value) VALUES('draft_defaults','{"style":{"topFit":"fill","topCropBottom":0.2,"split":0.66},"backgroundIds":[17],"bannerId":23}');
        "#).unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let q: (i64, String, i64, String, String) = c
            .query_row(
                "SELECT position,stage,project_id,source,error FROM factory_items WHERE id=41",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            q,
            (
                7,
                "rendering_final".into(),
                1,
                "/old.mp4".into(),
                "Заметка".into()
            )
        );
        assert_eq!(
            c.query_row::<String, _, _>("SELECT stage FROM factory_items WHERE id=42", [], |r| r
                .get(0))
                .unwrap(),
            "fetching"
        );
        let t = crate::factory::profiles::templates(&c).unwrap().remove(0);
        assert_eq!(
            (
                t.name.as_str(),
                t.config.style.top_fit,
                t.config.banner_id,
                t.config.background_ids
            ),
            (
                "По умолчанию",
                crate::render::TopFit::Auto,
                Some(23),
                vec![17]
            )
        );
        assert_eq!(t.config.style.split, 0.66);
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
        assert!(c.prepare("SELECT * FROM batch_items").is_err());
    }

    #[test]
    fn failed_migration_rolls_back_schema_data_and_version_then_can_retry() {
        let c = v8();
        // Сбой после DROP/RENAME, то есть после всей сложной миграции V9.
        assert!(apply_migration(
            &c,
            9,
            &format!("{SCHEMA_V9} INSERT INTO nonexistent VALUES (1);")
        )
        .is_err());
        assert_eq!(version(&c), 8);
        assert!(c.is_autocommit());
        assert_eq!(
            c.query_row::<String, _, _>("SELECT caption FROM publish_jobs WHERE id=9", [], |r| r
                .get(0))
                .unwrap(),
            "Описание"
        );
        assert!(c.prepare("SELECT * FROM ready_videos").is_err());
        migrate(&c).unwrap();
        assert_eq!(version(&c), 20);
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    #[test]
    fn source_migration_is_atomic_keeps_v10_profiles_and_dedup_relationships() {
        let c = v8();
        apply_migration(&c, 9, SCHEMA_V9).unwrap();
        apply_migration(&c, 10, SCHEMA_V10).unwrap();
        c.execute_batch("INSERT INTO channels(id,name,source_kind,source) VALUES(4,'Старый профиль','youtube','https://youtube.com/@old'); INSERT INTO source_items(id,channel_id,external_id,source,title) VALUES(7,4,'ext','/source','Сохранённая запись'); INSERT INTO factory_items(id,position,kind,source,source_item_id,channel_id) VALUES(9,3,'link','/source',7,4);").unwrap();
        assert!(apply_migration(
            &c,
            11,
            &format!("{SCHEMA_V11} INSERT INTO absent VALUES(1);")
        )
        .is_err());
        assert_eq!(version(&c), 10);
        assert!(c.prepare("SELECT monitor_options FROM channels").is_err());
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let profile = crate::factory::profiles::channels(&c).unwrap().remove(0);
        assert_eq!(profile.name, "Старый профиль");
        assert!(!profile.monitor.enabled);
        assert_eq!(
            c.query_row::<i64, _, _>(
                "SELECT source_item_id FROM factory_items WHERE id=9",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            7
        );
        assert!(c.execute("INSERT INTO factory_items(position,kind,source,source_item_id) VALUES(4,'link','/duplicate',7)",[]).is_err());
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
    #[test]
    fn analysis_inputs_v12_rolls_back_and_keeps_old_analysis() {
        let c = v8();
        apply_migration(&c, 9, SCHEMA_V9).unwrap();
        apply_migration(&c, 10, SCHEMA_V10).unwrap();
        apply_migration(&c, 11, SCHEMA_V11).unwrap();
        c.execute("INSERT INTO analysis(project_id,signals_json,words_json,sentences_json) VALUES(1,'{}','[]','[]')",[]).unwrap();
        assert!(apply_migration(
            &c,
            12,
            &format!("{SCHEMA_V12} INSERT INTO absent VALUES(1);")
        )
        .is_err());
        assert_eq!(version(&c), 11);
        assert!(c.prepare("SELECT * FROM analysis_sources").is_err());
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        assert_eq!(
            c.query_row::<String, _, _>(
                "SELECT signals_json FROM analysis WHERE project_id=1",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            "{}"
        );
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    #[test]
    fn publications_v13_migrates_old_jobs_and_manual_history_atomically() {
        let c = v8();
        for (n, schema) in [
            (9, SCHEMA_V9),
            (10, SCHEMA_V10),
            (11, SCHEMA_V11),
            (12, SCHEMA_V12),
        ] {
            apply_migration(&c, n, schema).unwrap();
        }
        c.execute_batch("INSERT INTO moments(id,project_id,start,\"end\")VALUES(11,1,30,60); INSERT INTO drafts(id,moment_id)VALUES(12,11); INSERT INTO posts(id,draft_id,method,mode,status,publish_id,sent_at)VALUES(20,12,'api','direct','done','manual-id',12320),(21,12,'api','direct','uploading','pending-id',NULL);").unwrap();
        assert!(apply_migration(
            &c,
            13,
            &format!("{SCHEMA_V13} INSERT INTO absent VALUES(1);")
        )
        .is_err());
        assert_eq!(version(&c), 12);
        assert!(c.prepare("SELECT * FROM publications").is_err());
        assert!(c.prepare("SELECT * FROM posts").is_ok());
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let old: (String, i64, String) = c
            .query_row(
                "SELECT state,attempts,external_id FROM publications WHERE id=9",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(old, ("needs_check".into(), 2, "external-123".into()));
        let manual: (String, String) = c
            .query_row(
                "SELECT state,external_id FROM publications WHERE draft_id=12",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(manual, ("needs_check".into(), "pending-id".into()));
        assert_eq!(
            c.query_row::<i64, _, _>(
                "SELECT count(*) FROM publication_attempts WHERE publication_id IS NOT NULL",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            3
        );
        assert!(c.prepare("SELECT * FROM posts").is_err());
        assert!(c.prepare("SELECT * FROM publish_jobs").is_err());
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
}

/// Ключ-значение в таблице `kv`: настройки и курсоры, общие для всех модулей.
pub(crate) fn kv_get(db: &Connection, key: &str) -> Option<String> {
    db.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

pub(crate) fn kv_set(db: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())

}
