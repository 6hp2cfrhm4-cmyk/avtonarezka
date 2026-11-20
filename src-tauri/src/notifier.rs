//! Уведомления на телефон через Telegram-бота: «нужна проверка», ошибки, итог ночи. Работает и на сервере, пока ПК выключен.
//! Только отправка сообщений вам: токен берётся из защищённого хранилища, текст чатов и имена не пересылаются.
use std::time::Duration;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::{
    db::{kv_get, kv_set},
    host::{AppHandle, Manager, State},
    state::AppState,
};

type CmdResult<T> = Result<T, String>;

const KV_CFG: &str = "notify_config";
const KV_LAST_EVENT: &str = "notify_last_event";
const KV_LAST_SUMMARY: &str = "notify_last_summary_day";
const KEY_TOKEN: &str = "notify_bot_token";
const ENDPOINT: &str = "https://api.telegram.org";
/// Не больше стольких сообщений в час: серия ошибок не должна засыпать телефон.
const MAX_PER_HOUR: usize = 12;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    /// Числовой chat_id (бот пишет вам) или @канал.
    pub chat: String,
    /// Час (0–23) ежедневной сводки; 24 — не слать.
    pub summary_hour: u32,
    pub attention: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self { enabled: false, chat: String::new(), summary_hour: 9, attention: true }
    }
}

pub fn load_config(db: &Connection) -> Config {
    kv_get(db, KV_CFG).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
}

fn valid_token(t: &str) -> bool {
    t.contains(':') && !t.is_empty() && t.len() < 200 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'_' || b == b'-')
}

/// Текст сообщения в виде, пригодном для отправки: без управляющих знаков, не длиннее 1000 знаков.
pub fn clean(text: &str) -> String {
    let t: String = text.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    t.chars().take(1000).collect()
}

pub async fn send(endpoint: &str, token: &str, chat: &str, text: &str) -> Result<(), String> {
    if !valid_token(token) {
        return Err("Некорректный токен бота".into());
    }
    let chat = chat.trim();
    if chat.is_empty() || chat.contains(char::is_whitespace) {
        return Err("Укажите chat_id или @канал".into());
    }
    // токен уходит только на Bot API Telegram (или на loopback в тестах)
    let url = reqwest::Url::parse(endpoint).map_err(|_| "Некорректный адрес Bot API")?;
    let local = url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    if !(local || (url.scheme() == "https" && url.host_str() == Some("api.telegram.org"))) {
        return Err("Токен можно отправлять только в Telegram".into());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string())?;
    let r = client
        .post(format!("{}/bot{token}/sendMessage", endpoint.trim_end_matches('/')))
        .json(&serde_json::json!({"chat_id": chat, "text": clean(text), "disable_web_page_preview": true}))
        .send()
        .await
        .map_err(|e| format!("Telegram недоступен: {}", e.without_url()))?;
    if !r.status().is_success() {
        return Err(format!("Telegram ответил {}", r.status()));
    }
    Ok(())
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            tick(&app).await;
        }
    });
}

/// События, о которых стоит знать: «внимание» из фабрики и эфиров. Возвращает новые, не отправленные раньше.
pub fn pending_attention(db: &Connection, limit: usize) -> Vec<(i64, String)> {
    let last: i64 = kv_get(db, KV_LAST_EVENT).and_then(|v| v.parse().ok()).unwrap_or_else(|| db.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0)).unwrap_or(0));
    db.prepare("SELECT id,message FROM events WHERE id>?1 AND kind='attention' ORDER BY id LIMIT ?2")
        .and_then(|mut s| s.query_map(params![last, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default()
}

pub fn summary_text(db: &Connection, since: i64) -> String {
    let n = |sql: &str| db.query_row::<i64, _, _>(sql, [since], |r| r.get(0)).unwrap_or(0);
    format!(
        "Сводка за сутки\nГотово роликов: {}\nС ошибкой/проверкой: {}\nМоментов из эфиров: {}\nВыложено: {}\nНовых кандидатов в трендах: {}",
        n("SELECT COUNT(*) FROM events WHERE kind='stage' AND stage='completed' AND created_at>=?1"),
        n("SELECT COUNT(*) FROM events WHERE kind='attention' AND created_at>=?1"),
        n("SELECT COUNT(*) FROM live_moments WHERE state='cut' AND created_at>=?1"),
        n("SELECT COUNT(*) FROM publications WHERE state='sent' AND sent_at>=?1"),
        n("SELECT COUNT(*) FROM discovery_candidates WHERE first_seen>=?1"),
    )
}

async fn tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (cfg, events, summary) = {
        let db = state.db.0.lock().unwrap();
        let cfg = load_config(&db);
        if !cfg.enabled || cfg.chat.is_empty() {
            return;
        }
        let events = if cfg.attention { pending_attention(&db, 20) } else { vec![] };
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let due = cfg.summary_hour < 24 && chrono::Timelike::hour(&now) >= cfg.summary_hour && kv_get(&db, KV_LAST_SUMMARY).as_deref() != Some(&today);
        let summary = due.then(|| (today, summary_text(&db, chrono::Utc::now().timestamp() - 86400)));
        (cfg, events, summary)
    };
    let Some(token) = state.secrets.get(KEY_TOKEN).await.ok().flatten() else { return };
    // предохранитель от шквала: за последний час уже ушло много сообщений
    let hour_key = "notify_sent_hour";
    let (mut sent_in_hour, mut hour) = {
        let db = state.db.0.lock().unwrap();
        let raw = kv_get(&db, hour_key).unwrap_or_default();
        let mut p = raw.split(':');
        (p.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0), p.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0))
    };
    let cur_hour = chrono::Utc::now().timestamp() / 3600;
    if hour != cur_hour {
        sent_in_hour = 0;
        hour = cur_hour;
    }
    let mut last_ok = None;
    for (id, msg) in &events {
        if sent_in_hour >= MAX_PER_HOUR {
            break;
        }
        if send(ENDPOINT, &token, &cfg.chat, &format!("⚠ Автонарезка: {msg}")).await.is_err() {
            break; // повтор — на следующем круге; события не теряются
        }
        sent_in_hour += 1;
        last_ok = Some(*id);
    }
    {
        let db = state.db.0.lock().unwrap();
        if let Some(id) = last_ok {
            let _ = kv_set(&db, KV_LAST_EVENT, &id.to_string());
        } else if events.is_empty() && kv_get(&db, KV_LAST_EVENT).is_none() {
            // первый запуск: старое не пересылаем
            let _ = kv_set(&db, KV_LAST_EVENT, &db.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get::<_, i64>(0)).unwrap_or(0).to_string());
        }
    }
    if let Some((day, text)) = summary {
        if sent_in_hour < MAX_PER_HOUR && send(ENDPOINT, &token, &cfg.chat, &text).await.is_ok() {
            sent_in_hour += 1;
            let _ = kv_set(&state.db.0.lock().unwrap(), KV_LAST_SUMMARY, &day);
        }
    }
    let _ = kv_set(&state.db.0.lock().unwrap(), hour_key, &format!("{sent_in_hour}:{hour}"));
}

// ------------------------------------------------------------------ команды окна

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyState {
    pub config: Config,
    pub token: bool,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn notify_state(state: State<'_, AppState>) -> CmdResult<NotifyState> {
    let token = state.secrets.get(KEY_TOKEN).await.ok().flatten().is_some();
    let db = state.db.0.lock().unwrap();
    Ok(NotifyState { config: load_config(&db), token })
}

/// Сохраняет настройки; непустой `token` заменяет токен, `Some("")` удаляет.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn notify_set(state: State<'_, AppState>, config: Config, token: Option<String>) -> CmdResult<()> {
    let mut config = config;
    config.chat = config.chat.trim().to_string();
    config.summary_hour = config.summary_hour.min(24);
    if let Some(t) = token {
        let t = t.trim();
        if t.is_empty() {
            state.secrets.delete(KEY_TOKEN).await.map_err(|e| e.to_string())?;
        } else if !valid_token(t) {
            return Err("Токен бота выглядит неверно (ожидается 123456:ABC…)".into());
        } else {
            state.secrets.set(KEY_TOKEN, t).await.map_err(|e| e.to_string())?;
        }
    }
    kv_set(&state.db.0.lock().unwrap(), KV_CFG, &serde_json::to_string(&config).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn notify_test(state: State<'_, AppState>) -> CmdResult<()> {
    let cfg = load_config(&state.db.0.lock().unwrap());
    let token = state.secrets.get(KEY_TOKEN).await.ok().flatten().ok_or("Токен бота не сохранён")?;
    send(ENDPOINT, &token, &cfg.chat, "Автонарезка: проверка связи. Если вы это видите, уведомления работают.").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn token_and_text_are_sanitised() {
        assert!(valid_token("123456:ABC-def_1"));
        assert!(!valid_token("no-colon"));
        assert!(!valid_token("12:abc def"));
        assert_eq!(clean("a\u{0007}b\nc"), "ab\nc");
        assert_eq!(clean(&"я".repeat(2000)).chars().count(), 1000);
    }

    #[tokio::test]
    async fn token_never_goes_to_foreign_host() {
        let e = send("https://evil.example", "123:abc", "42", "x").await.unwrap_err();
        assert!(e.contains("только в Telegram"), "{e}");
        assert!(send("http://evil.example", "123:abc", "42", "x").await.is_err());
        assert!(send(ENDPOINT, "bad", "42", "x").await.unwrap_err().contains("токен"));
    }

    #[tokio::test]
    async fn sends_json_to_bot_api_on_loopback() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        let got = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let g = got.clone();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            *g.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").await;
        });
        send(&addr, "123:abc", "42", "привет").await.unwrap();
        let req = got.lock().unwrap().clone();
        assert!(req.starts_with("POST /bot123:abc/sendMessage"), "{req}");
        assert!(req.contains("\"chat_id\":\"42\"") && req.contains("привет"));
    }

    #[test]
    fn attention_events_are_picked_once_and_summary_counts() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute("INSERT INTO events(kind,stage,message) VALUES('attention','failed','старое')", []).unwrap();
        // первый запуск: старое пропускаем — курсор ставится на последнее событие
        kv_set(&db, KV_LAST_EVENT, "1").unwrap();
        db.execute("INSERT INTO events(kind,stage,message) VALUES('stage','completed','ok')", []).unwrap();
        db.execute("INSERT INTO events(kind,stage,message) VALUES('attention','needs_check','нужна проверка')", []).unwrap();
        let p = pending_attention(&db, 10);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, "нужна проверка");
        let s = summary_text(&db, 0);
        assert!(s.contains("Готово роликов: 1") && s.contains("С ошибкой/проверкой: 2"), "{s}");
    }
}
