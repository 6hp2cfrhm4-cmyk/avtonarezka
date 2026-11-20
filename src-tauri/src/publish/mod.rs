//! Общие возможности и транспорт площадок. Единственная очередь остаётся publish_queue;
//! этот модуль не создаёт фоновых отправок и ничего не публикует при запуске.
pub mod autofill;
pub mod browser;
pub mod calendar;
pub mod instagram;
pub mod oauth;
pub mod output;
pub mod schedule;
pub mod telegram;
pub mod tiktok;
pub mod youtube;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlatformId {
    Tiktok,
    Youtube,
    Instagram,
    Vk,
    Telegram,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Caps {
    pub platform: PlatformId,
    pub name: &'static str,
    pub caption_max: usize,
    pub title_max: usize,
    pub hashtags_max: usize,
    pub duration_max: Option<f64>,
    pub file_max: Option<u64>,
    pub daily_default: u32,
    pub native_schedule: bool,
    pub login: &'static str,
}
pub const TIKTOK: Caps = Caps {
    platform: PlatformId::Tiktok,
    name: "TikTok",
    caption_max: 3999,
    title_max: 0,
    hashtags_max: 5,
    duration_max: None,
    file_max: None,
    daily_default: 5,
    native_schedule: false,
    login: "oauth_or_browser",
};
pub const TELEGRAM: Caps = Caps {
    platform: PlatformId::Telegram,
    name: "Telegram",
    caption_max: 1024,
    title_max: 0,
    hashtags_max: 30,
    duration_max: None,
    file_max: Some(50_000_000),
    daily_default: 10,
    native_schedule: false,
    login: "bot_token",
};
pub const YOUTUBE: Caps = Caps {
    platform: PlatformId::Youtube,
    name: "YouTube Shorts",
    caption_max: 5000,
    title_max: 100,
    hashtags_max: 15,
    duration_max: Some(180.0),
    file_max: Some(256_000_000_000),
    daily_default: 5,
    native_schedule: true,
    login: "google_oauth",
};
pub const INSTAGRAM: Caps = Caps {
    platform: PlatformId::Instagram,
    name: "Instagram Reels",
    caption_max: 2200,
    title_max: 0,
    hashtags_max: 30,
    duration_max: Some(900.0),
    file_max: Some(300_000_000),
    daily_default: 5,
    native_schedule: false,
    login: "facebook_page_token",
};

#[derive(Clone, Debug)]
pub struct Upload {
    pub path: PathBuf,
    pub caption: String,
    pub duration: f64,
    pub scheduled_at: Option<i64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub external_id: String,
    pub url: Option<String>,
    pub published: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Rejected,
    Auth,
    RetryAfter(u64),
    NeedsCheck,
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
pub trait Platform: Send + Sync {
    fn caps(&self) -> Caps;
    fn method(&self) -> &'static str {
        "api"
    }
    fn publish<'a>(
        &'a self,
        upload: &'a Upload,
        on: &'a mut (dyn FnMut(f64, String) + Send),
    ) -> BoxFuture<'a, Result<Receipt, Failure>>;
}

pub fn validate(upload: &Upload, caps: Caps) -> Result<(), Failure> {
    if !upload.duration.is_finite() || upload.duration <= 0.0 {
        return Err(Failure::new(
            FailureKind::Rejected,
            "Неизвестна длительность ролика",
        ));
    }
    if upload.caption.encode_utf16().count() > caps.caption_max {
        return Err(Failure::new(
            FailureKind::Rejected,
            format!("Подпись длиннее лимита {}: {}", caps.name, caps.caption_max),
        ));
    }
    if caps.duration_max.is_some_and(|m| upload.duration > m) {
        return Err(Failure::new(
            FailureKind::Rejected,
            "Ролик длиннее лимита площадки",
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn platform_caps() -> Vec<Caps> {
    vec![TIKTOK, TELEGRAM, YOUTUBE, INSTAGRAM]
}
