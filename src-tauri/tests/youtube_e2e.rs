//! Скачивание с YouTube против поддельного `yt-dlp`: скрипт печатает те же строки, что настоящий, и создаёт файл.
//! Сети нет, YouTube не затрагивается. Последний тест (по умолчанию пропущен) гоняет настоящий yt-dlp.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use avtonarezka_lib::youtube::engine::{self, DownloadError, DownloadRequest, Stage, Update};
use tokio::sync::watch;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("avto-yt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("yt-dlp");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn request(out_dir: &Path, preset: &str) -> DownloadRequest {
    DownloadRequest {
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
        video_id: "dQw4w9WgXcQ".into(),
        preset: preset.into(),
        out_dir: out_dir.to_path_buf(),
        ffmpeg_dir: None,
    }
}

/// Находит значение флага (`-P <папка>`) среди аргументов, как это делает настоящий yt-dlp.
const FIND_DIR: &str = r#"dir=""; prev=""; for a in "$@"; do if [ "$prev" = "-P" ]; then dir="$a"; fi; prev="$a"; done"#;

const INFO_JSON: &str = r#"{"id":"dQw4w9WgXcQ","title":"Тестовый ролик","channel":"Канал","duration":213,
"thumbnail":"https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg",
"formats":[{"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","filesize":3000000},
{"format_id":"137","ext":"mp4","height":1080,"fps":25,"vcodec":"avc1.640028","acodec":"none","filesize":80000000}]}"#;

#[tokio::test]
async fn fake_ytdlp_scenarios() {
    let dir = scratch("fake");
    let out = dir.join("out");

    // --- сведения о ролике
    let ytdlp = fake(&dir, &format!("if [ \"$1\" = \"-J\" ]; then cat <<'EOF'\n{INFO_JSON}\nEOF\nexit 0; fi\nexit 9"));
    let info = engine::analyze(&ytdlp, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ").await.unwrap();
    assert_eq!((info.title.as_str(), info.max_height), ("Тестовый ролик", 1080));
    assert_eq!(info.presets.iter().find(|p| p.key == "1080").unwrap().approx_bytes, Some(83_000_000));

    // --- успех: видео и аудио отдельными потоками, склейка, перенос
    let ytdlp = fake(
        &dir,
        &format!(
            r#"{FIND_DIR}
echo "AVTO|D|downloading|1000|4000|NA|500|6|137|avc1.640028|none|1080|25|NA"
echo "AVTO|D|finished|4000|4000|NA|500|NA|137|avc1.640028|none|1080|25|NA"
echo "AVTO|D|downloading|100|400|NA|50|2|140|none|mp4a.40.2|NA|NA|129.5"
echo "AVTO|D|finished|400|400|NA|50|NA|140|none|mp4a.40.2|NA|NA|129.5"
echo "AVTO|P|started|Merger" >&2
echo "AVTO|P|finished|Merger" >&2
echo "AVTO|P|started|MoveFiles" >&2
f="$dir/Fake [dQw4w9WgXcQ].mp4"
printf video > "$f"
echo "AVTO|F|$f""#
        ),
    );
    let (_tx, rx) = watch::channel(false);
    let mut updates: Vec<Update> = Vec::new();
    let path = engine::download(&ytdlp, &request(&out, "1080"), rx, |u| updates.push(u)).await.unwrap();
    assert_eq!(path, out.join("Fake [dQw4w9WgXcQ].mp4"));
    assert!(path.is_file());
    let stages: Vec<Stage> = updates.iter().map(|u| u.stage).collect();
    assert_eq!(stages.first(), Some(&Stage::Downloading));
    let merge = stages.iter().position(|s| *s == Stage::Merging).expect("нет этапа склейки");
    let finish = stages.iter().position(|s| *s == Stage::Finishing).expect("нет этапа переноса");
    assert!(merge < finish && finish == stages.len() - 1);
    let last = &updates.last().unwrap().snapshot;
    assert_eq!((last.downloaded, last.total), (4400, Some(4400)));
    assert_eq!(last.streams.len(), 2);
    assert!(last.streams.iter().all(|s| s.finished));
    // доля только растёт
    let fractions: Vec<f64> = updates.iter().filter_map(|u| u.snapshot.fraction).collect();
    assert!(fractions.windows(2).all(|w| w[0] <= w[1]), "{fractions:?}");

    // --- yt-dlp не напечатал путь: файл находим по id в имени, а промежуточные куски склейки не берём
    let out2 = dir.join("out2");
    std::fs::create_dir_all(&out2).unwrap();
    std::fs::write(out2.join("Клип [dQw4w9WgXcQ].f137.mp4"), "v").unwrap();
    std::fs::write(out2.join("Клип [dQw4w9WgXcQ].mp4.part"), "p").unwrap();
    std::fs::write(out2.join("Клип [dQw4w9WgXcQ].mp4"), "done").unwrap();
    let ytdlp = fake(&dir, "exit 0");
    let (_tx, rx) = watch::channel(false);
    let found = engine::download(&ytdlp, &request(&out2, "720"), rx, |_| {}).await.unwrap();
    assert_eq!(found, out2.join("Клип [dQw4w9WgXcQ].mp4"));

    // --- ошибка yt-dlp: показываем его текст без префикса
    let ytdlp = fake(&dir, "echo 'ERROR: [youtube] dQw4w9WgXcQ: Video unavailable' >&2\nexit 1");
    let (_tx, rx) = watch::channel(false);
    let err = engine::download(&ytdlp, &request(&dir.join("out3"), "1080"), rx, |_| {}).await.unwrap_err();
    assert_eq!(err, DownloadError::Failed("[youtube] dQw4w9WgXcQ: Video unavailable".into()));
    let err = engine::analyze(&ytdlp, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ").await.unwrap_err();
    assert_eq!(err, "[youtube] dQw4w9WgXcQ: Video unavailable");

    // --- остановка посреди загрузки
    let ytdlp = fake(&dir, "echo \"AVTO|D|downloading|1000|4000|NA|500|6|137|avc1.640028|none|1080|25|NA\"\nexec sleep 30");
    let (tx, rx) = watch::channel(false);
    let started = std::time::Instant::now();
    let res = tokio::time::timeout(
        Duration::from_secs(10),
        engine::download(&ytdlp, &request(&dir.join("out4"), "1080"), rx, |_| {
            let _ = tx.send(true);
        }),
    )
    .await
    .expect("остановка не сработала");
    assert_eq!(res, Err(DownloadError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(5));

    // --- остановлено ещё до запуска
    let (tx, rx) = watch::channel(false);
    tx.send(true).unwrap();
    assert_eq!(engine::download(&ytdlp, &request(&dir.join("out5"), "1080"), rx, |_| {}).await, Err(DownloadError::Cancelled));

    // --- yt-dlp нет в системе
    let (_tx, rx) = watch::channel(false);
    let missing = dir.join("нет-такого");
    let err = engine::download(&missing, &request(&dir.join("out6"), "1080"), rx, |_| {}).await.unwrap_err();
    assert_eq!(err, DownloadError::Failed(engine::NOT_FOUND.into()));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Настоящий yt-dlp и настоящий YouTube: `cargo test --test youtube_e2e -- --ignored`.
#[tokio::test]
#[ignore = "нужны сеть, yt-dlp, Deno и ffmpeg"]
async fn live_download_720p_is_h264_mp4() {
    let ytdlp = engine::find_ytdlp().expect("yt-dlp не найден");
    let id = "dQw4w9WgXcQ";
    let url = format!("https://www.youtube.com/watch?v={id}");
    let info = engine::analyze(&ytdlp, &url, id).await.expect("analyze");
    assert!(info.max_height >= 720, "{info:?}");

    let out = scratch("live");
    let (_tx, rx) = watch::channel(false);
    let mut last_fraction = None;
    let path = engine::download(
        &ytdlp,
        &DownloadRequest { url, video_id: id.into(), preset: "720".into(), out_dir: out.clone(), ffmpeg_dir: engine::find_ffmpeg_dir() },
        rx,
        |u| last_fraction = u.snapshot.fraction.or(last_fraction),
    )
    .await
    .expect("download");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mp4"), "{path:?}");
    assert_eq!(last_fraction, Some(1.0));

    // повторный запуск того же ролика не качает заново и возвращает тот же файл
    let (_tx, rx) = watch::channel(false);
    let again = engine::download(
        &ytdlp,
        &DownloadRequest { url: format!("https://www.youtube.com/watch?v={id}"), video_id: id.into(), preset: "720".into(), out_dir: out.clone(), ffmpeg_dir: engine::find_ffmpeg_dir() },
        rx,
        |_| {},
    )
    .await
    .expect("повторный запуск");
    assert_eq!(again, path);
    let _ = std::fs::remove_dir_all(&out);
}
