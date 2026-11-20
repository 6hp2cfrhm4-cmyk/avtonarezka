//! Поддельный эфир: `ffmpeg -re` читает файл с громким звуком в известные секунды, поддельный IRC-сервер
//! шлёт всплеск сообщений в те же секунды. Движок должен вырезать ровно один клип вокруг всплеска
//! (без перекодирования), по команде — ещё один вручную, и корректно закончить, когда «эфир» кончился.
use avtonarezka_lib::live::{
    detector::Params,
    engine::{now_s, run, ChatTarget, Cmd, Engine, Event, Input},
};
use std::{path::{Path, PathBuf}, process::{Command, Output}, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::TcpListener, sync::{mpsc, watch}};

const LEN: u32 = 40;
const BURST_FROM: f64 = 20.0;
const BURST_TO: f64 = 24.0;

fn encode_stream(path: &Path, audio_first: bool, data: Option<&Path>) -> Output {
    // звук тихий, кроме секунд всплеска; ключевой кадр каждую секунду, чтобы резать по сегментам
    let af = format!("volume='if(between(t,{BURST_FROM},{BURST_TO}),1.0,0.02)':eval=frame");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc=s=320x240:r=25:d={LEN}"), "-f", "lavfi", "-i", &format!("sine=f=440:r=44100:d={LEN}")]);
    if let Some(data) = data {
        cmd.args(["-f", "data", "-i"]).arg(data);
    }
    if audio_first {
        cmd.args(["-map", "1:a:0", "-map", "0:v:0"]);
    }
    if data.is_some() {
        cmd.args(["-map", "2:d:0", "-c:d", "copy"]);
    }
    cmd.args(["-af", &af, "-c:v", "libx264", "-threads:v", "2", "-preset", "ultrafast", "-g", "25", "-keyint_min", "25", "-sc_threshold", "0", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(path).output().expect("ffmpeg нужен для теста")
}

fn stream_types(path: &Path) -> Vec<String> {
    let out = Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=codec_type", "-of", "json"]).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe: {}", String::from_utf8_lossy(&out.stderr));
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    info["streams"].as_array().unwrap().iter().map(|stream| stream["codec_type"].as_str().unwrap().to_string()).collect()
}

fn make_stream(path: &Path, audio_first: bool) {
    if audio_first {
        let data = path.with_extension("bin");
        std::fs::write(&data, "метаданные эфира").unwrap();
        if encode_stream(path, true, Some(&data)).status.success() && stream_types(path) == ["audio", "video", "data"] {
            return;
        }
    }
    let out = encode_stream(path, audio_first, None);
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    if audio_first {
        assert_eq!(stream_types(path), ["audio", "video"]);
    }
}

/// IRC: после входа шлёт 1–2 сообщения в секунду, а в секунды всплеска — по 25 от разных авторов с подсказками.
async fn fake_irc() -> std::io::Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0").await?;
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = l.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        let mut joined = false;
        while let Ok(Some(line)) = lines.next_line().await {
            assert!(!line.contains("PASS"), "анонимный вход без пароля: {line}");
            if line.starts_with("JOIN #") {
                joined = true;
                break;
            }
        }
        assert!(joined);
        let _ = w.write_all(b"PING :tmi.twitch.tv\r\n").await;
        let t0 = std::time::Instant::now();
        let mut sec = 0u32;
        while t0.elapsed().as_secs() < LEN as u64 + 3 {
            let t = t0.elapsed().as_secs_f64();
            let burst = (BURST_FROM..BURST_TO).contains(&t);
            let n = if burst { 25 } else { 1 + (sec % 2) };
            for i in 0..n {
                let text = if burst { "KEKW это клип ахах" } else { "обычное сообщение" };
                let author = if burst { format!("viewer{i}") } else { format!("reg{}", sec % 3) };
                let emotes = if burst { "emotes=25:0-4" } else { "emotes=" };
                if w.write_all(format!("@{emotes} :{author}!{author}@{author}.tmi.twitch.tv PRIVMSG #fake :{text}\r\n").as_bytes()).await.is_err() {
                    return;
                }
            }
            sec += 1;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    Ok(port)
}

fn db_of(path: &Path, extra: &[&str]) -> (f64, f64) {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-nostats", "-i"]).arg(path).args(extra).args(["-vn", "-af", "volumedetect", "-f", "null", "-"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let get = |k: &str| text.lines().find_map(|l| l.split(k).nth(1).and_then(|r| r.trim().trim_end_matches("dB").trim().parse::<f64>().ok())).unwrap_or(-91.0);
    (get("mean_volume:"), get("max_volume:"))
}

fn duration(path: &Path) -> f64 {
    let out = Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(path).output().unwrap();
    assert!(out.status.success(), "ffprobe: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_stream_yields_one_clip_around_the_burst_and_a_manual_cut() {
    check_fake_stream(false, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_first_stream_yields_one_playable_video_clip() {
    check_fake_stream(true, false).await;
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("live-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn check_fake_stream(audio_first: bool, manual_cut: bool) {
    let dir = test_dir(if audio_first { "audio-first" } else { "manual" });
    let src = dir.join(if audio_first { "stream.ts" } else { "stream.mp4" });
    make_stream(&src, audio_first);
    let chat = match fake_irc().await {
        Ok(port) => Some(ChatTarget { host: "127.0.0.1".into(), port, channel: "fake".into() }),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => None,
        Err(err) => panic!("Не удалось запустить тестовый IRC: {err}"),
    };
    let use_chat = chat.is_some();

    let mut e = Engine::new(Input::File(src), dir.join("ring"), dir.join("clips"), chat);
    if !audio_first {
        e.seg_s = 1;
    }
    e.cut_wait_s = 15.0;
    e.params = Params { chat: use_chat, step_s: 1.0, window_s: 30.0, warmup_s: 8.0, pre_s: 6.0, post_s: 2.0, min_len_s: 5.0, max_len_s: 25.0, cooldown_s: 10.0, ..Params::default() };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctx, crx) = mpsc::unbounded_channel();
    let (_stop_tx, stop_rx) = watch::channel(false);
    let started = now_s();
    let task = tokio::spawn(run(e, tx, crx, stop_rx));

    let mut moments = vec![];
    let mut signals = 0;
    let mut ended = None;
    let mut manual_sent = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(LEN as u64 + 40);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        match ev {
            Event::Signal { t, .. } => {
                signals += 1;
                if manual_cut && !manual_sent && t - started > 33.0 {
                    manual_sent = true;
                    ctx.send(Cmd::CutNow { back_s: 6.0 }).unwrap();
                }
            }
            Event::Moment { trigger, file, manual } => moments.push((trigger, file, manual)),
            Event::CutFailed { error, .. } => panic!("резка не удалась: {error}"),
            Event::Restarted { reason } => panic!("неожиданный перезапуск записи: {reason}"),
            Event::Ended(why) => { ended = Some(why); break; }
            Event::Started => {}
        }
    }
    assert_eq!(ended.as_deref(), Some("эфир закончился"));
    task.await.unwrap().unwrap();
    assert!(signals > 20, "замеры идут каждую секунду: {signals}");

    let auto: Vec<_> = moments.iter().filter(|m| !m.2).collect();
    assert_eq!(auto.len(), 1, "ровно один найденный момент: {moments:?}");
    let (tr, file, _) = auto[0];
    // окно по часам: всплеск в секундах 20–24 от начала эфира (± секунды запуска)
    assert!(tr.start <= started + BURST_FROM + 2.0 && tr.start >= started + BURST_FROM - 12.0, "старт до пика: {} (эфир с {started})", tr.start - started);
    assert!(tr.end >= started + BURST_TO - 1.0, "конец не раньше конца пика: {}", tr.end - started);
    let d = duration(file);
    assert!(d > 3.0 && d <= 26.0, "длина клипа {d}");
    assert_eq!(stream_types(file), ["video", "audio"]);
    let (_, max) = db_of(file, &[]);
    assert!(max > -25.0, "в клипе слышен всплеск: max {max} дБ");
    let (head_mean, _) = db_of(file, &["-t", "1"]);
    assert!(head_mean < -40.0, "клип начинается до всплеска, в тишине: {head_mean} дБ");

    let manual: Vec<_> = moments.iter().filter(|m| m.2).collect();
    assert_eq!(manual.len(), usize::from(manual_cut));
    if manual_cut {
        let md = duration(&manual[0].1);
        assert!((3.0..=9.0).contains(&md), "ручной клип ≈ 6 с: {md}");
    }
    if audio_first {
        let text = std::fs::read_to_string(dir.join("ring/ring.m3u8")).unwrap();
        let segs = avtonarezka_lib::live::ring::parse_playlist(&text, &dir.join("ring"));
        assert!(segs.iter().filter(|seg| (seg.dur - 4.0).abs() < 0.05).count() >= 3, "кольцо содержит сегменты по 4 с");
        assert_eq!(stream_types(&segs[0].path), ["video", "audio"]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn write_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn stalled_recorder_restarts_three_times_then_ends() {
    let dir = test_dir("stall");
    let ffmpeg = dir.join("ffmpeg");
    write_executable(&ffmpeg, "#!/bin/sh\nexec sleep 3600\n");
    let old_segment = dir.join("seg_00000000.ts");
    let old_clip = dir.join("live_123_1.mp4");
    std::fs::write(&old_segment, []).unwrap();
    std::fs::write(&old_clip, []).unwrap();
    let mut e = Engine::new(Input::File(dir.join("stream.ts")), dir.clone(), dir.clone(), None);
    e.ffmpeg = ffmpeg.to_string_lossy().into_owned();
    e.stall_s = 0.15;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_cmd, cmd_rx) = mpsc::unbounded_channel();
    let (_stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(e, tx, cmd_rx, stop_rx));

    let restarts = tokio::time::timeout(Duration::from_secs(5), async {
        assert!(matches!(rx.recv().await, Some(Event::Started)));
        let mut restarts = 0;
        loop {
            match rx.recv().await.expect("движок завершился без события Ended") {
                Event::Restarted { reason } => {
                    assert!(reason.contains("Нет новых сегментов"));
                    restarts += 1;
                }
                Event::Ended(why) => {
                    assert_eq!(why, "запись остановилась: нет новых сегментов");
                    break restarts;
                }
                Event::Signal { .. } => {},
                other => panic!("неожиданное событие: {other:?}"),
            }
        }
    }).await.expect("запись не остановилась после зависания");
    task.await.unwrap().unwrap();
    assert_eq!(restarts, 3);
    assert!(!old_segment.exists());
    assert!(old_clip.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn invalid_cuts_emit_failures_and_are_deleted() {
    let dir = test_dir("invalid-cuts");
    let ffmpeg = dir.join("ffmpeg");
    write_executable(&ffmpeg, r#"#!/bin/sh
for last do :; done
case "$last" in
    *.m3u8) exec sleep 3600 ;;
    *_1.mp4) exec dd if=/dev/zero of="$last" bs=261 count=1 ;;
    *.mp4) exec dd if=/dev/zero of="$last" bs=1024 count=12 ;;
esac
"#);
    write_executable(&dir.join("ffprobe"), "#!/bin/sh\nprintf '0.5\\n'\n");
    let ring_dir = dir.join("ring");
    let clips_dir = dir.join("clips");
    let mut e = Engine::new(Input::File(dir.join("stream.ts")), ring_dir.clone(), clips_dir.clone(), None);
    e.ffmpeg = ffmpeg.to_string_lossy().into_owned();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(e, tx, cmd_rx, stop_rx));

    tokio::time::timeout(Duration::from_secs(5), async {
        assert!(matches!(rx.recv().await, Some(Event::Started)));
        let pdt = (chrono::Utc::now() - chrono::TimeDelta::seconds(10)).to_rfc3339();
        let playlist = format!("#EXTM3U\n#EXT-X-PROGRAM-DATE-TIME:{pdt}\n#EXTINF:20,\nseg_00000000.ts\n");
        std::fs::write(ring_dir.join("ring.m3u8"), playlist).unwrap();
        for expected_error in ["слишком маленький (261 байт)", "слишком короткий (0.50 с"] {
            cmd.send(Cmd::CutNow { back_s: 6.0 }).unwrap();
            loop {
                match rx.recv().await.expect("движок завершился без результата резки") {
                    Event::Signal { .. } => continue,
                    Event::CutFailed { error, .. } => {
                        assert!(error.contains(expected_error), "неожиданная ошибка: {error}");
                        break;
                    }
                    other => panic!("неожиданное событие: {other:?}"),
                }
            }
        }
        stop.send(true).unwrap();
        task.await.unwrap().unwrap();
    }).await.expect("движок не сообщил об ошибке резки");
    assert!(std::fs::read_dir(&clips_dir).unwrap().next().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}
