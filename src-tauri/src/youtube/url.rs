//! Проверка и приведение ссылок на YouTube. Порт `core/url.py`: принимается только ссылка на одно видео.

use reqwest::Url;

const HOSTS: [&str; 5] = ["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be"];
const PATH_PREFIXES: [&str; 4] = ["shorts", "live", "embed", "v"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("{0}")]
    Invalid(String),
    #[error("Плейлисты пока не поддерживаются — вставьте ссылку на одно видео.")]
    Playlist,
}

fn invalid(message: impl Into<String>) -> UrlError {
    UrlError::Invalid(message.into())
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 11-символьный id видео или понятная ошибка.
pub fn extract_video_id(text: &str) -> Result<String, UrlError> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err(invalid("Вставьте ссылку на YouTube-видео."));
    }
    let raw = if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") };

    let parsed = Url::parse(&raw).map_err(|_| invalid("Не YouTube-ссылка."))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(invalid("Поддерживаются только ссылки http/https."));
    }
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    if !HOSTS.contains(&host.as_str()) {
        let shown = if host.is_empty() { "неизвестный хост" } else { &host };
        return Err(invalid(format!("Не YouTube-ссылка: {shown}.")));
    }

    let query: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
    let param = |name: &str| query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

    let candidate: Option<String> = if host == "youtu.be" {
        parsed.path().trim_start_matches('/').split('/').next().map(str::to_owned)
    } else if parsed.path() == "/watch" {
        param("v").map(str::to_owned)
    } else {
        let parts: Vec<&str> = parsed.path().split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 && PATH_PREFIXES.contains(&parts[0]) {
            Some(parts[1].to_owned())
        } else if parsed.path().starts_with("/playlist") || param("list").is_some() {
            return Err(UrlError::Playlist);
        } else {
            None
        }
    };

    match candidate {
        Some(id) if is_video_id(&id) => Ok(id),
        _ => Err(invalid("Не удалось найти ID видео в ссылке.")),
    }
}

/// Каноничная ссылка `watch?v=<id>`. Всё, что не одно видео YouTube, отклоняется.
pub fn normalize_youtube_url(text: &str) -> Result<String, UrlError> {
    extract_video_id(text).map(|id| format!("https://www.youtube.com/watch?v={id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VID: &str = "dQw4w9WgXcQ";

    #[test]
    fn accepts_single_video_links() {
        for text in [
            format!("https://www.youtube.com/watch?v={VID}"),
            format!("https://youtube.com/watch?v={VID}&t=42s"),
            format!("https://m.youtube.com/watch?v={VID}"),
            format!("https://music.youtube.com/watch?v={VID}"),
            format!("https://youtu.be/{VID}"),
            format!("youtu.be/{VID}?si=abc"),
            format!("https://www.youtube.com/shorts/{VID}"),
            format!("https://www.youtube.com/live/{VID}"),
            format!("https://www.youtube.com/embed/{VID}"),
            format!("  https://www.youtube.com/watch?v={VID}  "),
            format!("www.youtube.com/watch?v={VID}"),
        ] {
            assert_eq!(extract_video_id(&text).as_deref(), Ok(VID), "{text}");
            assert_eq!(normalize_youtube_url(&text), Ok(format!("https://www.youtube.com/watch?v={VID}")), "{text}");
        }
    }

    #[test]
    fn rejects_other_input() {
        for text in [
            "".to_string(),
            "   ".to_string(),
            format!("https://example.com/watch?v={VID}"),
            format!("https://www.youtube.com.evil.net/watch?v={VID}"),
            format!("ftp://www.youtube.com/watch?v={VID}"),
            "file:///etc/passwd".to_string(),
            "https://www.youtube.com/watch?v=short".to_string(),
            "https://www.youtube.com/watch".to_string(),
            "https://www.youtube.com/".to_string(),
            "not a url at all".to_string(),
        ] {
            assert!(matches!(normalize_youtube_url(&text), Err(UrlError::Invalid(_))), "{text:?}");
        }
    }

    #[test]
    fn watch_link_with_list_param_is_still_a_single_video() {
        assert_eq!(
            normalize_youtube_url(&format!("https://www.youtube.com/watch?v={VID}&list=PLabc")),
            Ok(format!("https://www.youtube.com/watch?v={VID}"))
        );
    }

    #[test]
    fn playlists_are_reported_separately() {
        assert_eq!(normalize_youtube_url("https://www.youtube.com/playlist?list=PLabc123"), Err(UrlError::Playlist));
    }

    #[test]
    fn messages_are_in_russian() {
        let err = normalize_youtube_url("https://example.com/x").unwrap_err();
        assert_eq!(err.to_string(), "Не YouTube-ссылка: example.com.");
    }
}
