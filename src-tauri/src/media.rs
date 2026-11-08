//! ffprobe и вспомогательные проверки окружения.

use std::path::Path;

use anyhow::{bail, Context};
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct VideoInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
    pub codec: String,
    pub size: u64,
}

pub async fn probe(path: &Path) -> anyhow::Result<VideoInfo> {
    let out = crate::platform::command("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .await
        .context("не удалось запустить ffprobe (установлен ли ffmpeg?)")?;
    if !out.status.success() {
        bail!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = v["streams"].as_array().context("ffprobe: нет потоков")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .context("в файле нет видеодорожки")?;

    let duration = v["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| video["duration"].as_str().and_then(|d| d.parse().ok()))
        .context("не удалось определить длительность")?;

    Ok(VideoInfo {
        duration,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps: parse_rate(video["avg_frame_rate"].as_str().unwrap_or("0/1")),
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
        codec: video["codec_name"].as_str().unwrap_or("?").to_string(),
        size: v["format"]["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

/// Длительность аудиофайла (музыки). Ошибка, если в файле нет звука.
pub async fn probe_audio(path: &Path) -> anyhow::Result<f64> {
    let out = crate::platform::command("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .await
        .context("не удалось запустить ffprobe (установлен ли ffmpeg?)")?;
    if !out.status.success() {
        bail!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    if !v["streams"].as_array().is_some_and(|s| s.iter().any(|x| x["codec_type"] == "audio")) {
        bail!("в файле нет звука");
    }
    v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).context("не удалось определить длительность")
}

/// Кадр-миниатюра шириной 360 px.
pub async fn make_thumb(src: &Path, dst: &Path, at: f64) -> anyhow::Result<()> {
    if let Some(dir) = dst.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let out = crate::platform::command("ffmpeg")
        .args(["-y", "-v", "error", "-ss", &format!("{at:.2}"), "-i"])
        .arg(src)
        .args(["-frames:v", "1", "-vf", "scale=360:-2", "-q:v", "4"])
        .arg(dst)
        .output()
        .await
        .context("не удалось запустить ffmpeg")?;
    if !out.status.success() || !dst.is_file() {
        bail!("ffmpeg не смог снять кадр: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Кадр черновика целиком (540 px по ширине) — афиша карточки.
pub async fn make_poster(src: &Path, dst: &Path, at: f64) -> anyhow::Result<()> {
    let out = crate::platform::command("ffmpeg")
        .args(["-y", "-v", "error", "-ss", &format!("{at:.2}"), "-i"])
        .arg(src)
        .args(["-frames:v", "1", "-q:v", "3"])
        .arg(dst)
        .output()
        .await
        .context("не удалось запустить ffmpeg")?;
    if !out.status.success() || !dst.is_file() {
        bail!("ffmpeg не смог снять кадр: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn parse_rate(r: &str) -> f64 {
    match r.split_once('/') {
        Some((n, d)) => {
            let (n, d): (f64, f64) = (n.parse().unwrap_or(0.0), d.parse().unwrap_or(1.0));
            if d == 0.0 { 0.0 } else { n / d }
        }
        None => r.parse().unwrap_or(0.0),
    }
}

/// Быстрый отпечаток файла: размер + первые и последние 4 МБ.
/// Гигабайтное видео целиком хешировать незачем.
pub async fn quick_hash(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    const CHUNK: u64 = 4 * 1024 * 1024;
    let mut f = tokio::fs::File::open(path).await?;
    let len = f.metadata().await?.len();
    let mut h = Sha256::new();
    h.update(len.to_le_bytes());

    let mut buf = vec![0u8; CHUNK.min(len) as usize];
    f.read_exact(&mut buf).await?;
    h.update(&buf);
    if len > CHUNK {
        let tail = CHUNK.min(len - CHUNK) as usize;
        f.seek(std::io::SeekFrom::Start(len - tail as u64)).await?;
        let mut buf = vec![0u8; tail];
        f.read_exact(&mut buf).await?;
        h.update(&buf);
    }
    Ok(hex::encode(&h.finalize()[..12]))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvReport {
    /// `linux` или `windows`: интерфейс по нему выбирает подсказки по установке утилит.
    pub os: &'static str,
    pub ffmpeg: Option<String>,
    pub nvenc: bool,
    pub libass: bool,
    /// Версия yt-dlp; `None`, если программы нет.
    pub ytdlp: Option<String>,
    /// yt-dlp не старше версии, с которой работает утилита-образец.
    pub ytdlp_ok: bool,
    /// `deno` или `node`: без них YouTube отдаёт неполный список форматов.
    pub js_runtime: Option<String>,
}

pub async fn check_env() -> EnvReport {
    let version = crate::platform::command("ffmpeg").arg("-version").output().await.ok();
    let ffmpeg = version
        .as_ref()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_owned));

    let has = |needle: &'static str, args: &'static [&'static str]| async move {
        crate::platform::command("ffmpeg")
            .args(["-hide_banner"])
            .args(args)
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(needle))
            .unwrap_or(false)
    };
    let ytdlp = match crate::youtube::engine::find_ytdlp() {
        Some(path) => crate::youtube::engine::tool_version(&path).await,
        None => None,
    };
    EnvReport {
        os: std::env::consts::OS,
        ffmpeg,
        nvenc: has("h264_nvenc", &["-encoders"]).await,
        libass: has(" ass ", &["-filters"]).await,
        ytdlp_ok: ytdlp.as_deref().is_some_and(|v| crate::youtube::select::version_at_least(v, crate::youtube::select::MIN_YTDLP)),
        ytdlp,
        js_runtime: crate::youtube::engine::find_js_runtime().map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_video(name: &str, secs: u32, audio: bool) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("avto-media-{}-{name}.mp4", std::process::id()));
        let mut args = vec!["-y".to_string(), "-v".into(), "error".into(), "-f".into(), "lavfi".into(), "-i".into(),
            format!("testsrc2=s=320x240:r=25:d={secs}")];
        if audio {
            args.extend(["-f", "lavfi", "-i", &format!("sine=d={secs}")].map(String::from));
        }
        args.extend(["-pix_fmt", "yuv420p", "-shortest"].map(String::from));
        args.push(path.to_string_lossy().into_owned());
        assert!(std::process::Command::new("ffmpeg").args(&args).status().unwrap().success());
        path
    }

    #[tokio::test]
    async fn probe_reads_size_fps_duration_and_audio() {
        let with = make_video("a", 3, true);
        let info = probe(&with).await.unwrap();
        assert_eq!((info.width, info.height), (320, 240));
        assert!((info.fps - 25.0).abs() < 0.01);
        assert!((info.duration - 3.0).abs() < 0.2);
        assert!(info.has_audio);

        let without = make_video("b", 2, false);
        assert!(!probe(&without).await.unwrap().has_audio);
    }

    #[tokio::test]
    async fn probe_rejects_non_video() {
        let txt = std::env::temp_dir().join(format!("avto-media-{}.txt", std::process::id()));
        std::fs::write(&txt, "не видео").unwrap();
        assert!(probe(&txt).await.is_err());
    }

    #[tokio::test]
    async fn quick_hash_is_stable_and_tells_files_apart() {
        let a = make_video("h1", 2, false);
        let b = make_video("h2", 3, false);
        assert_eq!(quick_hash(&a).await.unwrap(), quick_hash(&a).await.unwrap());
        assert_ne!(quick_hash(&a).await.unwrap(), quick_hash(&b).await.unwrap());
    }

    #[test]
    fn frame_rate_parsing() {
        assert!((parse_rate("30000/1001") - 29.97).abs() < 0.01);
        assert_eq!(parse_rate("25/1"), 25.0);
        assert_eq!(parse_rate("0/0"), 0.0);
        assert_eq!(parse_rate("30"), 30.0);
    }
}
