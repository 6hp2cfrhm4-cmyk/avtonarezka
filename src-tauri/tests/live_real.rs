//! Настоящий эфир Twitch (только руками): `AVTO_LIVE_URL=https://twitch.tv/имя AVTO_LIVE_SECS=480 cargo test -j2 --test live_real -- --ignored --nocapture`
//! Адрес потока даёт yt-dlp, чат читается анонимно. Печатает замеры и найденные моменты, в конце режет последние 60 с вручную.
use avtonarezka_lib::live::{chat, engine::{run, ChatTarget, Cmd, Engine, Event, Input}};
use std::{process::Command, time::Duration};
use tokio::sync::{mpsc, watch};

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn real_stream() {
    let Ok(url) = std::env::var("AVTO_LIVE_URL") else { return };
    let secs: u64 = std::env::var("AVTO_LIVE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(480);
    let out = Command::new("yt-dlp").args(["-g", "-f", "720p60/720p/best[height<=720]/best", "--no-playlist", "--no-cache-dir", "--no-update", &url]).output().unwrap();
    let input = String::from_utf8_lossy(&out.stdout).lines().find(|l| l.starts_with("http")).expect("эфир не идёт").to_string();
    let channel = chat::twitch_channel(&url).expect("ссылка Twitch");
    let dir = std::env::temp_dir().join(format!("live-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let e = Engine::new(Input::Url(input), dir.join("ring"), dir.join("clips"), Some(ChatTarget { host: "irc.chat.twitch.tv".into(), port: 6667, channel }));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctx, crx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(e, tx, crx, stop_rx));
    let t0 = std::time::Instant::now();
    let (mut n, mut max_z, mut max_msgs, mut cut_sent) = (0u32, 0f64, 0f64, false);
    let mut moments = vec![];
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Started => println!("запись началась"),
            Event::Restarted { reason } => println!("{reason}"),
            Event::Signal { z, msgs, authors, audio_db, .. } => {
                n += 1;
                max_z = max_z.max(z);
                max_msgs = max_msgs.max(msgs);
                if n % 30 == 0 { println!("[{:>3}с] z={z:.1} чат={msgs} авторов={authors} звук={audio_db:.0} дБ", t0.elapsed().as_secs()); }
                if t0.elapsed().as_secs() > secs && !cut_sent { cut_sent = true; ctx.send(Cmd::CutNow { back_s: 60.0 }).unwrap(); }
                if t0.elapsed().as_secs() > secs + 25 { let _ = stop_tx.send(true); }
            }
            Event::Moment { trigger, file, manual } => { println!("МОМЕНТ manual={manual} score={:.1} длина={:.0}с файл={}", trigger.score, trigger.end - trigger.start, file.display()); moments.push((file, manual)); }
            Event::CutFailed { error, .. } => println!("резка не удалась: {error}"),
            Event::Ended(w) => { println!("конец: {w}"); break; }
        }
        if t0.elapsed() > Duration::from_secs(secs + 120) { let _ = stop_tx.send(true); }
    }
    let _ = task.await;
    println!("замеров {n}, макс. z {max_z:.1}, макс. сообщений за интервал {max_msgs}");
    for (f, m) in &moments {
        let o = Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration,size:stream=codec_name,width,height", "-of", "default=nw=1"]).arg(f).output().unwrap();
        println!("{} manual={m}\n{}", f.display(), String::from_utf8_lossy(&o.stdout));
    }
    assert!(n > 20 && moments.iter().any(|m| m.1), "замеры шли и ручной клип вырезан");
}
