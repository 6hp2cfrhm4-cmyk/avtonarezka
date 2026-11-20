//! Только чтение официальных API. Не повторяет публикации и не скрейпит страницы.
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub views: Option<i64>,
    pub likes: Option<i64>,
    pub comments: Option<i64>,
    pub shares: Option<i64>,
}
impl Counts {
    pub fn validate(&self) -> Result<(), String> {
        let all = [self.views, self.likes, self.comments, self.shares];
        if all.iter().all(Option::is_none) {
            return Err("Укажи хотя бы одну доступную метрику".into());
        }
        if all
            .into_iter()
            .flatten()
            .any(|n| !(0..=1_000_000_000_000).contains(&n))
        {
            return Err("Метрики должны быть целыми числами от 0 до 1 000 000 000 000".into());
        }
        Ok(())
    }
}
#[derive(Debug, PartialEq)]
pub enum Error {
    Auth,
    Rate(u64),
    Unavailable,
    Invalid,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auth => {
                "Нет доступа к статистике. Проверь права приложения и подключи аккаунт заново"
            }
            Self::Rate(_) => "Площадка ограничила частоту запросов или исчерпала квоту",
            Self::Unavailable => {
                "Статистика пока недоступна: ролик удалён, скрыт или метрики не поддерживаются"
            }
            Self::Invalid => "Площадка вернула некорректную статистику",
        })
    }
}
pub struct Collector {
    http: Client,
    base: Url,
    token: String,
    platform: &'static str,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}
fn count(v: &Value) -> Result<Option<i64>, Error> {
    if v.is_null() {
        return Ok(None);
    }
    let n = v
        .as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .ok_or(Error::Invalid)?;
    if !(0..=1_000_000_000_000).contains(&n) {
        return Err(Error::Invalid);
    }
    Ok(Some(n))
}
impl Collector {
    pub fn new(platform: &str, token: String, endpoint: Option<&str>) -> Result<Self, Error> {
        let (platform, host) = match platform {
            "youtube" => ("youtube", "www.googleapis.com"),
            "instagram" => ("instagram", "graph.facebook.com"),
            "vk" => ("vk", "api.vk.com"),
            _ => return Err(Error::Unavailable),
        };
        let base = crate::publish::youtube::endpoint(
            endpoint.unwrap_or(&format!("https://{host}/")),
            host,
        )
        .map_err(|_| Error::Invalid)?;
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Auth);
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| Error::Invalid)?;
        Ok(Self {
            http,
            base,
            token,
            platform,
        })
    }
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, Error> {
        let mut url = self.base.join(path).map_err(|_| Error::Invalid)?;
        // VK's documented method parameters differ from Google/Meta. The URL is
        // never included in an error; redirects are disabled before attaching a token.
        let request = if self.platform == "vk" {
            // The official VK PHP SDK sends method parameters as a POST form.
            // video.get is read-only; the token must not appear in the URL.
            let mut form = query.to_vec();
            form.extend([("v", "5.199"), ("access_token", self.token.as_str())]);
            self.http.post(url).form(&form)
        } else {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
            self.http.get(url).bearer_auth(&self.token)
        };
        let response = request.send().await.map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(Error::Unavailable);
        }
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600)
            .clamp(60, 86400);
        if status.as_u16() == 401 {
            return Err(Error::Auth);
        }
        if status.as_u16() == 429 {
            return Err(Error::Rate(retry));
        }
        if response.content_length().is_some_and(|l| l > 1_000_000) {
            return Err(Error::Invalid);
        }
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::Unavailable)?;
            if data.len() + chunk.len() > 1_000_000 {
                return Err(Error::Invalid);
            }
            data.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&data).map_err(|_| Error::Invalid)?;
        // VK errors use HTTP 200 and error_code, not Graph's error.code.
        if self.platform == "vk" {
            match v["error"]["error_code"].as_i64() {
                Some(5 | 7 | 14 | 15 | 27 | 28 | 203) => return Err(Error::Auth),
                Some(6 | 9 | 29) => return Err(Error::Rate(retry)),
                Some(_) => return Err(Error::Unavailable),
                None => {}
            }
        }
        if v["error"]["code"].as_i64() == Some(190) {
            return Err(Error::Auth);
        }
        let reason = v["error"]["errors"][0]["reason"].as_str().unwrap_or("");
        if matches!(v["error"]["code"].as_i64(), Some(4 | 17 | 32 | 613))
            || matches!(
                reason,
                "quotaExceeded" | "dailyLimitExceeded" | "rateLimitExceeded"
            )
        {
            return Err(Error::Rate(retry));
        }
        if status.as_u16() == 403 {
            return Err(Error::Auth);
        }
        if !status.is_success() || v.get("error").is_some() {
            return Err(Error::Unavailable);
        }
        Ok(v)
    }
    pub async fn youtube(&self, video: &str, channel: &str) -> Result<Counts, Error> {
        if !valid_id(video) || !valid_id(channel) {
            return Err(Error::Invalid);
        }
        let v = self
            .get(
                "youtube/v3/videos",
                &[("part", "statistics,snippet"), ("id", video)],
            )
            .await?;
        let items = v["items"]
            .as_array()
            .filter(|a| a.len() == 1)
            .ok_or(Error::Unavailable)?;
        let row = &items[0];
        if row["id"].as_str() != Some(video)
            || row["snippet"]["channelId"].as_str() != Some(channel)
        {
            return Err(Error::Auth);
        }
        let s = &row["statistics"];
        let counts = Counts {
            views: count(&s["viewCount"])?,
            likes: count(&s["likeCount"])?,
            comments: count(&s["commentCount"])?,
            shares: None,
        };
        counts.validate().map_err(|_| Error::Unavailable)?;
        Ok(counts)
    }
    pub async fn instagram(&self, media: &str, user: &str) -> Result<Counts, Error> {
        if !valid_id(media) || !valid_id(user) {
            return Err(Error::Invalid);
        }
        let owner = self
            .get(&format!("v27.0/{media}"), &[("fields", "id,owner")])
            .await?;
        if owner["id"].as_str() != Some(media) || owner["owner"]["id"].as_str() != Some(user) {
            return Err(Error::Auth);
        }
        let v = self
            .get(
                &format!("v27.0/{media}/insights"),
                &[("metric", "views,likes,comments,shares")],
            )
            .await?;
        let mut counts = Counts::default();
        for m in v["data"].as_array().ok_or(Error::Invalid)? {
            let value = if let Some(total) = m.get("total_value") {
                count(&total["value"])?
            } else {
                let values = m["values"]
                    .as_array()
                    .filter(|a| a.len() == 1)
                    .ok_or(Error::Invalid)?;
                count(&values[0]["value"])?
            };
            match m["name"].as_str() {
                Some("views") => counts.views = value,
                Some("likes") => counts.likes = value,
                Some("comments") => counts.comments = value,
                Some("shares") => counts.shares = value,
                _ => {}
            }
        }
        counts.validate().map_err(|_| Error::Unavailable)?;
        Ok(counts)
    }

    /// `video.get`, VKCOM/vk-api-schema @333481bd082ad747d4873ef4a77f9247097eeef0.
    /// Requires a user token. The publication ID is `<signed_owner_id>_<video_id>`;
    /// community owners have a negative ID. Never accept an access_key/URL here.
    pub async fn vk(&self, video: &str, owner: &str) -> Result<Counts, Error> {
        fn number(raw: &str) -> Option<i64> {
            let n = raw.parse::<i64>().ok()?;
            (n != 0 && n.to_string() == raw).then_some(n)
        }
        if self.platform != "vk" {
            return Err(Error::Invalid);
        }
        let (video_owner, media) = video.split_once('_').ok_or(Error::Invalid)?;
        let owner_id = number(owner).ok_or(Error::Invalid)?;
        let media_id = number(media).filter(|n| *n > 0).ok_or(Error::Invalid)?;
        if number(video_owner) != Some(owner_id) {
            return Err(Error::Invalid);
        }
        let v = self
            .get(
                "method/video.get",
                &[("videos", video), ("count", "1"), ("extended", "0")],
            )
            .await?;
        let items = v["response"]["items"]
            .as_array()
            .filter(|items| items.len() == 1)
            .ok_or(Error::Unavailable)?;
        let row = &items[0];
        if row["owner_id"].as_i64() != Some(owner_id) || row["id"].as_i64() != Some(media_id) {
            return Err(Error::Auth);
        }
        let counts = Counts {
            views: count(&row["views"])?,
            likes: count(&row["likes"]["count"])?,
            comments: count(&row["comments"])?,
            shares: count(&row["reposts"]["count"])?,
        };
        counts.validate().map_err(|_| Error::Unavailable)?;
        Ok(counts)
    }
}
