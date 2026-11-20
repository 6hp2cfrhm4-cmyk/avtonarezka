//! Helix Get Clips: запрос по broadcaster + датам, затем фильтр video_id.
//! Проверено по https://dev.twitch.tv/docs/api/reference/clips#get-clips 2026-11-14:
//! vod_offset — секунды от начала VOD, nullable и появляется с задержкой.
use super::{audio, HOP};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    #[serde(default)]
    pub video_id: String,
    pub vod_offset: Option<f64>,
    pub duration: f64,
    #[serde(default)]
    pub view_count: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipFeatures {
    pub video_id: String,
    pub clips: usize,
    pub missing_offset: usize,
    pub truncated: bool,
    pub curve: Vec<f32>,
}
#[derive(Deserialize)]
struct Page {
    data: Vec<Clip>,
    #[serde(default)]
    pagination: Pagination,
}
#[derive(Default, Deserialize)]
struct Pagination {
    cursor: Option<String>,
}

pub fn features(items: Vec<Clip>, video_id: &str, duration: f64, truncated: bool) -> ClipFeatures {
    let mut counts = vec![0.0f32; (duration / HOP).ceil() as usize];
    let mut seen = HashSet::new();
    let mut clips = 0;
    let mut missing_offset = 0;
    for c in items.into_iter().filter(|c| c.video_id == video_id) {
        if !seen.insert(c.id) {
            continue;
        }
        let Some(at) = c.vod_offset else {
            missing_offset += 1;
            continue;
        };
        if !at.is_finite()
            || at < 0.0
            || at >= duration
            || !c.duration.is_finite()
            || c.duration <= 0.0
        {
            continue;
        }
        let a = (at / HOP).floor() as usize;
        let b =
            (((at + c.duration.min(180.0)).min(duration) / HOP).ceil() as usize).min(counts.len());
        for x in &mut counts[a..b] {
            *x += 1.0 + (1.0 + c.view_count as f32).ln().min(8.0) * 0.1;
        }
        clips += 1;
    }
    let curve = audio::robust_z(&counts);
    ClipFeatures {
        video_id: video_id.into(),
        clips,
        missing_offset,
        truncated,
        curve,
    }
}

pub fn parse(bytes: &[u8], video_id: &str, duration: f64) -> anyhow::Result<ClipFeatures> {
    if bytes.len() > 16 * 1024 * 1024
        || !duration.is_finite()
        || !(0.0..=86400.0).contains(&duration)
        || duration == 0.0
    {
        bail!("Некорректный файл клипов или длительность");
    }
    let page: Page = serde_json::from_slice(bytes).context("Нужен JSON ответа Helix Get Clips")?;
    let truncated = page.pagination.cursor.is_some();
    Ok(features(page.data, video_id, duration, truncated))
}

pub struct TwitchApi {
    pub base: String,
    pub token_url: String,
    pub client: reqwest::Client,
}
impl Default for TwitchApi {
    fn default() -> Self {
        Self {
            base: "https://api.twitch.tv/helix".into(),
            token_url: "https://id.twitch.tv/oauth2/token".into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
        }
    }
}
impl TwitchApi {
    pub async fn collect(
        &self,
        id: &str,
        secret: &str,
        video_id: &str,
        duration: f64,
    ) -> anyhow::Result<ClipFeatures> {
        if video_id.is_empty() || !video_id.chars().all(|c| c.is_ascii_digit()) {
            bail!("Нужен числовой ID Twitch VOD");
        }
        if !duration.is_finite() || duration <= 0.0 || duration > 86400.0 {
            bail!("Некорректная длительность VOD");
        }
        let response = self
            .client
            .post(&self.token_url)
            .form(&[
                ("client_id", id),
                ("client_secret", secret),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "Twitch: не удалось получить токен (HTTP {})",
                response.status()
            );
        }
        let token: serde_json::Value = response.json().await?;
        let token = token
            .get("access_token")
            .and_then(|v| v.as_str())
            .context("Twitch не вернул токен")?;
        let mut url = reqwest::Url::parse(&format!("{}/videos", self.base))?;
        url.query_pairs_mut().append_pair("id", video_id);
        let response = self
            .client
            .get(url)
            .header("Client-Id", id)
            .bearer_auth(token)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Twitch VOD: HTTP {}", response.status());
        }
        let video: serde_json::Value = response.json().await?;
        let video = video.pointer("/data/0").context("Twitch VOD недоступен")?;
        let broadcaster = video
            .get("user_id")
            .and_then(|v| v.as_str())
            .context("Нет broadcaster ID")?;
        let started = video
            .get("created_at")
            .and_then(|v| v.as_str())
            .context("Нет даты VOD")?;
        let start = chrono::DateTime::parse_from_rfc3339(started)?;
        let end = (start + chrono::Duration::seconds(duration.ceil() as i64 + 60)).to_rfc3339();
        let mut cursor: Option<String> = None;
        let mut data = Vec::new();
        let mut seen = HashSet::new();
        for page_index in 0..10 {
            let mut query = vec![
                ("broadcaster_id", broadcaster.to_string()),
                ("started_at", started.to_string()),
                ("ended_at", end.clone()),
                ("first", "100".into()),
            ];
            if let Some(c) = cursor.as_ref() {
                query.push(("after", c.clone()));
            }
            let mut url = reqwest::Url::parse(&format!("{}/clips", self.base))?;
            url.query_pairs_mut().extend_pairs(query);
            let response = self
                .client
                .get(url)
                .header("Client-Id", id)
                .bearer_auth(token)
                .send()
                .await?;
            // Квота/авторизация не маскируются пустым результатом. Следующий запуск повторит запрос.
            if !response.status().is_success() {
                bail!("Twitch Clips: HTTP {}", response.status());
            }
            let page: Page = response.json().await?;
            data.extend(page.data);
            cursor = page.pagination.cursor.filter(|v| !v.is_empty());
            if cursor.is_none() {
                return Ok(features(data, video_id, duration, false));
            }
            if page_index == 9 || !seen.insert(cursor.clone().unwrap()) {
                return Ok(features(data, video_id, duration, true));
            }
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_matching_vod_offsets_count_and_duplicates_do_not_amplify() {
        let bytes = br#"{"data":[{"id":"a","video_id":"1","vod_offset":120,"duration":20,"view_count":5},{"id":"a","video_id":"1","vod_offset":120,"duration":20},{"id":"b","video_id":"1","vod_offset":null,"duration":10},{"id":"c","video_id":"2","vod_offset":20,"duration":10}]}"#;
        let f = parse(bytes, "1", 300.0).unwrap();
        assert_eq!((f.clips, f.missing_offset), (1, 1));
        assert!(f.curve[250] > 3.0);
        assert!(f.curve[40] <= 0.0);
    }
}
