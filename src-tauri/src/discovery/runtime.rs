//! Автопоиск в работе: опрос провайдеров по таймеру (когда поток `discovery` включён) и команды окна.
use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};

use super::{
    providers::{Found, Net},
    store::{self, Candidate, Config, Stats},
};
use crate::{
    analysis_inputs::{TwitchCredentials, TWITCH_KEY},
    db::{kv_get, kv_set},
    host::{AppHandle, Emitter, Manager, State},
    state::AppState,
    workflows::{runtime::is_enabled, Flow},
};

type CmdResult<T> = Result<T, String>;

const KV_STATUS: &str = "discovery_status";
const KV_LAST: &str = "discovery_last_poll";
const KEY_YOUTUBE: &str = "discovery_youtube_key";
const KEY_KICK: &str = "discovery_kick_token";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub configured: bool,
    pub ok: Option<bool>,
    pub at: i64,
    pub found: usize,
    pub message: String,
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let due = {
                let state = app.state::<AppState>();
                let db = state.db.0.lock().unwrap();
                let cfg = store::load_config(&db);
                let last: i64 = kv_get(&db, KV_LAST).and_then(|v| v.parse().ok()).unwrap_or(0);
                is_enabled(&db, Flow::Discovery) && now() - last >= i64::from(cfg.interval_min) * 60
            };
            if due {
                let _ = poll(&app, &Net::default()).await;
            }
        }
    });
}

/// Один круг опроса. Каждый провайдер сам по себе: сбой одного не мешает остальным, итог каждого виден в статусе.
pub async fn poll(app: &AppHandle, net: &Net) -> CmdResult<Stats> {
    let state = app.state::<AppState>();
    let cfg = {
        let db = state.db.0.lock().unwrap();
        // отметка до запроса: сбой сети не превращается в опрос каждую минуту
        let _ = kv_set(&db, KV_LAST, &now().to_string());
        store::load_config(&db)
    };
    let mut status: BTreeMap<String, ProviderStatus> = BTreeMap::new();
    let mut founds: Vec<Found> = Vec::new();
    let mut put = |name: &str, configured: bool, r: Option<Result<usize, String>>| {
        let (ok, message, found) = match r {
            None => (None, "не настроено".to_string(), 0),
            Some(Ok(n)) => (Some(true), String::new(), n),
            Some(Err(e)) => (Some(false), e, 0),
        };
        status.insert(name.into(), ProviderStatus { configured, ok, at: now(), found, message });
    };

    // Twitch
    if cfg.twitch {
        let creds = state.secrets.get(TWITCH_KEY).await.ok().flatten().and_then(|v| serde_json::from_str::<TwitchCredentials>(&v).ok());
        match creds {
            None => put("twitch", false, None),
            Some(c) => {
                let lang = cfg.languages.first().cloned().unwrap_or_default();
                match net.twitch(&c.client_id, &c.client_secret, &lang, cfg.games_top).await {
                    Ok(f) => {
                        put("twitch", true, Some(Ok(f.len())));
                        founds.extend(f);
                    }
                    Err(e) => put("twitch", true, Some(Err(format!("{e:#}")))),
                }
            }
        }
    }
    // Kick
    if cfg.kick {
        match state.secrets.get(KEY_KICK).await.ok().flatten() {
            None => put("kick", false, None),
            Some(t) => match net.kick(&t).await {
                Ok(f) => {
                    put("kick", true, Some(Ok(f.len())));
                    founds.extend(f);
                }
                Err(e) => put("kick", true, Some(Err(format!("{e:#}")))),
            },
        }
    }
    // YouTube
    if cfg.youtube {
        match state.secrets.get(KEY_YOUTUBE).await.ok().flatten() {
            None => put("youtube", false, None),
            Some(k) => match net.youtube(&k, &cfg.region).await {
                Ok(f) => {
                    put("youtube", true, Some(Ok(f.len())));
                    founds.extend(f);
                }
                Err(e) => put("youtube", true, Some(Err(format!("{e:#}")))),
            },
        }
    }
    // Темы дня
    if cfg.topics {
        let lang = cfg.languages.first().cloned().unwrap_or_else(|| "ru".into());
        match net.topics(&cfg.region, &lang).await {
            Ok(t) => {
                let n = t.len();
                let db = state.db.0.lock().unwrap();
                let _ = store::save_topics(&db, &t);
                drop(db);
                put("topics", true, Some(Ok(n)));
            }
            Err(e) => put("topics", true, Some(Err(format!("{e:#}")))),
        }
    }

    let stats = {
        let db = state.db.0.lock().unwrap();
        let _ = kv_set(&db, KV_STATUS, &serde_json::to_string(&status).unwrap_or_default());
        store::ingest(&db, &cfg, &founds, now())?
    };
    let _ = app.emit("discovery-changed", ());
    if stats.auto_accepted > 0 {
        crate::factory::start(app);
    }
    Ok(stats)
}

// ------------------------------------------------------------------ команды окна

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryState {
    pub config: Config,
    pub new: Vec<Candidate>,
    pub watch: Vec<Candidate>,
    pub accepted: Vec<Candidate>,
    pub providers: BTreeMap<String, ProviderStatus>,
    pub topics: Vec<String>,
    pub last_poll: i64,
    pub flow_enabled: bool,
    pub youtube_key: bool,
    pub kick_token: bool,
    pub twitch_keys: bool,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn discovery_state(state: State<'_, AppState>) -> CmdResult<DiscoveryState> {
    let youtube_key = state.secrets.get(KEY_YOUTUBE).await.ok().flatten().is_some();
    let kick_token = state.secrets.get(KEY_KICK).await.ok().flatten().is_some();
    let twitch_keys = state.secrets.get(TWITCH_KEY).await.ok().flatten().is_some();
    let db = state.db.0.lock().unwrap();
    let e = |e: rusqlite::Error| e.to_string();
    let mut providers: BTreeMap<String, ProviderStatus> = kv_get(&db, KV_STATUS).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    // «настроено» берём из ключей сейчас, а не из последнего опроса
    for (name, on) in [("twitch", twitch_keys), ("kick", kick_token), ("youtube", youtube_key), ("topics", true)] {
        providers.entry(name.into()).or_default().configured = on;
    }
    Ok(DiscoveryState {
        config: store::load_config(&db),
        new: store::list(&db, "new").map_err(e)?,
        watch: store::list(&db, "watch").map_err(e)?,
        accepted: store::list(&db, "accepted").map_err(e)?,
        providers,
        topics: store::load_topics(&db),
        last_poll: kv_get(&db, KV_LAST).and_then(|v| v.parse().ok()).unwrap_or(0),
        flow_enabled: is_enabled(&db, Flow::Discovery),
        youtube_key,
        kick_token,
        twitch_keys,
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn discovery_config_set(app: AppHandle, config: Config) -> CmdResult<Config> {
    let state = app.state::<AppState>();
    let saved = store::save_config(&state.db.0.lock().unwrap(), config).map_err(|e| e.to_string())?;
    let _ = app.emit("discovery-changed", ());
    Ok(saved)
}

/// Ключ YouTube Data API или токен Kick. Пустое значение удаляет. Ключи Twitch задаются в настройках анализа.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn discovery_secret_set(state: State<'_, AppState>, name: String, value: String) -> CmdResult<()> {
    let key = match name.as_str() {
        "youtube" => KEY_YOUTUBE,
        "kick" => KEY_KICK,
        _ => return Err("Неизвестный ключ".into()),
    };
    let value = value.trim();
    if value.is_empty() {
        return state.secrets.delete(key).await.map_err(|e| e.to_string());
    }
    if value.len() > 512 || value.chars().any(char::is_whitespace) {
        return Err("Ключ выглядит неверно".into());
    }
    state.secrets.set(key, value).await.map_err(|e| e.to_string())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn discovery_poll_now(app: AppHandle) -> CmdResult<Stats> {
    poll(&app, &Net::default()).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn discovery_accept(app: AppHandle, id: i64) -> CmdResult<Option<i64>> {
    let state = app.state::<AppState>();
    let item = store::accept(&state.db.0.lock().unwrap(), id, false, now())?;
    let _ = app.emit("discovery-changed", ());
    if item.is_some() {
        crate::factory::start(&app);
    }
    Ok(item)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn discovery_reject(app: AppHandle, id: i64) -> CmdResult<()> {
    let state = app.state::<AppState>();
    store::reject(&state.db.0.lock().unwrap(), id, now())?;
    let _ = app.emit("discovery-changed", ());
    Ok(())
}

/// Проверка ключа интеграции одним запросом: `twitch`, `youtube` или `kick`. Ключи не показываются.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn integration_test(state: State<'_, AppState>, name: String) -> CmdResult<String> {
    let net = Net::default();
    let region = store::load_config(&state.db.0.lock().unwrap()).region;
    match name.as_str() {
        "twitch" => {
            let v = state.secrets.get(TWITCH_KEY).await.ok().flatten().ok_or("Ключи Twitch не заданы")?;
            let c: TwitchCredentials = serde_json::from_str(&v).map_err(|_| "Повреждены настройки Twitch")?;
            net.twitch_token(&c.client_id, &c.client_secret).await.map_err(|e| format!("{e:#}"))?;
            Ok("Twitch: токен получен, ключи рабочие".into())
        }
        "youtube" => {
            let k = state.secrets.get(KEY_YOUTUBE).await.ok().flatten().ok_or("Ключ YouTube не задан")?;
            let n = net.youtube(&k, &region).await.map_err(|e| format!("{e:#}"))?.len();
            Ok(format!("YouTube: ключ рабочий, получено видео: {n}"))
        }
        "kick" => {
            let t = state.secrets.get(KEY_KICK).await.ok().flatten().ok_or("Токен Kick не задан")?;
            let n = net.kick(&t).await.map_err(|e| format!("{e:#}"))?.len();
            Ok(format!("Kick: токен рабочий, эфиров: {n}"))
        }
        _ => Err("Неизвестная интеграция".into()),
    }
}
