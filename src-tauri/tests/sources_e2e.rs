//! Обычные тесты используют локальные файлы/серверы. Сетевые проверки только #[ignore].
#![cfg(unix)]
use avtonarezka_lib::sources::{
    self, DownloadMode, FolderSource, MonitorOptions, Source, SourceItem, Window, YtDlpSource,
};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use tokio::sync::watch;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("avto-sources-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn synthetic(path: &Path) {
    let s = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x180:r=12",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=400:sample_rate=16000",
            "-t",
            "12",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(s.success());
}
fn duration(path: &Path) -> f64 {
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8(o.stdout).unwrap().trim().parse().unwrap()
}

#[tokio::test]
async fn flat_playlist_proxy_hot_section_and_hls_fallback_share_the_existing_engine() {
    let dir = scratch("ytdlp");
    let src = dir.join("fixture.mp4");
    synthetic(&src);
    let script = dir.join("fake-ytdlp");
    let body = format!(
        r#"#!/usr/bin/python3
import sys,json,pathlib,shutil,subprocess
root=pathlib.Path({root});source=root/'fixture.mp4';a=sys.argv[1:]
with (root/'args.jsonl').open('a') as f:f.write(json.dumps(a)+'\n')
if '--dump-single-json' in a:
 print(json.dumps({{'entries':[{{'id':'abcdefghijk','title':'Stream one','duration':120}},{{'id':'lmnopqrstuv','title':'Stream two','duration':20}},{{'id':'01234567890','title':'Stream three','duration':300}}]}}));sys.exit(0)
out=pathlib.Path(a[a.index('-P')+1]);out.mkdir(parents=True,exist_ok=True);target=out/'Fake [fixture].mp4'
if '--download-sections' in a:
 if (root/'fail-sections').exists():print('ERROR: HLS range unavailable',file=sys.stderr);sys.exit(1)
 start,end=map(float,a[a.index('--download-sections')+1].lstrip('*').split('-'))
 subprocess.run(['ffmpeg','-v','error','-y','-ss',str(start),'-i',str(source),'-t',str(end-start),'-c:v','libx264','-preset','ultrafast','-c:a','aac','-threads','1',str(target)],check=True)
else:shutil.copyfile(source,target)
print('AVTO|F|'+str(target))
"#,
        root = serde_json::to_string(&dir.to_string_lossy()).unwrap()
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let driver = YtDlpSource {
        kind: "youtube".into(),
        executable: script,
    };
    let channel = driver
        .classify("https://youtube.com/playlist?list=PLtest")
        .unwrap();
    assert!(channel.collection);
    let options = MonitorOptions::default();
    let items = driver.list_new(&channel, None, &options).await.unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        driver
            .list_new(&channel, Some(i64::MAX), &options)
            .await
            .unwrap()
            .len(),
        3,
        "since не теряет поздно обработанный live"
    );
    let (_tx, rx) = watch::channel(false);
    let proxy = driver
        .download(
            &items[0],
            DownloadMode::Proxy,
            &options,
            &dir.join("proxy"),
            rx.clone(),
        )
        .await
        .unwrap();
    assert!(proxy.path.is_file());
    assert!(!proxy.fallback_full);
    let window = Window {
        start: 3.0,
        end: 6.0,
    };
    let section = driver
        .download(
            &items[0],
            DownloadMode::Section(window),
            &options,
            &dir.join("section"),
            rx.clone(),
        )
        .await
        .unwrap();
    assert_eq!(section.offset, 3.0);
    assert!((duration(&section.path) - 3.0).abs() < 0.2);
    std::fs::write(dir.join("fail-sections"), b"1").unwrap();
    let full = driver
        .download(
            &items[0],
            DownloadMode::Section(window),
            &options,
            &dir.join("hls"),
            rx,
        )
        .await
        .unwrap();
    assert!(full.fallback_full);
    assert_eq!(full.offset, 0.0);
    assert!((duration(&full.path) - 12.0).abs() < 0.2);
    let log = std::fs::read_to_string(dir.join("args.jsonl")).unwrap();
    let calls = log
        .lines()
        .map(|l| serde_json::from_str::<Vec<String>>(l).unwrap())
        .collect::<Vec<_>>();
    assert!(calls
        .iter()
        .all(|a| a.contains(&"--ignore-config".into())
            && !a.contains(&"--cookies-from-browser".into())));
    assert!(calls[0].contains(&"--flat-playlist".into()));
    assert!(calls
        .iter()
        .any(|a| a.iter().any(|s| s.contains("height<=360"))));
    let (tx, rx) = watch::channel(false);
    tx.send(true).unwrap();
    assert!(driver
        .download(
            &items[0],
            DownloadMode::Section(window),
            &options,
            &dir.join("cancel"),
            rx
        )
        .await
        .unwrap_err()
        .contains("отменена"));
    assert!(
        !dir.join("cancel").exists(),
        "отмена не запускает полный fallback"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn folder_waits_for_complete_stable_files_and_tracks_replacements() {
    let dir = scratch("folder");
    let video = dir.join("готовая запись.mp4");
    synthetic(&video);
    std::fs::write(dir.join("loading.mp4.part"), b"partial").unwrap();
    std::fs::write(dir.join("empty.mp4"), b"").unwrap();
    let source = sources::classify("folder", dir.to_str().unwrap()).unwrap();
    let opts = MonitorOptions::default();
    assert!(FolderSource
        .list_new(&source, None, &opts)
        .await
        .unwrap()
        .is_empty());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&video)
        .unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
        .unwrap();
    let first = FolderSource.list_new(&source, None, &opts).await.unwrap();
    assert_eq!(first.len(), 1);
    assert!((first[0].duration.unwrap() - 12.0).abs() < 0.2);
    assert_eq!(
        FolderSource.list_new(&source, None, &opts).await.unwrap()[0].external_id,
        first[0].external_id
    );
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(90))
        .unwrap();
    assert_ne!(
        FolderSource.list_new(&source, None, &opts).await.unwrap()[0].external_id,
        first[0].external_id
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
#[ignore = "нужны разрешённая короткая VOD-ссылка AVTO_TEST_VOD, сеть и уже установленный yt-dlp"]
async fn live_short_vod_proxy_and_section() {
    let url = std::env::var("AVTO_TEST_VOD").expect("AVTO_TEST_VOD");
    let source = sources::classify_link(&url).unwrap();
    assert!(!source.collection);
    let driver = YtDlpSource::detected(&source.kind).unwrap();
    let opts = MonitorOptions::default();
    let items = driver.list_new(&source, None, &opts).await.unwrap();
    let item: &SourceItem = items.first().unwrap();
    assert!(item.duration.is_some_and(|s| s < 600.0));
    let dir = scratch("live");
    let (_tx, rx) = watch::channel(false);
    driver
        .download(
            item,
            DownloadMode::Proxy,
            &opts,
            &dir.join("proxy"),
            rx.clone(),
        )
        .await
        .unwrap();
    let section = driver
        .download(
            item,
            DownloadMode::Section(Window {
                start: 0.0,
                end: 5.0,
            }),
            &opts,
            &dir.join("section"),
            rx,
        )
        .await
        .unwrap();
    assert!(section.path.is_file());
}

#[tokio::test]
async fn kick_archive_http_lists_completed_vods_and_reports_blocked_access() {
    use axum::{routing::get, Json, Router};
    let app=Router::new().route("/channels/xqc/videos",get(||async{Json(serde_json::json!([
        {"is_live":false,"session_title":"Открытый VOD","duration":180000,"video":{"uuid":"100ca507-87a7-4745-b31a-5193e9feb4af","status":"public"}},
        {"is_live":true,"video":{"uuid":"100ca507-87a7-4745-b31a-5193e9feb4af"}}
    ]))})).route("/channels/blocked/videos",get(||async{axum::http::StatusCode::FORBIDDEN}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let archive = sources::kick::Archive {
        base,
        client: reqwest::Client::new(),
    };
    let source = sources::classify("kick", "https://kick.com/xqc/videos").unwrap();
    let list = archive.list(&source).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].duration, Some(180.0));
    let blocked = sources::classify("kick", "https://kick.com/blocked/videos").unwrap();
    assert!(archive
        .list(&blocked)
        .await
        .unwrap_err()
        .contains("HTTP 403"));
    server.abort();
}

#[tokio::test]
#[ignore = "Разрешённый публичный AVTO_TEST_VOD, установленный yt-dlp; скачивает только 10с, без полного fallback"]
async fn live_hls_vod_section_without_full_fallback() {
    let url = std::env::var("AVTO_TEST_VOD").expect("AVTO_TEST_VOD");
    let source = sources::classify_link(&url).unwrap();
    assert!(!source.collection);
    let driver = YtDlpSource::detected(&source.kind).unwrap();
    let opts = MonitorOptions::default();
    let list = driver.list_new(&source, None, &opts).await.unwrap();
    let item = list.first().expect("Публичный VOD");
    let dir = std::env::var_os("AVTO_TEST_MEDIA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| scratch("live-hls"));
    std::fs::create_dir_all(&dir).unwrap();
    let window = Window {
        start: 600.0,
        end: 610.0,
    };
    assert!(item.duration.is_some_and(|d| d > window.end));
    let (request, args) =
        sources::download_request(item, DownloadMode::Section(window), &opts, &dir).unwrap();
    let (_tx, rx) = watch::channel(false);
    let start = std::time::Instant::now();
    let path = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        avtonarezka_lib::youtube::engine::download_with_args(
            &driver.executable,
            &request,
            args,
            rx,
            |_| {},
        ),
    )
    .await
    .expect("Секция не скачалась за 3 минуты")
    .unwrap();
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type,width,height",
            "-of",
            "json",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let duration: f64 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (9.5..12.0).contains(&duration),
        "Длительность секции: {duration}"
    );
    let streams = info["streams"].as_array().unwrap();
    assert!(streams.iter().any(|s| s["codec_type"] == "audio"));
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    let report = serde_json::json!({"source":item.source,"externalId":item.external_id,"sourceDuration":item.duration,"path":path,"window":window,"actualDuration":duration,"width":video["width"],"height":video["height"],"elapsedSeconds":start.elapsed().as_secs_f64(),"fallbackFull":false});
    std::fs::write(
        dir.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("{report}");
}

#[tokio::test]
#[ignore = "Публичный архив Kick AVTO_TEST_VOD, только metadata; отдельных моделей/утилит не скачивает"]
async fn live_kick_archive_contains_selected_vod() {
    let source =
        sources::classify_link(&std::env::var("AVTO_TEST_VOD").expect("AVTO_TEST_VOD")).unwrap();
    assert_eq!(source.kind, "kick");
    let driver = YtDlpSource::detected("kick").unwrap();
    let opts = MonitorOptions::default();
    let single = driver.list_new(&source, None, &opts).await.unwrap();
    let url = reqwest::Url::parse(&source.url).unwrap();
    let name = url.path_segments().unwrap().next().unwrap();
    let archive = sources::classify("kick", &format!("https://kick.com/{name}/videos")).unwrap();
    let listed = driver.list_new(&archive, None, &opts).await.unwrap();
    assert!(listed
        .iter()
        .any(|v| v.external_id == single[0].external_id));
    eprintln!(
        "Kick archive {}: {} completed public videos; selected VOD found",
        name,
        listed.len()
    );
}
