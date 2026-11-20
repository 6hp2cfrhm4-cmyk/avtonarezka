//! Провайдеры трендов. Разбор ответов — чистые функции (тесты на записанных ответах),
//! сетевая часть — `Net` с подменяемыми адресами. Каждый провайдер необязателен и отвечает за себя сам.
use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::Observation;

/// Найденный кандидат до оценки. `key` стабилен между опросами и нужен для снимков и дедупликации.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub key: String,
    /// live | vod | clip
    pub kind: &'static str,
    pub channel: String,
    pub obs: Observation,
}

fn s(v: &Value, p: &str) -> String {
    v.pointer(p).and_then(Value::as_str).unwrap_or_default().to_string()
}
fn n(v: &Value, p: &str) -> f64 {
    v.pointer(p).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|t| t.parse().ok()))).unwrap_or(0.0)
}

// ---------------------------------------------------------------- Twitch Helix

pub fn parse_twitch_streams(body: &[u8]) -> Result<Vec<Found>> {
    let v: Value = serde_json::from_slice(body).context("Twitch: не JSON")?;
    let data = v.get("data").and_then(Value::as_array).context("Twitch: нет data")?;
    Ok(data
        .iter()
        .filter(|d| s(d, "/type") == "live" || d.get("type").is_none())
        .filter_map(|d| {
            let login = s(d, "/user_login");
            if login.is_empty() {
                return None;
            }
            Some(Found {
                key: format!("twitch:live:{login}"),
                kind: "live",
                channel: s(d, "/user_name"),
                obs: Observation {
                    platform: "twitch".into(),
                    url: format!("https://twitch.tv/{login}"),
                    title: s(d, "/title"),
                    category: s(d, "/game_name"),
                    language: s(d, "/language"),
                    metric: n(d, "/viewer_count"),
                    previous: 0.0,
                    window_min: 0.0,
                    duration_min: None,
                },
            })
        })
        .collect())
}

pub fn parse_twitch_games(body: &[u8]) -> Result<Vec<(String, String)>> {
    let v: Value = serde_json::from_slice(body).context("Twitch: не JSON")?;
    let data = v.get("data").and_then(Value::as_array).context("Twitch: нет data")?;
    Ok(data.iter().map(|d| (s(d, "/id"), s(d, "/name"))).filter(|(i, _)| !i.is_empty()).collect())
}

pub fn parse_twitch_clips(body: &[u8], category: &str) -> Result<Vec<Found>> {
    let v: Value = serde_json::from_slice(body).context("Twitch: не JSON")?;
    let data = v.get("data").and_then(Value::as_array).context("Twitch: нет data")?;
    Ok(data
        .iter()
        .filter_map(|d| {
            let id = s(d, "/id");
            let url = s(d, "/url");
            if id.is_empty() || url.is_empty() {
                return None;
            }
            Some(Found {
                key: format!("twitch:clip:{id}"),
                kind: "clip",
                channel: s(d, "/broadcaster_name"),
                obs: Observation {
                    platform: "twitch".into(),
                    url,
                    title: s(d, "/title"),
                    category: category.to_string(),
                    language: s(d, "/language"),
                    metric: n(d, "/view_count"),
                    previous: 0.0,
                    window_min: 0.0,
                    duration_min: None,
                },
            })
        })
        .collect())
}

// ---------------------------------------------------------------- Kick

/// Официальный `GET /public/v1/livestreams` (нужен токен). Поля по документации Kick.
pub fn parse_kick_livestreams(body: &[u8]) -> Result<Vec<Found>> {
    let v: Value = serde_json::from_slice(body).context("Kick: не JSON")?;
    let data = v.get("data").and_then(Value::as_array).context("Kick: нет data")?;
    Ok(data
        .iter()
        .filter_map(|d| {
            let slug = s(d, "/slug");
            if slug.is_empty() {
                return None;
            }
            Some(Found {
                key: format!("kick:live:{slug}"),
                kind: "live",
                channel: slug.clone(),
                obs: Observation {
                    platform: "kick".into(),
                    url: format!("https://kick.com/{slug}"),
                    title: s(d, "/stream_title"),
                    category: s(d, "/category/name"),
                    language: s(d, "/language"),
                    metric: n(d, "/viewer_count"),
                    previous: 0.0,
                    window_min: 0.0,
                    duration_min: None,
                },
            })
        })
        .collect())
}

// ---------------------------------------------------------------- YouTube

/// «PT1H2M3S» → минуты.
pub fn iso_duration_min(t: &str) -> Option<f64> {
    let rest = t.strip_prefix('P')?;
    let (date, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut total = 0.0;
    let mut num = String::new();
    for c in date.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let k: f64 = num.parse().ok()?;
            num.clear();
            total += match c {
                'D' => k * 1440.0,
                _ => return None,
            };
        }
    }
    for c in time.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let k: f64 = num.parse().ok()?;
            num.clear();
            total += match c {
                'H' => k * 60.0,
                'M' => k,
                'S' => k / 60.0,
                _ => return None,
            };
        }
    }
    Some(total)
}

pub fn parse_youtube_videos(body: &[u8]) -> Result<Vec<Found>> {
    let v: Value = serde_json::from_slice(body).context("YouTube: не JSON")?;
    if let Some(msg) = v.pointer("/error/message").and_then(Value::as_str) {
        bail!("YouTube: {msg}");
    }
    let items = v.get("items").and_then(Value::as_array).context("YouTube: нет items")?;
    Ok(items
        .iter()
        .filter_map(|d| {
            let id = s(d, "/id");
            if id.is_empty() {
                return None;
            }
            let live = s(d, "/snippet/liveBroadcastContent") == "live";
            Some(Found {
                key: format!("youtube:{}:{id}", if live { "live" } else { "vod" }),
                kind: if live { "live" } else { "vod" },
                channel: s(d, "/snippet/channelTitle"),
                obs: Observation {
                    platform: "youtube".into(),
                    url: format!("https://www.youtube.com/watch?v={id}"),
                    title: s(d, "/snippet/title"),
                    category: s(d, "/snippet/categoryId"),
                    language: {
                        let l = s(d, "/snippet/defaultAudioLanguage");
                        l.split('-').next().unwrap_or_default().to_string()
                    },
                    metric: n(d, if live { "/liveStreamingDetails/concurrentViewers" } else { "/statistics/viewCount" }),
                    previous: 0.0,
                    window_min: 0.0,
                    duration_min: iso_duration_min(&s(d, "/contentDetails/duration")),
                },
            })
        })
        .collect())
}

// ---------------------------------------------------------------- Темы (Google Trends, Wikipedia)

fn unescape(t: &str) -> String {
    t.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'")
}

/// RSS Google Trends: заголовок каждого `<item>`.
pub fn parse_trends_rss(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in body.split("<item>").skip(1) {
        let chunk = chunk.split("</item>").next().unwrap_or("");
        if let Some(t) = chunk.split("<title>").nth(1).and_then(|r| r.split("</title>").next()) {
            let t = t.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>");
            let t = unescape(t);
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    out
}

/// Wikipedia «самое читаемое»: названия статей без служебных страниц.
pub fn parse_wiki_top(body: &[u8]) -> Result<Vec<String>> {
    let v: Value = serde_json::from_slice(body).context("Wikipedia: не JSON")?;
    let arts = v.pointer("/items/0/articles").and_then(Value::as_array).context("Wikipedia: нет articles")?;
    Ok(arts
        .iter()
        .map(|a| s(a, "/article").replace('_', " "))
        .filter(|t| !t.is_empty() && !t.contains(':') && t != "Заглавная страница" && t != "Main Page")
        .take(40)
        .collect())
}

// ---------------------------------------------------------------- сеть

#[derive(Clone)]
pub struct Net {
    pub twitch_base: String,
    pub twitch_token: String,
    pub kick_base: String,
    pub youtube_base: String,
    pub trends_url: String,
    pub wiki_base: String,
    pub client: reqwest::Client,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            twitch_base: "https://api.twitch.tv/helix".into(),
            twitch_token: "https://id.twitch.tv/oauth2/token".into(),
            kick_base: "https://api.kick.com/public/v1".into(),
            youtube_base: "https://www.googleapis.com/youtube/v3".into(),
            trends_url: "https://trends.google.com/trending/rss".into(),
            wiki_base: "https://wikimedia.org/api/rest_v1/metrics/pageviews/top".into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(25))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("avtonarezka/0.3 (+trend radar)")
                .build()
                .expect("HTTP client"),
        }
    }
}

fn with_query(base: &str, q: &[(&str, &str)]) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(base)?;
    url.query_pairs_mut().extend_pairs(q);
    Ok(url)
}

async fn ok_bytes(r: reqwest::Response, what: &str) -> Result<Vec<u8>> {
    let st = r.status();
    if !st.is_success() {
        bail!("{what}: HTTP {st}");
    }
    Ok(r.bytes().await?.to_vec())
}

impl Net {
    pub async fn twitch_token(&self, id: &str, secret: &str) -> Result<String> {
        let r = self
            .client
            .post(&self.twitch_token)
            .form(&[("client_id", id), ("client_secret", secret), ("grant_type", "client_credentials")])
            .send()
            .await?;
        let b = ok_bytes(r, "Twitch: токен").await?;
        let v: Value = serde_json::from_slice(&b)?;
        v.get("access_token").and_then(Value::as_str).map(str::to_string).context("Twitch не вернул токен")
    }

    async fn twitch_get(&self, id: &str, token: &str, path: &str, q: &[(&str, &str)]) -> Result<Vec<u8>> {
        let mut url = reqwest::Url::parse(&format!("{}/{path}", self.twitch_base))?;
        url.query_pairs_mut().extend_pairs(q);
        let r = self.client.get(url).header("Client-Id", id).bearer_auth(token).send().await?;
        ok_bytes(r, &format!("Twitch {path}")).await
    }

    /// Топ эфиров по языку + клипы за сутки по самым популярным играм.
    pub async fn twitch(&self, id: &str, secret: &str, language: &str, games_top: usize) -> Result<Vec<Found>> {
        let token = self.twitch_token(id, secret).await?;
        let mut out = parse_twitch_streams(&self.twitch_get(id, &token, "streams", &[("first", "50"), ("language", language)]).await?)?;
        let games = parse_twitch_games(&self.twitch_get(id, &token, "games/top", &[("first", &games_top.to_string())]).await?)?;
        let since = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        for (gid, name) in games {
            let body = self.twitch_get(id, &token, "clips", &[("game_id", &gid), ("started_at", &since), ("first", "20")]).await?;
            out.extend(parse_twitch_clips(&body, &name)?.into_iter().filter(|f| language.is_empty() || f.obs.language.is_empty() || f.obs.language == language));
        }
        Ok(out)
    }

    pub async fn kick(&self, token: &str) -> Result<Vec<Found>> {
        let r = self
            .client
            .get(with_query(&format!("{}/livestreams", self.kick_base), &[("sort", "viewer_count"), ("limit", "50")])?)
            .bearer_auth(token)
            .send()
            .await?;
        parse_kick_livestreams(&ok_bytes(r, "Kick").await?)
    }

    pub async fn youtube(&self, key: &str, region: &str) -> Result<Vec<Found>> {
        let r = self
            .client
            .get(with_query(
                &format!("{}/videos", self.youtube_base),
                &[("part", "snippet,statistics,contentDetails"), ("chart", "mostPopular"), ("regionCode", region), ("maxResults", "50"), ("key", key)],
            )?)
            .send()
            .await?;
        parse_youtube_videos(&ok_bytes(r, "YouTube").await?)
    }

    pub async fn topics(&self, geo: &str, wiki_lang: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut err = None;
        match self.client.get(with_query(&self.trends_url, &[("geo", geo)])?).send().await {
            Ok(r) => match ok_bytes(r, "Google Trends").await {
                Ok(b) => out.extend(parse_trends_rss(&String::from_utf8_lossy(&b))),
                Err(e) => err = Some(e),
            },
            Err(e) => err = Some(e.into()),
        }
        let day = chrono::Utc::now() - chrono::Duration::days(2);
        let url = format!("{}/{wiki_lang}.wikipedia/all-access/{}", self.wiki_base, day.format("%Y/%m/%d"));
        match self.client.get(url).send().await {
            Ok(r) => match ok_bytes(r, "Wikipedia").await.and_then(|b| parse_wiki_top(&b)) {
                Ok(t) => out.extend(t),
                Err(e) => err = Some(e),
            },
            Err(e) => err = Some(e.into()),
        }
        if out.is_empty() {
            if let Some(e) = err {
                return Err(e);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAMS: &str = r#"{"data":[
      {"id":"1","user_login":"zxcursed","user_name":"ZXCursed","game_name":"Dota 2","type":"live","title":"ФИНАЛ турнира","viewer_count":4200,"language":"ru","started_at":"2026-11-19T10:00:00Z"},
      {"id":"2","user_login":"","user_name":"?","type":"live","viewer_count":5},
      {"id":"3","user_login":"rerun","user_name":"Rerun","game_name":"Chess","type":"","title":"повтор","viewer_count":10,"language":"ru"}
    ],"pagination":{}}"#;
    const CLIPS: &str = r#"{"data":[{"id":"AwkwardClip","url":"https://clips.twitch.tv/AwkwardClip","broadcaster_name":"Bro","title":"Момент!","view_count":812,"language":"ru","created_at":"2026-11-19T09:00:00Z","duration":28.5},{"id":"","url":""}]}"#;
    const KICK: &str = r#"{"data":[{"broadcaster_user_id":1,"channel_id":2,"slug":"trainwreckstv","stream_title":"Слоты","viewer_count":9100,"language":"ru","category":{"id":3,"name":"Slots"},"started_at":"2026-11-19T08:00:00Z"}],"message":"OK"}"#;
    const YT: &str = r#"{"items":[
      {"id":"abc123","snippet":{"title":"Обзор","channelTitle":"Канал","categoryId":"20","defaultAudioLanguage":"ru-RU","liveBroadcastContent":"none"},"statistics":{"viewCount":"1500000"},"contentDetails":{"duration":"PT12M30S"}},
      {"id":"live1","snippet":{"title":"Эфир","channelTitle":"Стрим","categoryId":"20","liveBroadcastContent":"live"},"liveStreamingDetails":{"concurrentViewers":"3300"},"contentDetails":{"duration":"P0D"}}
    ]}"#;

    #[test]
    fn twitch_streams_parse_and_skip_broken() {
        let f = parse_twitch_streams(STREAMS.as_bytes()).unwrap();
        // эфир без логина пропущен, повтор (type="") тоже не «live»
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key, "twitch:live:zxcursed");
        assert_eq!(f[0].obs.metric, 4200.0);
        assert_eq!(f[0].obs.url, "https://twitch.tv/zxcursed");
        assert_eq!(f[0].obs.category, "Dota 2");
    }

    #[test]
    fn twitch_clips_parse() {
        let f = parse_twitch_clips(CLIPS.as_bytes(), "Dota 2").unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, "clip");
        assert_eq!(f[0].obs.metric, 812.0);
        assert_eq!(f[0].obs.category, "Dota 2");
    }

    #[test]
    fn kick_parse() {
        let f = parse_kick_livestreams(KICK.as_bytes()).unwrap();
        assert_eq!(f[0].obs.url, "https://kick.com/trainwreckstv");
        assert_eq!(f[0].obs.category, "Slots");
        assert_eq!(f[0].obs.metric, 9100.0);
    }

    #[test]
    fn youtube_parse_vod_and_live() {
        let f = parse_youtube_videos(YT.as_bytes()).unwrap();
        assert_eq!(f[0].kind, "vod");
        assert_eq!(f[0].obs.metric, 1_500_000.0);
        assert_eq!(f[0].obs.language, "ru");
        assert!((f[0].obs.duration_min.unwrap() - 12.5).abs() < 1e-9);
        assert_eq!(f[1].kind, "live");
        assert_eq!(f[1].obs.metric, 3300.0);
        let err = parse_youtube_videos(br#"{"error":{"message":"quotaExceeded"}}"#).unwrap_err();
        assert!(err.to_string().contains("quotaExceeded"));
    }

    #[test]
    fn iso_durations() {
        assert_eq!(iso_duration_min("PT1H").unwrap(), 60.0);
        assert_eq!(iso_duration_min("PT90S").unwrap(), 1.5);
        assert_eq!(iso_duration_min("P1DT2M").unwrap(), 1442.0);
        assert!(iso_duration_min("12:30").is_none());
    }

    #[test]
    fn trends_and_wiki_topics() {
        let rss = "<rss><channel><title>Daily</title><item><title>Курс доллара &amp; нефть</title><ht:approx_traffic>200+</ht:approx_traffic></item><item><title><![CDATA[Финал Лиги]]></title></item></channel></rss>";
        assert_eq!(parse_trends_rss(rss), vec!["Курс доллара & нефть".to_string(), "Финал Лиги".to_string()]);
        let wiki = r#"{"items":[{"articles":[{"article":"Заглавная_страница","views":9},{"article":"Служебная:Поиск","views":8},{"article":"Чемпионат_мира","views":7}]}]}"#;
        assert_eq!(parse_wiki_top(wiki.as_bytes()).unwrap(), vec!["Чемпионат мира".to_string()]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_twitch_streams(b"<html>").is_err());
        assert!(parse_kick_livestreams(br#"{"message":"nope"}"#).is_err());
    }
}
