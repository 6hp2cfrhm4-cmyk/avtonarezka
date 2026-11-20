//! Хранение автопоиска: настройки, снимки популярности, кандидаты, принятие и отказ.
//! Всё работает над `&Connection`, поэтому проверяется тестами на базе в памяти.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{auto_accept, providers::Found, trend_score, Taste};
use crate::{
    db::{kv_get, kv_set},
    factory::{self, profiles},
};

const KV_CONFIG: &str = "discovery_config";
const KV_TOPICS: &str = "discovery_topics";
pub const SANDBOX: &str = "Тренды (песочница)";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub twitch: bool,
    pub kick: bool,
    pub youtube: bool,
    pub topics: bool,
    pub languages: Vec<String>,
    pub categories: Vec<String>,
    /// Ваши слова-темы; к ним добавляются темы дня (Google Trends, Wikipedia).
    pub keywords: Vec<String>,
    pub known_channels: Vec<String>,
    /// Каналы/ссылки, которым разрешено автопринятие.
    pub whitelist: Vec<String>,
    pub threshold: f64,
    pub show_from: f64,
    pub daily_limit: u32,
    pub interval_min: u32,
    pub region: String,
    pub games_top: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            twitch: true,
            kick: false,
            youtube: false,
            topics: true,
            languages: vec!["ru".into()],
            categories: vec![],
            keywords: vec![],
            known_channels: vec![],
            whitelist: vec![],
            threshold: 0.75,
            show_from: 0.2,
            daily_limit: 5,
            interval_min: 10,
            region: "RU".into(),
            games_top: 8,
        }
    }
}

impl Config {
    pub fn normalised(mut self) -> Self {
        self.interval_min = self.interval_min.clamp(5, 240);
        self.daily_limit = self.daily_limit.min(100);
        self.games_top = self.games_top.clamp(1, 20);
        self.threshold = if self.threshold.is_finite() { self.threshold.clamp(0.0, 1.0) } else { 0.75 };
        self.show_from = if self.show_from.is_finite() { self.show_from.clamp(0.0, 1.0) } else { 0.2 };
        self.region = self.region.trim().to_uppercase();
        if self.region.len() != 2 {
            self.region = "RU".into();
        }
        for list in [&mut self.languages, &mut self.categories, &mut self.keywords, &mut self.known_channels, &mut self.whitelist] {
            list.iter_mut().for_each(|x| *x = x.trim().to_string());
            list.retain(|x| !x.is_empty());
            list.dedup();
        }
        self
    }
}

pub fn load_config(db: &Connection) -> Config {
    kv_get(db, KV_CONFIG).and_then(|j| serde_json::from_str::<Config>(&j).ok()).unwrap_or_default().normalised()
}
pub fn save_config(db: &Connection, c: Config) -> rusqlite::Result<Config> {
    let c = c.normalised();
    kv_set(db, KV_CONFIG, &serde_json::to_string(&c).unwrap_or_default())?;
    Ok(c)
}
pub fn load_topics(db: &Connection) -> Vec<String> {
    kv_get(db, KV_TOPICS).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
}
pub fn save_topics(db: &Connection, t: &[String]) -> rusqlite::Result<()> {
    kv_set(db, KV_TOPICS, &serde_json::to_string(t).unwrap_or_default())
}

/// Скорость роста считаем по самому раннему снимку из последнего часа, но не младше 8 минут.
fn previous_metric(db: &Connection, key: &str, now: i64) -> (f64, f64) {
    db.query_row(
        "SELECT metric, taken_at FROM discovery_snapshots WHERE key=?1 AND taken_at BETWEEN ?2 AND ?3 ORDER BY taken_at LIMIT 1",
        params![key, now - 3600, now - 480],
        |r| Ok((r.get::<_, f64>(0)?, (now - r.get::<_, i64>(1)?) as f64 / 60.0)),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or((0.0, 0.0))
}

#[derive(Serialize, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub seen: usize,
    pub fresh: usize,
    pub shown: usize,
    pub auto_accepted: usize,
}

fn today_start(now: i64) -> i64 {
    now - now.rem_euclid(86400)
}

pub fn ingest(db: &Connection, cfg: &Config, founds: &[Found], now: i64) -> Result<Stats, String> {
    let mut topics = cfg.keywords.clone();
    topics.extend(load_topics(db));
    let mut stats = Stats::default();
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    for f in founds {
        stats.seen += 1;
        let (previous, window) = previous_metric(&tx, &f.key, now);
        tx.execute("INSERT OR REPLACE INTO discovery_snapshots(key,taken_at,metric) VALUES(?1,?2,?3)", params![f.key, now, f.obs.metric]).map_err(|e| e.to_string())?;
        let state: Option<String> = tx.query_row("SELECT state FROM discovery_candidates WHERE key=?1", [&f.key], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        // Уже решённое не трогаем, кроме обновления «последний раз видели».
        if state.as_deref().is_some_and(|s| s != "new") {
            tx.execute("UPDATE discovery_candidates SET last_seen=?2 WHERE key=?1", params![f.key, now]).map_err(|e| e.to_string())?;
            continue;
        }
        let mut obs = f.obs.clone();
        obs.previous = previous;
        obs.window_min = window;
        let taste = Taste {
            languages: cfg.languages.clone(),
            categories: cfg.categories.clone(),
            topics: topics.clone(),
            known_channels: cfg.known_channels.clone(),
            already_processed: tx
                .query_row("SELECT EXISTS(SELECT 1 FROM source_items WHERE external_id=?1 OR source=?2)", params![f.key, f.obs.url], |r| r.get(0))
                .unwrap_or(false),
        };
        let sc = trend_score(&obs, &taste);
        if sc.score < cfg.show_from {
            // не засоряем список слабыми; уже сохранённый кандидат остаётся с прежней оценкой
            continue;
        }
        tx.execute(
            "INSERT INTO discovery_candidates(key,kind,platform,url,title,category,language,channel_name,metric,previous,window_min,duration_min,score,why,first_seen,last_seen)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)
             ON CONFLICT(key) DO UPDATE SET title=excluded.title,category=excluded.category,metric=excluded.metric,previous=excluded.previous,window_min=excluded.window_min,
               duration_min=excluded.duration_min,score=excluded.score,why=excluded.why,last_seen=excluded.last_seen",
            params![f.key, f.kind, obs.platform, obs.url, obs.title, obs.category, obs.language, f.channel, obs.metric, previous, window, obs.duration_min, sc.score, sc.why, now],
        )
        .map_err(|e| e.to_string())?;
        if state.is_none() {
            stats.fresh += 1;
        }
        stats.shown += 1;
    }
    // Старое молчание: снимки живут двое суток, неразобранные кандидаты — трое суток после последнего появления.
    tx.execute("DELETE FROM discovery_snapshots WHERE taken_at < ?1", [now - 2 * 86400]).map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM discovery_candidates WHERE state='new' AND last_seen < ?1", [now - 3 * 86400]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    stats.auto_accepted = auto_pass(db, cfg, now)?;
    Ok(stats)
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub id: i64,
    pub kind: String,
    pub platform: String,
    pub url: String,
    pub title: String,
    pub category: String,
    pub language: String,
    pub channel_name: String,
    pub metric: f64,
    pub previous: f64,
    pub window_min: f64,
    pub score: f64,
    pub why: String,
    pub state: String,
    pub auto: bool,
    pub last_seen: i64,
}

pub fn list(db: &Connection, state: &str) -> rusqlite::Result<Vec<Candidate>> {
    let mut st = db.prepare(
        "SELECT id,kind,platform,url,title,category,language,channel_name,metric,previous,window_min,score,why,state,auto,last_seen
         FROM discovery_candidates WHERE state=?1 ORDER BY score DESC, last_seen DESC LIMIT 200",
    )?;
    let rows = st.query_map([state], |r| {
        Ok(Candidate {
            id: r.get(0)?,
            kind: r.get(1)?,
            platform: r.get(2)?,
            url: r.get(3)?,
            title: r.get(4)?,
            category: r.get(5)?,
            language: r.get(6)?,
            channel_name: r.get(7)?,
            metric: r.get(8)?,
            previous: r.get(9)?,
            window_min: r.get(10)?,
            score: r.get(11)?,
            why: r.get(12)?,
            state: r.get(13)?,
            auto: r.get::<_, i64>(14)? != 0,
            last_seen: r.get(15)?,
        })
    })?;
    rows.collect()
}

/// Канал-«песочница»: монитор выключен, режим review, права не указаны, поэтому ничего не уходит само.
pub fn sandbox_channel(db: &Connection) -> Result<profiles::Channel, String> {
    if let Some(c) = profiles::channels(db)?.into_iter().find(|c| c.name == SANDBOX) {
        return Ok(c);
    }
    profiles::save_channel(
        db,
        profiles::Channel {
            name: SANDBOX.into(),
            source_kind: "youtube".into(),
            enabled: true, // канал рабочий, но монитор у него выключен: он только принимает найденное
            mode: "review".into(),
            permission: "unspecified".into(),
            ..Default::default()
        },
    )
}

/// Принять кандидата. Запись и эфир идут по-разному: запись ставится в очередь фабрики,
/// эфир помечается «следить» и ждёт модуль live (его берёт поток `live`).
pub fn accept(db: &Connection, id: i64, auto: bool, now: i64) -> Result<Option<i64>, String> {
    let c: Candidate = list_one(db, id)?;
    if c.state != "new" {
        return Err("Кандидат уже рассмотрен".into());
    }
    if c.kind == "live" {
        db.execute("UPDATE discovery_candidates SET state='watch',auto=?2,decided_at=?3 WHERE id=?1", params![id, auto, now]).map_err(|e| e.to_string())?;
        return Ok(None);
    }
    let channel = sandbox_channel(db)?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let key: String = tx.query_row("SELECT key FROM discovery_candidates WHERE id=?1", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO source_items(channel_id,external_id,source,title,duration,state) VALUES(?1,?2,?3,?4,NULL,'queued')
         ON CONFLICT(channel_id,external_id) DO UPDATE SET state='queued'",
        params![channel.id, key, c.url, c.title],
    )
    .map_err(|e| e.to_string())?;
    let source_id: i64 = tx.query_row("SELECT id FROM source_items WHERE channel_id=?1 AND external_id=?2", params![channel.id, key], |r| r.get(0)).map_err(|e| e.to_string())?;
    let position: i64 = tx.query_row("SELECT COALESCE(MAX(position),0)+1 FROM factory_items", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let item = factory::insert_item(
        &tx,
        &factory::NewItem { kind: "link".into(), source: c.url.clone(), title: c.title.clone(), preset: "1080".into(), precut: false, channel_id: Some(channel.id), priority: 0 },
        Some(source_id),
        position,
    )?;
    tx.execute("UPDATE discovery_candidates SET state='accepted',auto=?2,decided_at=?3,source_item_id=?4 WHERE id=?1", params![id, auto, now, source_id]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(item))
}

pub fn reject(db: &Connection, id: i64, now: i64) -> Result<(), String> {
    let n = db.execute("UPDATE discovery_candidates SET state='rejected',decided_at=?2 WHERE id=?1 AND state='new'", params![id, now]).map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("Кандидат уже рассмотрен".into());
    }
    Ok(())
}

fn list_one(db: &Connection, id: i64) -> Result<Candidate, String> {
    list_by(db, "id=?1", id)
}
fn list_by(db: &Connection, cond: &str, id: i64) -> Result<Candidate, String> {
    db.query_row(
        &format!("SELECT id,kind,platform,url,title,category,language,channel_name,metric,previous,window_min,score,why,state,auto,last_seen FROM discovery_candidates WHERE {cond}"),
        [id],
        |r| {
            Ok(Candidate {
                id: r.get(0)?,
                kind: r.get(1)?,
                platform: r.get(2)?,
                url: r.get(3)?,
                title: r.get(4)?,
                category: r.get(5)?,
                language: r.get(6)?,
                channel_name: r.get(7)?,
                metric: r.get(8)?,
                previous: r.get(9)?,
                window_min: r.get(10)?,
                score: r.get(11)?,
                why: r.get(12)?,
                state: r.get(13)?,
                auto: r.get::<_, i64>(14)? != 0,
                last_seen: r.get(15)?,
            })
        },
    )
    .map_err(|_| "Кандидат не найден".to_string())
}

fn whitelisted(cfg: &Config, c: &Candidate) -> bool {
    cfg.whitelist.iter().any(|w| {
        let w = w.to_lowercase();
        c.url.to_lowercase().contains(&w) || c.channel_name.to_lowercase() == w
    })
}

/// Автопринятие по правилу: оценка ≥ порога, канал разрешён, суточный лимит не выбран.
fn auto_pass(db: &Connection, cfg: &Config, now: i64) -> Result<usize, String> {
    if cfg.daily_limit == 0 {
        return Ok(0);
    }
    let permission = sandbox_permission(db);
    let mut today: u32 = db
        .query_row("SELECT COUNT(*) FROM discovery_candidates WHERE auto=1 AND decided_at>=?1", [today_start(now)], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let mut done = 0;
    for c in list(db, "new").map_err(|e| e.to_string())? {
        if auto_accept(c.score, cfg.threshold, whitelisted(cfg, &c), &permission, today, cfg.daily_limit) {
            accept(db, c.id, true, now)?;
            today += 1;
            done += 1;
        }
    }
    Ok(done)
}

fn sandbox_permission(db: &Connection) -> String {
    profiles::channels(db).ok().and_then(|cs| cs.into_iter().find(|c| c.name == SANDBOX)).map(|c| c.permission).unwrap_or_else(|| "unspecified".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Observation;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }
    fn found(key: &str, kind: &'static str, metric: f64) -> Found {
        Found {
            key: key.into(),
            kind,
            channel: "Стример".into(),
            obs: Observation { platform: "twitch".into(), url: format!("https://twitch.tv/{key}"), title: "Финал турнира".into(), category: "Dota 2".into(), language: "ru".into(), metric, previous: 0.0, window_min: 0.0, duration_min: None },
        }
    }
    fn cfg() -> Config {
        Config { categories: vec!["Dota 2".into()], keywords: vec!["турнир".into()], ..Default::default() }
    }

    #[test]
    fn growth_comes_from_snapshots() {
        let c = db();
        let k = cfg();
        // первый опрос: роста нет — оценка скромная; через 30 минут зрителей в 5 раз больше
        ingest(&c, &k, &[found("a", "vod", 1000.0)], 10_000).unwrap();
        let first = list(&c, "new").unwrap()[0].score;
        ingest(&c, &k, &[found("a", "vod", 5000.0)], 10_000 + 1800).unwrap();
        let after = list(&c, "new").unwrap();
        assert!(after[0].score > first, "{} > {first}", after[0].score);
        assert!(after[0].why.contains("+400 %"), "{}", after[0].why);
        assert_eq!(after[0].window_min, 30.0);
    }

    #[test]
    fn weak_candidates_stay_out_and_language_is_filtered() {
        let c = db();
        let mut weak = found("w", "vod", 3.0);
        weak.obs.category = "Chess".into();
        weak.obs.title = "тихо".into();
        let mut en = found("e", "vod", 90_000.0);
        en.obs.language = "en".into();
        let st = ingest(&c, &cfg(), &[weak, en], 100).unwrap();
        assert_eq!(st.shown, 0);
        assert!(list(&c, "new").unwrap().is_empty());
    }

    #[test]
    fn accept_vod_queues_item_in_sandbox_and_is_idempotent() {
        let c = db();
        ingest(&c, &cfg(), &[found("v1", "vod", 20_000.0)], 100).unwrap();
        let id = list(&c, "new").unwrap()[0].id;
        let item = accept(&c, id, false, 200).unwrap().expect("запись ставится в очередь");
        let (stage, src, ch): (String, String, i64) = c.query_row("SELECT stage,source,channel_id FROM factory_items WHERE id=?1", [item], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((stage.as_str(), src.as_str()), ("queued", "https://twitch.tv/v1"));
        assert_eq!(ch, sandbox_channel(&c).unwrap().id);
        assert!(!sandbox_channel(&c).unwrap().monitor.enabled, "песочница не опрашивается монитором");
        assert!(accept(&c, id, false, 201).is_err(), "второй раз принять нельзя");
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM factory_items", [], |r| r.get(0)).unwrap(), 1);
        // принятое больше не пересчитывается и не возвращается в «новые»
        ingest(&c, &cfg(), &[found("v1", "vod", 99_000.0)], 5000).unwrap();
        assert!(list(&c, "new").unwrap().is_empty());
        assert_eq!(list(&c, "accepted").unwrap().len(), 1);
    }

    #[test]
    fn live_goes_to_watch_without_download() {
        let c = db();
        ingest(&c, &cfg(), &[found("s1", "live", 20_000.0)], 100).unwrap();
        let id = list(&c, "new").unwrap()[0].id;
        assert_eq!(accept(&c, id, false, 200).unwrap(), None);
        assert_eq!(list(&c, "watch").unwrap().len(), 1);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM factory_items", [], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn auto_accept_needs_whitelist_and_respects_daily_limit() {
        let c = db();
        let mut k = Config { threshold: 0.3, daily_limit: 1, ..cfg() };
        // без белого списка и разрешения — только в Inbox
        let st = ingest(&c, &k, &[found("x1", "vod", 50_000.0)], 100).unwrap();
        assert_eq!(st.auto_accepted, 0);
        k.whitelist = vec!["twitch.tv/".into()];
        let st = ingest(&c, &k, &[found("x2", "vod", 50_000.0), found("x3", "vod", 60_000.0)], 200).unwrap();
        assert_eq!(st.auto_accepted, 1, "суточный лимит 1");
        assert_eq!(list(&c, "accepted").unwrap().iter().filter(|c| c.auto).count(), 1);
    }

    #[test]
    fn reject_and_config_roundtrip() {
        let c = db();
        ingest(&c, &cfg(), &[found("r1", "vod", 50_000.0)], 100).unwrap();
        let id = list(&c, "new").unwrap()[0].id;
        reject(&c, id, 150).unwrap();
        assert!(reject(&c, id, 151).is_err());
        let saved = save_config(&c, Config { interval_min: 1, region: "x".into(), keywords: vec![" а ".into(), "".into()], ..Default::default() }).unwrap();
        assert_eq!((saved.interval_min, saved.region.as_str(), saved.keywords.clone()), (5, "RU", vec!["а".to_string()]));
        assert_eq!(load_config(&c), saved);
    }

    #[test]
    fn processed_source_is_not_offered_again() {
        let c = db();
        c.execute("INSERT INTO source_items(external_id,source,title) VALUES('old','https://twitch.tv/p1','уже было')", []).unwrap();
        let st = ingest(&c, &cfg(), &[found("p1", "vod", 50_000.0)], 100).unwrap();
        assert_eq!(st.shown, 0);
    }
}
