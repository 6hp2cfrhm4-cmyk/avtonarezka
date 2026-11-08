//! Запуск yt-dlp: поиск программы, сведения о ролике и скачивание. Порт `analyze.py` и `downloader.py`.
//!
//! Настройка yt-dlp та же, что в утилите: один ролик без плейлиста, Deno или Node для задач YouTube, склейка потоков
//! ffmpeg'ом без перекодирования, докачка частично скачанного, повторы при сбоях сети.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::{mpsc, watch},
};

use super::{
    progress::{codec_label, parse_line, Line, Snapshot, Tracker},
    select::{self, Fmt, PRESETS},
    url::UrlError,
};

#[cfg(not(windows))]
pub const NOT_FOUND: &str = "yt-dlp не найден. Установи yt-dlp и Deno (Arch: sudo pacman -S yt-dlp deno).";
#[cfg(windows)]
pub const NOT_FOUND: &str = "yt-dlp не найден. Он должен лежать в папке tools рядом с программой: переустанови Автонарезку.";

/// Высота кадра в имени: без неё скачанное в 720p при выборе 1080p было бы принято за «уже скачано».
/// На Windows название короче: весь путь к файлу (с `.f399.mp4.part` и папками) обязан уложиться в 260 знаков.
#[cfg(not(windows))]
const OUTPUT_TEMPLATE: &str = "%(title).150B [%(id)s] [%(height)sp].%(ext)s";
#[cfg(windows)]
const OUTPUT_TEMPLATE: &str = "%(title).90B [%(id)s] [%(height)sp].%(ext)s";
/// Что печатает yt-dlp на каждый кусок скачанного; разбор — в `progress::parse_line`.
const DOWNLOAD_TEMPLATE: &str = "download:AVTO|D|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s|%(info.format_id)s|%(info.vcodec)s|%(info.acodec)s|%(info.height)s|%(info.fps)s|%(info.abr)s";
const POSTPROCESS_TEMPLATE: &str = "postprocess:AVTO|P|%(progress.status)s|%(progress.postprocessor)s";
const FILE_PRINT: &str = "after_move:AVTO|F|%(filepath)s";
const RETRIES: &str = "5";
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(60);
/// Не чаще раза в это время сообщаем о ходе одного ролика (в утилите то же значение).
const EMIT_EVERY: Duration = Duration::from_millis(150);
const TAIL_LINES: usize = 40;

// ---------------------------------------------------------------- поиск программ

/// yt-dlp запускается как есть. На Windows ему включаем UTF-8: иначе кириллица в названиях и путях
/// печаталась бы в кодовой странице системы (а недопустимые в ней знаки пропадали бы).
fn ytdlp_command(ytdlp: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = crate::platform::command_at(ytdlp);
    #[cfg(windows)]
    cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
    cmd
}

/// yt-dlp из PATH. В debug-сборке `AVTO_YTDLP` подменяет путь (для проверок на поддельной программе).
pub fn find_ytdlp() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        if let Some(p) = std::env::var_os("AVTO_YTDLP") {
            let p = PathBuf::from(p);
            return p.is_file().then_some(p);
        }
    }
    crate::platform::find_tool("yt-dlp")
}

/// Папка с ffmpeg для `--ffmpeg-location`.
pub fn find_ffmpeg_dir() -> Option<PathBuf> {
    crate::platform::find_tool("ffmpeg").and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Какой JS-рантайм есть для задач YouTube: сначала Deno, запасной — Node (как в утилите).
pub fn find_js_runtime() -> Option<&'static str> {
    ["deno", "node"].into_iter().find(|n| crate::platform::find_tool(n).is_some())
}

pub async fn tool_version(path: &Path) -> Option<String> {
    let run = ytdlp_command(path).arg("--version").stdin(Stdio::null()).kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_secs(10), run).await.ok()?.ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn spawn_error(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        NOT_FOUND.to_string()
    } else {
        format!("не удалось запустить yt-dlp: {e}")
    }
}

/// Понятный текст ошибки: последняя строка `ERROR: …` без префикса, иначе последняя непустая строка.
fn error_text<'a>(lines: impl Iterator<Item = &'a str>, code: Option<i32>) -> String {
    let lines: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(l) = lines.iter().rev().copied().find(|l| l.contains("ERROR:")) {
        let (_, msg) = l.split_once("ERROR:").unwrap_or(("", l));
        return msg.trim().to_string();
    }
    match (lines.last(), code) {
        (Some(l), _) => (*l).to_string(),
        (None, Some(c)) => format!("yt-dlp завершился с ошибкой (код {c})"),
        (None, None) => "yt-dlp был остановлен системой".to_string(),
    }
}

// ---------------------------------------------------------------- сведения о ролике

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    pub key: &'static str,
    pub label: &'static str,
    pub available: bool,
    pub approx_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub duration: Option<u64>,
    /// Адрес обложки у YouTube; наружу не отдаётся, команда сама скачивает её и подставляет `thumb`.
    #[serde(skip)]
    pub thumbnail_url: Option<String>,
    pub thumb: Option<String>,
    pub max_height: u32,
    pub best_fps: Option<f64>,
    pub best_codec: Option<String>,
    pub hdr: bool,
    pub presets: Vec<PresetInfo>,
}

#[derive(Deserialize)]
struct RawInfo {
    id: Option<String>,
    title: Option<String>,
    channel: Option<String>,
    uploader: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
    #[serde(rename = "_type")]
    kind: Option<String>,
    #[serde(default)]
    formats: Vec<RawFormat>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_id: Option<String>,
    ext: Option<String>,
    height: Option<f64>,
    fps: Option<f64>,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<f64>,
    filesize_approx: Option<f64>,
    dynamic_range: Option<String>,
}

fn to_fmt(f: &RawFormat) -> Fmt {
    Fmt {
        ext: f.ext.clone().unwrap_or_default(),
        height: f.height.filter(|h| *h > 0.0).map(|h| h as u32),
        fps: f.fps.filter(|v| *v > 0.0),
        vcodec: f.vcodec.clone(),
        acodec: f.acodec.clone(),
        size: f.filesize.or(f.filesize_approx).filter(|s| *s > 0.0).map(|s| s as u64),
        hdr: f.dynamic_range.as_deref().is_some_and(|d| !d.eq_ignore_ascii_case("sdr")),
    }
}

/// Разбирает ответ `yt-dlp -J`. Чистая функция: проверяется тестами без сети.
pub fn parse_info(json: &[u8], fallback_id: &str) -> Result<VideoInfo, String> {
    let raw: RawInfo = serde_json::from_slice(json).map_err(|e| format!("yt-dlp вернул непонятный ответ: {e}"))?;
    if matches!(raw.kind.as_deref(), Some("playlist" | "multi_video")) {
        return Err(UrlError::Playlist.to_string());
    }
    let formats: Vec<Fmt> = raw.formats.iter().filter(|f| f.format_id.is_some()).map(to_fmt).collect();
    let max_height = select::max_height(&formats);
    // при равенстве берём первый из списка, как `max` в утилите
    let best = formats
        .iter()
        .rev()
        .filter(|f| f.has_video() && f.height.is_some())
        .max_by(|a, b| a.height.cmp(&b.height).then(a.fps.partial_cmp(&b.fps).unwrap_or(std::cmp::Ordering::Equal)));

    let id = raw.id.filter(|i| !i.is_empty()).unwrap_or_else(|| fallback_id.to_string());
    let presets = PRESETS
        .iter()
        .map(|&(key, label)| {
            let available = select::preset_available(key, max_height);
            PresetInfo { key, label, available, approx_bytes: available.then(|| select::estimate_bytes(key, &formats)).flatten() }
        })
        .collect();

    Ok(VideoInfo {
        url: format!("https://www.youtube.com/watch?v={id}"),
        id,
        title: raw.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Без названия".into()),
        channel: raw.channel.or(raw.uploader).filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "—".into()),
        duration: raw.duration.filter(|d| *d > 0.0).map(|d| d as u64),
        thumbnail_url: raw.thumbnail,
        thumb: None,
        max_height,
        best_fps: best.and_then(|f| f.fps),
        best_codec: best.and_then(|f| f.vcodec.as_deref()).map(codec_label),
        hdr: best.is_some_and(|f| f.hdr),
        presets,
    })
}

/// Читает сведения о ролике, ничего не скачивая. `url` уже приведён к каноничному виду.
pub async fn analyze(ytdlp: &Path, url: &str, video_id: &str) -> Result<VideoInfo, String> {
    let child = ytdlp_command(ytdlp)
        .args(["-J", "--no-playlist", "--skip-download", "--no-warnings", "--no-colors"])
        .args(["--js-runtimes", "deno", "--js-runtimes", "node"])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(spawn_error)?;
    let out = tokio::time::timeout(ANALYZE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "yt-dlp слишком долго не отвечает. Проверь интернет и попробуй ещё раз.".to_string())?
        .map_err(|e| format!("yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(error_text(String::from_utf8_lossy(&out.stderr).lines(), out.status.code()));
    }
    parse_info(&out.stdout, video_id)
}

// ---------------------------------------------------------------- скачивание

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// Каноничная ссылка `watch?v=<id>`.
    pub url: String,
    pub video_id: String,
    pub preset: String,
    pub out_dir: PathBuf,
    pub ffmpeg_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Downloading,
    Merging,
    Finishing,
}

#[derive(Debug, Clone)]
pub struct Update {
    pub stage: Stage,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// Остановлено пользователем; частично скачанное остаётся для докачки.
    Cancelled,
    Failed(String),
}

/// Аргументы yt-dlp. Порт `build_options`: никакого перекодирования, только выбор потоков и склейка.
pub fn download_args(req: &DownloadRequest) -> Result<Vec<String>, String> {
    let format = select::build_format(&req.preset)?;
    let mut a: Vec<String> = [
        "--no-playlist", "--quiet", "--progress", "--newline", "--no-colors", "--no-warnings",
        "--windows-filenames", "--no-overwrites", "--continue",
        "--retries", RETRIES, "--fragment-retries", RETRIES,
        "--js-runtimes", "deno", "--js-runtimes", "node",
        // mp4, а если потоки в него не ложатся, yt-dlp сам берёт mkv
        "--merge-output-format", "mp4/mkv",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    if let Some(dir) = &req.ffmpeg_dir {
        a.extend(["--ffmpeg-location".to_string(), dir.display().to_string()]);
    }
    a.extend(["-f".to_string(), format]);
    a.extend(["-P".to_string(), req.out_dir.display().to_string()]);
    a.extend(["-o".to_string(), OUTPUT_TEMPLATE.to_string()]);
    a.extend(["--progress-template".to_string(), DOWNLOAD_TEMPLATE.to_string()]);
    a.extend(["--progress-template".to_string(), POSTPROCESS_TEMPLATE.to_string()]);
    a.extend(["--print".to_string(), FILE_PRINT.to_string()]);
    a.push(req.url.clone());
    Ok(a)
}

/// Строки процесса уходят в общий канал: прогресс yt-dlp печатает и в stdout, и в stderr.
fn pump<R: AsyncRead + Unpin + Send + 'static>(reader: R, tx: mpsc::UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// Если yt-dlp не напечатал путь (бывает, если файл уже был скачан), ищем файл по id в имени.
fn find_by_id(dir: &Path, video_id: &str) -> Option<PathBuf> {
    let needle = format!("[{video_id}]");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.contains(&needle) && !name.ends_with(".part") && !name.ends_with(".ytdl") && !is_stream_part(&name)
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Промежуточные файлы склейки: `имя [id].f136.mp4`.
fn is_stream_part(name: &str) -> bool {
    name.rsplit('.').nth(1).is_some_and(|stem_ext| {
        stem_ext.len() > 1 && stem_ext.starts_with('f') && stem_ext[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// Скачивает один ролик и возвращает путь к готовому файлу. `on_update` зовётся не чаще раза в 150 мс.
pub async fn download(
    ytdlp: &Path,
    req: &DownloadRequest,
    mut cancel: watch::Receiver<bool>,
    mut on_update: impl FnMut(Update),
) -> Result<PathBuf, DownloadError> {
    if *cancel.borrow() {
        return Err(DownloadError::Cancelled);
    }
    let args = download_args(req).map_err(DownloadError::Failed)?;
    std::fs::create_dir_all(&req.out_dir)
        .map_err(|e| DownloadError::Failed(format!("не удалось создать папку {}: {e}", req.out_dir.display())))?;

    let mut child = ytdlp_command(ytdlp)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| DownloadError::Failed(spawn_error(e)))?;
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        pump(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, tx.clone());
    }
    drop(tx);

    let mut tracker = Tracker::default();
    let mut snapshot = Snapshot::default();
    let mut stage = Stage::Downloading;
    let mut last_emit = Instant::now() - EMIT_EVERY;
    let mut file: Option<PathBuf> = None;
    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_LINES);
    let mut cancelled = false;
    let mut cancel_open = true;

    loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(line) = line else { break };
                match parse_line(&line) {
                    Line::Download(hook) => {
                        snapshot = tracker.update(&hook);
                        if hook.status == "finished" || last_emit.elapsed() >= EMIT_EVERY {
                            last_emit = Instant::now();
                            on_update(Update { stage, snapshot: snapshot.clone() });
                        }
                    }
                    Line::Post { status, name } => {
                        if status == "started" {
                            stage = if name == "MoveFiles" { Stage::Finishing } else { Stage::Merging };
                            on_update(Update { stage, snapshot: snapshot.clone() });
                        }
                    }
                    Line::File(path) => file = Some(path),
                    Line::Other(text) => {
                        if !text.trim().is_empty() {
                            if tail.len() == TAIL_LINES {
                                tail.pop_front();
                            }
                            tail.push_back(text);
                        }
                    }
                }
            }
            res = cancel.changed(), if cancel_open => {
                if res.is_err() {
                    // отправитель закрыт: отменять уже некому
                    cancel_open = false;
                } else if *cancel.borrow() {
                    cancelled = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| DownloadError::Failed(format!("yt-dlp: {e}")))?;
    if cancelled {
        return Err(DownloadError::Cancelled);
    }
    if !status.success() {
        return Err(DownloadError::Failed(error_text(tail.iter().map(String::as_str), status.code())));
    }
    file.filter(|p| p.is_file())
        .or_else(|| find_by_id(&req.out_dir, &req.video_id))
        .ok_or_else(|| DownloadError::Failed("yt-dlp закончил работу, но готового файла не видно.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RICK: &str = r#"{
      "id": "dQw4w9WgXcQ", "title": "Never Gonna Give You Up", "channel": "Rick Astley", "duration": 213,
      "thumbnail": "https://i.ytimg.com/vi_webp/dQw4w9WgXcQ/maxresdefault.webp",
      "formats": [
        {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","filesize":3449447},
        {"format_id":"136","ext":"mp4","height":720,"fps":25,"vcodec":"avc1.4D401F","acodec":"none","filesize":26455880,"dynamic_range":"SDR"},
        {"format_id":"137","ext":"mp4","height":1080,"fps":25,"vcodec":"avc1.640028","acodec":"none","filesize":80911999,"dynamic_range":"SDR"},
        {"format_id":"248","ext":"webm","height":1080,"fps":25,"vcodec":"vp9","acodec":"none","filesize_approx":30846567.0},
        {"format_id":"sb0","ext":"mhtml","vcodec":"none","acodec":"none"},
        {"format_id":null,"ext":"mp4","height":2160,"vcodec":"vp9","acodec":"none"}
      ]
    }"#;

    #[test]
    fn parses_info_and_presets() {
        let info = parse_info(RICK.as_bytes(), "fallback").unwrap();
        assert_eq!((info.id.as_str(), info.title.as_str(), info.channel.as_str()), ("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley"));
        assert_eq!(info.duration, Some(213));
        assert_eq!(info.max_height, 1080);
        assert_eq!((info.best_codec.as_deref(), info.best_fps), (Some("H.264"), Some(25.0)));
        assert!(!info.hdr);
        let by_key = |k: &str| info.presets.iter().find(|p| p.key == k).unwrap();
        assert!(by_key("720").available && by_key("1080").available && by_key("best").available);
        assert!(!by_key("1440").available && !by_key("2160").available);
        assert_eq!(by_key("1080").approx_bytes, Some(80_911_999 + 3_449_447));
        assert_eq!(by_key("2160").approx_bytes, None);
        assert_eq!(info.url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }

    #[test]
    fn missing_id_falls_back_and_playlist_is_rejected() {
        let info = parse_info(br#"{"title":"x"}"#, "abcdefghijk").unwrap();
        assert_eq!((info.id.as_str(), info.channel.as_str(), info.max_height), ("abcdefghijk", "—", 0));
        assert!(info.presets.iter().find(|p| p.key == "best").unwrap().available);
        assert!(parse_info(br#"{"_type":"playlist","entries":[]}"#, "x").unwrap_err().contains("Плейлисты"));
        assert!(parse_info(b"not json", "x").is_err());
    }

    #[test]
    fn args_follow_the_utility_options() {
        let req = DownloadRequest {
            url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            video_id: "dQw4w9WgXcQ".into(),
            preset: "1080".into(),
            out_dir: "/tmp/100%/Скачанное".into(),
            ffmpeg_dir: Some("/usr/bin".into()),
        };
        let a = download_args(&req).unwrap();
        let after = |flag: &str| a.iter().position(|x| x == flag).map(|i| a[i + 1].as_str());
        assert_eq!(after("--merge-output-format"), Some("mp4/mkv"));
        assert_eq!(after("--ffmpeg-location"), Some("/usr/bin"));
        assert_eq!(after("-P"), Some("/tmp/100%/Скачанное"));
        assert_eq!(after("-o"), Some(OUTPUT_TEMPLATE));
        assert!(after("-f").unwrap().contains("[vcodec^=avc1]"));
        assert!(a.contains(&"--no-playlist".to_string()) && a.contains(&"--continue".to_string()));
        assert_eq!(a.last().map(String::as_str), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        for forbidden in ["--recode-video", "--extract-audio", "--postprocessor-args", "--embed-subs"] {
            assert!(!a.contains(&forbidden.to_string()), "{forbidden}");
        }
        assert!(download_args(&DownloadRequest { preset: "bogus".into(), ..req }).is_err());
    }

    #[test]
    fn error_text_prefers_the_error_line() {
        let lines = ["WARNING: x", "ERROR: [youtube] abc: This video is unavailable", ""];
        assert_eq!(error_text(lines.into_iter(), Some(1)), "[youtube] abc: This video is unavailable");
        assert_eq!(error_text(["просто строка"].into_iter(), Some(1)), "просто строка");
        assert_eq!(error_text(std::iter::empty(), Some(2)), "yt-dlp завершился с ошибкой (код 2)");
    }

    #[test]
    fn stream_parts_are_not_taken_for_the_result() {
        assert!(is_stream_part("Видео [abc].f136.mp4"));
        assert!(is_stream_part("Видео [abc].f251-drc.webm"));
        assert!(!is_stream_part("Видео [abc].mp4"));
        assert!(!is_stream_part("Fast and Furious [abc].mp4"));
    }
}
