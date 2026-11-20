//! Получение replay-чата установленными утилитами. Ни установок, ни скрытого чтения cookies.
use super::{MonitorOptions, Result};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn find_twitch_downloader() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("AVTO_TWITCH_DOWNLOADER") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let name = if cfg!(windows) {
        "TwitchDownloaderCLI.exe"
    } else {
        "TwitchDownloaderCLI"
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(p.join("tools"));
    }
    dirs.into_iter().map(|p| p.join(name)).find(|p| p.is_file())
}
pub async fn version() -> Option<String> {
    let path = find_twitch_downloader()?;
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        crate::platform::command_at(path)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stdout
        .lines()
        .chain(stderr.lines())
        .find(|s| !s.trim().is_empty())?
        .trim();
    // CommandLineParser в официальном CLI1.56.5 возвращает1 на --version.
    // Разрешается только точный ответ с числовой версией, а не произвольная ошибка.
    let official = line
        .strip_prefix("TwitchDownloaderCLI ")
        .and_then(|v| v.split('+').next())
        .is_some_and(|v| {
            v.split('.').count() == 3
                && v.split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        });
    (out.status.success() || (out.status.code() == Some(1) && official))
        .then(|| line.chars().take(70).collect())
}
pub async fn download(
    kind: &str,
    url: &str,
    options: &MonitorOptions,
    dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let output = dir.join("chat.json");
    let mut cmd = match kind {
        "youtube" => {
            let tool =
                crate::youtube::engine::find_ytdlp().ok_or(crate::youtube::engine::NOT_FOUND)?;
            let mut cmd = crate::platform::command_at(tool);
            cmd.args([
                "--ignore-config",
                "--skip-download",
                "--write-subs",
                "--sub-langs",
                "live_chat",
                "--no-playlist",
                "--no-progress",
                "--output",
            ])
            .arg(dir.join("chat.%(ext)s"));
            if let Some(browser) = options.cookies_browser.as_deref() {
                cmd.args(["--cookies-from-browser", browser]);
            }
            cmd.arg("--").arg(url);
            cmd
        }
        "twitch" => {
            let tool=find_twitch_downloader().ok_or("TwitchDownloaderCLI не найден. Установка требует твоего согласия; можно импортировать JSON чата вручную")?;
            let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
            let id = parsed
                .path_segments()
                .and_then(|mut s| {
                    s.find(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
                })
                .ok_or("Не найден ID Twitch VOD")?;
            let mut cmd = crate::platform::command_at(tool);
            cmd.args([
                "chatdownload",
                "--id",
                id,
                "--collision",
                "Overwrite",
                "--threads",
                "2",
                "--temp-path",
            ])
            .arg(dir.join("cache"))
            .arg("--output")
            .arg(&output);
            cmd
        }
        "kick" => {
            return Err("Replay-чат Kick пока доступен импортом JSON с offset_seconds".into())
        }
        _ => return Err("Для локального видео импортируй JSON чата на экране анализа".into()),
    };
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = tokio::time::timeout(Duration::from_secs(900), cmd.status())
        .await
        .map_err(|_| "Получение чата заняло больше 15 минут".to_string())?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "Утилита чата завершилась с кодом {:?}; анализ продолжится без чата",
            status.code()
        ));
    }
    if kind == "youtube" {
        let p = dir.join("chat.live_chat.json");
        if p.is_file() {
            return Ok(p);
        }
        return Err("YouTube не вернул live_chat replay".into());
    }
    if !output.is_file() {
        return Err("Утилита не создала файл чата".into());
    }
    Ok(output)
}
