//! Пресеты качества и выражения формата для yt-dlp. Порт `core/selector.py` и `_apply_available_presets`.
//!
//! Выбор между кодеками, как и в утилите, отдан сортировке yt-dlp. Отличие одно: для 720p и 1080p просим H.264 + AAC
//! (если такие потоки есть), потому что окно предпросмотра, анализ и рендер заточены под них.

/// Самая старая версия yt-dlp, с которой работает утилита (`pyproject.toml`).
pub const MIN_YTDLP: &str = "2026.10.26";
pub const DEFAULT_PRESET: &str = "1080";

/// Ключ пресета, подпись. Ключ — предел по высоте кадра, кроме `best`.
pub const PRESETS: [(&str, &str); 5] =
    [("720", "720p"), ("1080", "1080p"), ("1440", "1440p"), ("2160", "4K"), ("best", "Лучшее")];

/// Предел по высоте для пресета; `None` у `best`.
pub fn height_limit(preset: &str) -> Result<Option<u32>, String> {
    match preset {
        "best" => Ok(None),
        p if PRESETS.iter().any(|(k, _)| *k == p) => Ok(p.parse().ok()),
        other => Err(format!("неизвестное качество: {other}")),
    }
}

/// Выражение `-f`. `bv*` — лучшее видео (отдельное или со звуком), `ba` — лучшее аудио, `/b` — запасной готовый файл.
pub fn build_format(preset: &str) -> Result<String, String> {
    Ok(match height_limit(preset)? {
        None => "bv*+ba/b".to_string(),
        Some(h) if h <= 1080 => {
            format!("bv*[height<={h}][vcodec^=avc1]+ba[ext=m4a]/bv*[height<={h}]+ba/b[height<={h}]")
        }
        Some(h) => format!("bv*[height<={h}]+ba/b[height<={h}]"),
    })
}

/// То, что нужно знать об одном потоке из списка форматов yt-dlp.
#[derive(Debug, Clone, Default)]
pub struct Fmt {
    pub ext: String,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub size: Option<u64>,
    pub hdr: bool,
}

impl Fmt {
    pub fn has_video(&self) -> bool {
        !matches!(self.vcodec.as_deref(), None | Some("none"))
    }
    pub fn has_audio(&self) -> bool {
        !matches!(self.acodec.as_deref(), None | Some("none"))
    }
    fn is_avc(&self) -> bool {
        self.vcodec.as_deref().is_some_and(|c| c.starts_with("avc1"))
    }
}

/// Самая большая высота кадра среди видеопотоков.
pub fn max_height(formats: &[Fmt]) -> u32 {
    formats.iter().filter(|f| f.has_video()).filter_map(|f| f.height).max().unwrap_or(0)
}

/// Пресет доступен, если у видео есть кадр не ниже. «Лучшее» доступно всегда.
pub fn preset_available(preset: &str, max_height: u32) -> bool {
    match height_limit(preset) {
        Ok(Some(limit)) => max_height >= limit,
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Примерный размер файла для пресета: самый большой известный размер подходящего видеопотока плюс аудио.
/// `None`, если YouTube не сообщил размер видео.
pub fn estimate_bytes(preset: &str, formats: &[Fmt]) -> Option<u64> {
    let limit = height_limit(preset).ok()?;
    let fits = |f: &&Fmt| f.has_video() && f.height.is_some() && limit.is_none_or(|l| f.height.unwrap_or(0) <= l);
    let all: Vec<&Fmt> = formats.iter().filter(fits).collect();

    let prefer_avc = limit.is_some_and(|l| l <= 1080);
    let avc: Vec<&Fmt> = all.iter().copied().filter(|f| f.is_avc()).collect();
    let pool = if prefer_avc && !avc.is_empty() { avc } else { all };

    let top = pool.iter().filter_map(|f| f.height).max()?;
    let video = pool.iter().filter(|f| f.height == Some(top)).filter_map(|f| f.size).max()?;
    let with_audio = pool.iter().any(|f| f.height == Some(top) && f.has_audio());

    let audio = if with_audio {
        0
    } else {
        let audio_only: Vec<&Fmt> = formats.iter().filter(|f| f.has_audio() && !f.has_video()).collect();
        let m4a: Vec<&Fmt> = audio_only.iter().copied().filter(|f| f.ext == "m4a").collect();
        let from = if prefer_avc && !m4a.is_empty() { m4a } else { audio_only };
        from.iter().filter_map(|f| f.size).max().unwrap_or(0)
    };
    Some(video + audio)
}

/// Сравнивает версии вида `2026.10.26` или `2026.10.26.232815` по числам.
pub fn version_at_least(version: &str, min: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.trim().split('.').map(|p| p.parse().ok()).collect() };
    match (parse(version), parse(min)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a >= b
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(h: u32, vcodec: &str, size: Option<u64>) -> Fmt {
        Fmt { ext: "mp4".into(), height: Some(h), fps: Some(25.0), vcodec: Some(vcodec.into()), acodec: Some("none".into()), size, hdr: false }
    }
    fn audio(ext: &str, size: u64) -> Fmt {
        Fmt { ext: ext.into(), height: None, fps: None, vcodec: Some("none".into()), acodec: Some("mp4a.40.2".into()), size: Some(size), hdr: false }
    }

    #[test]
    fn format_for_compat_presets_asks_for_h264_with_fallbacks() {
        assert_eq!(
            build_format("1080").unwrap(),
            "bv*[height<=1080][vcodec^=avc1]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]"
        );
        assert!(build_format("720").unwrap().starts_with("bv*[height<=720][vcodec^=avc1]+ba[ext=m4a]/"));
    }

    #[test]
    fn format_for_big_presets_matches_the_utility() {
        assert_eq!(build_format("1440").unwrap(), "bv*[height<=1440]+ba/b[height<=1440]");
        assert_eq!(build_format("2160").unwrap(), "bv*[height<=2160]+ba/b[height<=2160]");
        assert_eq!(build_format("best").unwrap(), "bv*+ba/b");
        assert!(build_format("999").is_err());
    }

    #[test]
    fn presets_are_available_up_to_the_max_height() {
        assert!(preset_available("720", 1080));
        assert!(preset_available("1080", 1080));
        assert!(!preset_available("1440", 1080));
        assert!(!preset_available("1080", 480));
        assert!(preset_available("best", 144));
        assert!(!preset_available("nope", 4320));
    }

    #[test]
    fn estimate_takes_h264_video_plus_m4a_audio() {
        let formats = vec![
            audio("m4a", 1_300_000),
            audio("m4a", 3_400_000),
            audio("webm", 3_500_000),
            video(1080, "vp9", Some(30_000_000)),
            video(1080, "avc1.640028", Some(80_000_000)),
            video(1080, "avc1.640028", None),
            video(2160, "vp9", Some(358_000_000)),
        ];
        assert_eq!(estimate_bytes("1080", &formats), Some(83_400_000));
        // для 4K кодек не выбираем: берём самый большой известный размер на этой высоте
        assert_eq!(estimate_bytes("2160", &formats), Some(358_000_000 + 3_500_000));
        assert_eq!(estimate_bytes("best", &formats), Some(358_000_000 + 3_500_000));
    }

    #[test]
    fn estimate_is_unknown_without_video_size() {
        let formats = vec![audio("m4a", 1_000_000), video(720, "avc1.4D401F", None)];
        assert_eq!(estimate_bytes("720", &formats), None);
        assert_eq!(estimate_bytes("1080", &formats), None);
    }

    #[test]
    fn estimate_does_not_add_audio_to_a_combined_stream() {
        let combined = Fmt { acodec: Some("mp4a.40.2".into()), ..video(360, "avc1.42001E", Some(11_800_000)) };
        let formats = vec![audio("m4a", 3_400_000), combined];
        assert_eq!(estimate_bytes("720", &formats), Some(11_800_000));
    }

    #[test]
    fn max_height_ignores_audio() {
        assert_eq!(max_height(&[audio("m4a", 1), video(720, "vp9", None), video(1080, "vp9", None)]), 1080);
        assert_eq!(max_height(&[audio("m4a", 1)]), 0);
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(version_at_least("2026.10.26", MIN_YTDLP));
        assert!(version_at_least("2026.11.01", MIN_YTDLP));
        assert!(version_at_least("2026.10.26.232815", MIN_YTDLP));
        assert!(!version_at_least("2026.08.19", MIN_YTDLP));
        assert!(!version_at_least("2025.12.31", MIN_YTDLP));
        assert!(!version_at_least("мусор", MIN_YTDLP));
    }
}
