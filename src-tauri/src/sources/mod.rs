//! Источники фабрики. Старый youtube engine остаётся общим исполнителем yt-dlp.
pub mod chat;
pub mod kick;
use crate::youtube::engine::{self, DownloadError, DownloadRequest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MonitorOptions {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub min_duration: Option<f64>,
    pub max_duration: Option<f64>,
    pub title_regex: String,
    pub cookies_browser: Option<String>,
    pub two_pass: bool,
    pub download_chat: bool,
    pub viewer_clips: bool,
}
impl Default for MonitorOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 15,
            min_duration: None,
            max_duration: None,
            title_regex: String::new(),
            cookies_browser: None,
            two_pass: true,
            download_chat: false,
            viewer_clips: false,
        }
    }
}
impl MonitorOptions {
    pub fn validate(&self) -> Result<()> {
        if !(1..=1440).contains(&self.interval_minutes) {
            return Err("Интервал проверки: от 1 до 1440 минут".into());
        }
        for v in [self.min_duration, self.max_duration].into_iter().flatten() {
            if !v.is_finite() || v < 0.0 {
                return Err("Длительность источника должна быть неотрицательным числом".into());
            }
        }
        if self
            .min_duration
            .zip(self.max_duration)
            .is_some_and(|(a, b)| a > b)
        {
            return Err("Максимальная длительность меньше минимальной".into());
        }
        self.filter()?;
        if self.cookies_browser.as_deref().is_some_and(|b| {
            ![
                "chromium", "chrome", "firefox", "edge", "brave", "opera", "vivaldi", "safari",
            ]
            .contains(&b)
        }) {
            return Err("Выбери браузер для cookies из списка".into());
        }
        Ok(())
    }
    pub fn filter(&self) -> Result<Option<regex::Regex>> {
        if self.title_regex.len() > 2048 {
            return Err("Регулярное выражение длиннее 2048 символов".into());
        }
        if self.title_regex.trim().is_empty() {
            return Ok(None);
        }
        regex::RegexBuilder::new(&self.title_regex)
            .size_limit(1 << 20)
            .build()
            .map(Some)
            .map_err(|e| format!("Ошибка фильтра названия: {e}"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub kind: String,
    pub url: String,
    pub collection: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceItem {
    pub external_id: String,
    pub source: String,
    pub title: String,
    pub duration: Option<f64>,
    pub published_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
}
impl Window {
    pub fn valid(self) -> bool {
        self.start.is_finite() && self.end.is_finite() && self.start >= 0.0 && self.end > self.start
    }
}
#[derive(Clone, Copy, Debug)]
pub enum DownloadMode {
    Proxy,
    Full,
    Section(Window),
}
#[derive(Clone, Debug)]
pub struct Downloaded {
    pub path: PathBuf,
    pub offset: f64,
    pub fallback_full: bool,
}

#[allow(async_fn_in_trait)]
pub trait Source: Send + Sync {
    fn classify(&self, url: &str) -> Result<SourceRef>;
    async fn list_new(
        &self,
        channel: &SourceRef,
        since: Option<i64>,
        options: &MonitorOptions,
    ) -> Result<Vec<SourceItem>>;
    async fn download(
        &self,
        item: &SourceItem,
        mode: DownloadMode,
        options: &MonitorOptions,
        dir: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<Downloaded>;
}

/// URL-классификация не запускает программ и не обращается в сеть.
pub fn classify(kind: &str, text: &str) -> Result<SourceRef> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err("Укажи адрес источника".into());
    }
    if ["file", "folder"].contains(&kind) {
        let p = Path::new(raw);
        if !p.is_absolute() {
            return Err("Нужен абсолютный путь к файлу или папке".into());
        }
        return Ok(SourceRef {
            kind: kind.into(),
            url: raw.into(),
            collection: kind == "folder",
        });
    }
    let raw = if raw.contains("://") {
        raw.into()
    } else {
        format!("https://{raw}")
    };
    let mut u = reqwest::Url::parse(&raw).map_err(|_| "Не удалось разобрать ссылку")?;
    if !["http", "https"].contains(&u.scheme())
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
    {
        return Err("Нужна ссылка http/https без логина и порта".into());
    }
    let host = u.host_str().unwrap_or("").to_lowercase();
    u.set_fragment(None);
    let parts: Vec<_> = u
        .path()
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    let collection = match kind {
        "youtube" => {
            if ![
                "youtube.com",
                "www.youtube.com",
                "m.youtube.com",
                "music.youtube.com",
                "youtu.be",
            ]
            .contains(&host.as_str())
            {
                return Err("Нужна ссылка YouTube".into());
            }
            if let Ok(single) = crate::youtube::url::normalize_youtube_url(&raw) {
                return Ok(SourceRef {
                    kind: kind.into(),
                    url: single,
                    collection: false,
                });
            }
            let list = u
                .query_pairs()
                .find(|(k, _)| k == "list")
                .map(|(_, v)| v.into_owned());
            if parts.first().is_some_and(|p| p == "playlist")
                && list.as_ref().is_some_and(|v| {
                    !v.is_empty()
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                })
            {
                u.set_query(None);
                u.query_pairs_mut().append_pair("list", &list.unwrap());
            } else if parts
                .first()
                .is_some_and(|p| p.starts_with('@') && p.len() > 1)
                || (parts.len() >= 2 && ["channel", "c", "user"].contains(&parts[0].as_str()))
            {
                let base = if parts[0].starts_with('@') { 1 } else { 2 };
                if parts.len() > base + 1
                    || (parts.len() == base + 1
                        && !["videos", "streams"].contains(&parts[base].as_str()))
                {
                    return Err("Выбери вкладку videos или streams канала YouTube".into());
                }
                u.set_query(None);
                if parts.len() == base {
                    u.set_path(&format!("{}/videos", u.path().trim_end_matches('/')));
                }
            } else {
                return Err("Нужна ссылка на видео, канал, streams или плейлист YouTube".into());
            }
            true
        }
        "twitch" => {
            if !["twitch.tv", "www.twitch.tv", "m.twitch.tv"].contains(&host.as_str()) {
                return Err("Нужна ссылка Twitch".into());
            }
            u.set_query(None);
            if parts.len() == 2
                && parts[0] == "videos"
                && parts[1].bytes().all(|b| b.is_ascii_digit())
            {
                false
            } else if (parts.len() == 1 || (parts.len() == 2 && parts[1] == "videos"))
                && parts[0]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !["videos", "directory", "settings", "downloads"].contains(&parts[0].as_str())
            {
                u.set_path(&format!("/{}/videos", parts[0]));
                u.set_query(Some("filter=archives&sort=time"));
                true
            } else {
                return Err("Нужна запись Twitch /videos/ID или канал".into());
            }
        }
        "kick" => {
            if !["kick.com", "www.kick.com"].contains(&host.as_str()) {
                return Err("Нужна ссылка Kick".into());
            }
            u.set_query(None);
            if (parts.len() == 3 && parts[1] == "videos")
                || (parts.len() == 2 && parts[0] == "video")
            {
                false
            } else if parts.len() == 1 || (parts.len() == 2 && parts[1] == "videos") {
                u.set_path(&format!("/{}/videos", parts[0]));
                true
            } else {
                return Err("Нужна запись или канал Kick".into());
            }
        }
        _ => return Err("Неизвестный источник".into()),
    };
    Ok(SourceRef {
        kind: kind.into(),
        url: u.to_string(),
        collection,
    })
}
pub fn classify_link(url: &str) -> Result<SourceRef> {
    for kind in ["youtube", "twitch", "kick"] {
        if let Ok(r) = classify(kind, url) {
            return Ok(r);
        }
    }
    Err("Поддерживаются ссылки YouTube, Twitch и Kick".into())
}

pub struct YtDlpSource {
    pub kind: String,
    pub executable: PathBuf,
}
impl YtDlpSource {
    pub fn detected(kind: &str) -> Result<Self> {
        Ok(Self {
            kind: kind.into(),
            executable: engine::find_ytdlp().ok_or(engine::NOT_FOUND)?,
        })
    }
}
impl Source for YtDlpSource {
    fn classify(&self, url: &str) -> Result<SourceRef> {
        classify(&self.kind, url)
    }
    async fn list_new(
        &self,
        channel: &SourceRef,
        _since: Option<i64>,
        options: &MonitorOptions,
    ) -> Result<Vec<SourceItem>> {
        if channel.kind == "kick" && channel.collection {
            return kick::Archive::default().list(channel).await;
        }
        // since — подсказка. Не отсекаем по upload_date: завершённый стрим мог начаться до прошлого опроса.
        let mut cmd = crate::platform::command_at(&self.executable);
        cmd.args([
            "--ignore-config",
            "--dump-single-json",
            "--skip-download",
            "--no-warnings",
            "--no-colors",
            "--socket-timeout",
            "30",
            "--js-runtimes",
            "deno",
            "--js-runtimes",
            "node",
        ]);
        if channel.collection {
            cmd.args(["--flat-playlist", "--playlist-end", "200"]);
        } else {
            cmd.arg("--no-playlist");
        }
        if let Some(browser) = &options.cookies_browser {
            cmd.args(["--cookies-from-browser", browser]);
        }
        cmd.arg(&channel.url)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(120), cmd.output())
            .await
            .map_err(|_| "Источник не ответил за 2 минуты")?
            .map_err(|e| format!("yt-dlp: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("Ошибка yt-dlp при проверке источника")
                .into());
        }
        parse_listing(&self.kind, &out.stdout)
    }
    async fn download(
        &self,
        item: &SourceItem,
        mode: DownloadMode,
        options: &MonitorOptions,
        dir: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<Downloaded> {
        let target = self.classify(&item.source)?;
        if target.collection {
            return Err("Загрузка ожидает одну запись, а не канал".into());
        }
        let (req, args) = download_request(item, mode, options, dir)?;
        match engine::download_with_args(&self.executable, &req, args, cancel.clone(), |_| {}).await
        {
            Ok(path) => Ok(Downloaded {
                path,
                offset: match mode {
                    DownloadMode::Section(w) => w.start,
                    _ => 0.0,
                },
                fallback_full: false,
            }),
            Err(DownloadError::Cancelled) => Err("Загрузка отменена".into()),
            Err(DownloadError::Failed(e)) if matches!(mode, DownloadMode::Section(_)) => {
                // HLS/экстрактор может не уметь sections. Полный файл лежит отдельно от частичных секций.
                let (req, args) =
                    download_request(item, DownloadMode::Full, options, &dir.join("fallback"))?;
                let path = engine::download_with_args(&self.executable, &req, args, cancel, |_| {})
                    .await
                    .map_err(|err| format!("Секция: {e}; полный файл: {err:?}"))?;
                Ok(Downloaded {
                    path,
                    offset: 0.0,
                    fallback_full: true,
                })
            }
            Err(DownloadError::Failed(e)) => Err(e),
        }
    }
}

pub fn download_request(
    item: &SourceItem,
    mode: DownloadMode,
    options: &MonitorOptions,
    dir: &Path,
) -> Result<(DownloadRequest, Vec<String>)> {
    options.validate()?;
    let req = DownloadRequest {
        url: item.source.clone(),
        video_id: item.external_id.clone(),
        preset: if matches!(mode, DownloadMode::Proxy) {
            "360"
        } else {
            "1080"
        }
        .into(),
        out_dir: dir.into(),
        ffmpeg_dir: engine::find_ffmpeg_dir(),
    };
    // Общие progress/retry/JS/ffmpeg параметры. Формат прокси задаётся явно (старые пресеты UI не меняем).
    let mut base = req.clone();
    base.preset = "1080".into();
    let mut args = engine::download_args(&base)?;
    args.pop();
    args.insert(0, "--ignore-config".into());
    args.extend(["--socket-timeout".into(), "30".into()]);
    let f = args.iter().position(|a| a == "-f").unwrap() + 1;
    if matches!(mode, DownloadMode::Proxy) {
        args[f] = "bv*[height<=360]+ba/b[height<=360]/worst".into();
    }
    if let DownloadMode::Section(w) = mode {
        if !w.valid() {
            return Err("Неверные границы горячего окна".into());
        }
        args.extend([
            "--download-sections".into(),
            format!("*{:.3}-{:.3}", w.start, w.end),
            "--force-keyframes-at-cuts".into(),
        ]);
    }
    if let Some(browser) = &options.cookies_browser {
        args.extend(["--cookies-from-browser".into(), browser.clone()]);
    }
    args.push(item.source.clone());
    Ok((req, args))
}

pub fn parse_listing(kind: &str, bytes: &[u8]) -> Result<Vec<SourceItem>> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("Непонятный ответ источника: {e}"))?;
    let entries = value
        .get("entries")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_else(|| vec![value]);
    let mut items = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for e in entries {
        if e["is_live"].as_bool() == Some(true)
            || matches!(
                e["live_status"].as_str(),
                Some("is_live" | "is_upcoming" | "post_live")
            )
        {
            continue;
        }
        let Some(id) = e["id"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if !ids.insert(id.to_string()) {
            continue;
        }
        let raw = e["webpage_url"]
            .as_str()
            .or_else(|| e["url"].as_str())
            .filter(|s| s.starts_with("http"));
        let raw = raw.map(str::to_owned).or_else(|| match kind {
            "youtube" => Some(format!("https://www.youtube.com/watch?v={id}")),
            "twitch" => Some(format!(
                "https://www.twitch.tv/videos/{}",
                id.trim_start_matches('v')
            )),
            _ => None,
        });
        let Some(source) = raw
            .and_then(|r| classify(kind, &r).ok())
            .filter(|r| !r.collection)
        else {
            continue;
        };
        items.push(SourceItem {
            external_id: id.into(),
            source: source.url,
            title: e["title"].as_str().unwrap_or("Без названия").into(),
            duration: e["duration"].as_f64().filter(|d| d.is_finite() && *d > 0.0),
            published_at: e["release_timestamp"].as_i64().or(e["timestamp"].as_i64()),
        });
    }
    Ok(items)
}

pub struct FolderSource;
impl Source for FolderSource {
    fn classify(&self, url: &str) -> Result<SourceRef> {
        classify("folder", url)
    }
    async fn list_new(
        &self,
        channel: &SourceRef,
        _since: Option<i64>,
        _options: &MonitorOptions,
    ) -> Result<Vec<SourceItem>> {
        let mut paths = Vec::new();
        let p = Path::new(&channel.url);
        if channel.kind == "file" {
            paths.push(p.to_path_buf());
        } else {
            for e in std::fs::read_dir(p).map_err(|e| format!("Не открыть папку: {e}"))?
            {
                let e = e.map_err(|e| e.to_string())?;
                if e.file_type().is_ok_and(|t| t.is_file()) {
                    paths.push(e.path());
                }
            }
        }
        paths.sort();
        let mut items = Vec::new();
        for path in paths {
            if !path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                ["mp4", "mkv", "mov", "webm", "avi", "m4v", "ts"]
                    .contains(&e.to_ascii_lowercase().as_str())
            }) {
                continue;
            }
            let m = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !m.is_file() || m.len() == 0 {
                continue;
            }
            let modified = m.modified().map_err(|e| e.to_string())?;
            // Не подбираем файл, в который ещё пишет загрузчик. .part и символические ссылки исключены.
            if SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                < Duration::from_secs(30)
            {
                continue;
            }
            let path = path.canonicalize().map_err(|e| e.to_string())?;
            let stamp = format!(
                "{}:{}:{}",
                path.display(),
                m.len(),
                modified
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let info =
                match tokio::time::timeout(Duration::from_secs(20), crate::media::probe(&path))
                    .await
                {
                    Ok(Ok(info)) => info,
                    _ => continue, // Незавершённый контейнер попробуем при следующем опросе.
                };
            if !info.duration.is_finite() || info.duration <= 0.0 {
                continue;
            }
            let after = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if after.len() != m.len() || after.modified().ok() != Some(modified) {
                continue;
            }
            items.push(SourceItem {
                external_id: hex::encode(Sha256::digest(stamp.as_bytes())),
                source: path.to_string_lossy().into_owned(),
                title: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                duration: Some(info.duration),
                published_at: modified
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs() as i64),
            });
        }
        Ok(items)
    }
    async fn download(
        &self,
        item: &SourceItem,
        _mode: DownloadMode,
        _options: &MonitorOptions,
        _dir: &Path,
        _cancel: watch::Receiver<bool>,
    ) -> Result<Downloaded> {
        if !Path::new(&item.source).is_file() {
            return Err("Исходный файл не найден".into());
        }
        Ok(Downloaded {
            path: PathBuf::from(&item.source),
            offset: 0.0,
            fallback_full: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_sources_and_reject_foreign_hosts_live_paths() {
        for (kind, url, collection) in [
            ("youtube", "youtube.com/@test", true),
            ("youtube", "https://youtube.com/@test/streams", true),
            ("youtube", "youtube.com/playlist?list=PLabc", true),
            ("youtube", "youtu.be/dQw4w9WgXcQ", false),
            ("twitch", "twitch.tv/test", true),
            ("twitch", "twitch.tv/videos/123", false),
            ("kick", "kick.com/test/videos/123-abc", false),
            ("kick", "kick.com/test", true),
        ] {
            assert_eq!(classify(kind, url).unwrap().collection, collection, "{url}");
        }
        for (kind, url) in [
            ("youtube", "https://youtube.com.evil.org/@test"),
            ("twitch", "file:///etc/passwd"),
            ("kick", "https://kick.com/"),
            ("twitch", "https://twitch.tv/directory"),
            ("youtube", "https://youtube.com/@test/featured"),
        ] {
            assert!(classify(kind, url).is_err(), "{url}");
        }
    }
    #[test]
    fn listing_deduplicates_and_waits_until_live_is_finished() {
        let data=br#"{"entries":[{"id":"dQw4w9WgXcQ","title":"one","duration":120},{"id":"dQw4w9WgXcQ"},{"id":"abcdefghijk","is_live":true},{"id":"lmnopqrstuv","live_status":"post_live"},null]}"#;
        let items = parse_listing("youtube", data).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].duration, Some(120.0));
    }
    #[test]
    fn cookies_are_opt_in_and_sections_have_separate_cut_arguments() {
        let item = SourceItem {
            external_id: "a".into(),
            source: "https://twitch.tv/videos/123".into(),
            title: "x".into(),
            duration: None,
            published_at: None,
        };
        let o = MonitorOptions::default();
        let (_, args) =
            download_request(&item, DownloadMode::Proxy, &o, Path::new("/tmp/x")).unwrap();
        assert!(args.contains(&"--ignore-config".into()));
        assert!(!args.contains(&"--cookies-from-browser".into()));
        assert!(args.iter().any(|s| s.contains("height<=360")));
        let mut o = o;
        o.cookies_browser = Some("firefox".into());
        let (_, args) = download_request(
            &item,
            DownloadMode::Section(Window {
                start: 61.0,
                end: 92.5,
            }),
            &o,
            Path::new("/tmp/y"),
        )
        .unwrap();
        assert!(args.contains(&"*61.000-92.500".into()));
        assert!(args.contains(&"--force-keyframes-at-cuts".into()));
        assert!(args.contains(&"firefox".into()));
    }
}
