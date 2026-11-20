//! Движок одного эфира, не зависящий от окна и базы: запись в кольцо, чат, звук, детектор, резка.
//! Наружу — поток событий (`Event`), поэтому его целиком проверяет тест на поддельном эфире.
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    sync::{mpsc, watch},
    time::Instant,
};

use super::{
    chat::{self, Bucket, Line},
    detector::{self, Params, Sample, State, Trigger},
    ring,
};

#[derive(Clone, Debug)]
pub enum Input {
    /// Адрес HLS (его даёт yt-dlp).
    Url(String),
    /// Файл, читаемый в реальном времени (поддельный эфир для проверки).
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct ChatTarget {
    pub host: String,
    pub port: u16,
    pub channel: String,
}

#[derive(Clone, Debug)]
pub struct Engine {
    pub input: Input,
    pub dir: PathBuf,
    pub clips_dir: PathBuf,
    pub chat: Option<ChatTarget>,
    pub params: Params,
    pub ffmpeg: String,
    /// Длина сегмента кольца; в рабочем режиме 4 с, в тестах меньше.
    pub seg_s: u32,
    /// Сколько ждать, пока кольцо дорастёт до конца окна, прежде чем резать то, что есть.
    pub cut_wait_s: f64,
    /// Сколько ждать новых сегментов, прежде чем перезапустить запись.
    pub stall_s: f64,
}

impl Engine {
    pub fn new(input: Input, dir: PathBuf, clips_dir: PathBuf, chat: Option<ChatTarget>) -> Self {
        Self { input, dir, clips_dir, chat, params: Params::default(), ffmpeg: "ffmpeg".into(), seg_s: ring::SEG_SECONDS, cut_wait_s: 45.0, stall_s: 45.0 }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Started,
    Restarted { reason: String },
    /// Один замер: нужен ленте сигналов на экране.
    Signal { t: f64, z: f64, msgs: f64, authors: f64, audio_db: f64 },
    /// Момент найден; клип уже вырезан в файл.
    Moment { trigger: Trigger, file: PathBuf, manual: bool },
    /// Вырезать не удалось (окно вне кольца и т. п.).
    CutFailed { trigger: Trigger, error: String },
    Ended(String),
}

pub enum Cmd {
    /// «Вырезать сейчас»: последние `back_s` секунд.
    CutNow { back_s: f64 },
}

pub fn now_s() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn record_args_seg(e: &Engine) -> Vec<String> {
    let mut a = match &e.input {
        Input::Url(u) => ring::record_args(u, &e.dir, false),
        Input::File(p) => ring::record_args(&p.to_string_lossy(), &e.dir, true),
    };
    if let Some(i) = a.iter().position(|x| x == "-hls_time") {
        a[i + 1] = e.seg_s.to_string();
    }
    a
}

async fn spawn_recorder(e: &Engine) -> Result<Child> {
    tokio::fs::create_dir_all(&e.dir).await?;
    // старое кольцо от прошлого запуска не подмешиваем
    if let Ok(mut rd) = tokio::fs::read_dir(&e.dir).await {
        while let Ok(Some(f)) = rd.next_entry().await {
            let name = f.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), "ring.m3u8" | "ring.m3u8.tmp") || (name.starts_with("seg_") && (name.ends_with(".ts") || name.ends_with(".ts.tmp"))) {
                let _ = tokio::fs::remove_file(f.path()).await;
            }
        }
    }
    Command::new(&e.ffmpeg).args(record_args_seg(e)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn().context("Не удалось запустить ffmpeg для записи эфира")
}

/// Громкость одного сегмента, дБ (средняя). Ошибка или тишина без звука — очень тихо.
async fn segment_db(ffmpeg: &str, path: &Path) -> f64 {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await;
    let Ok(out) = out else { return -91.0 };
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.split("mean_volume:").nth(1).and_then(|r| r.trim().trim_end_matches("dB").trim().parse::<f64>().ok()))
        .unwrap_or(-91.0)
}

async fn chat_task(t: ChatTarget, bucket: Arc<Mutex<Bucket>>, mut stop: watch::Receiver<bool>) {
    let mut backoff = 2u64;
    while !*stop.borrow() {
        let res: Result<()> = async {
            let sock = TcpStream::connect((t.host.as_str(), t.port)).await?;
            let (r, mut w) = sock.into_split();
            for line in chat::handshake(&t.channel, 10_000 + (now_s() as u32 % 80_000)) {
                w.write_all(format!("{line}\r\n").as_bytes()).await?;
            }
            let mut lines = BufReader::new(r).lines();
            loop {
                tokio::select! {
                    _ = stop.changed() => return Ok(()),
                    l = lines.next_line() => match l? {
                        None => anyhow::bail!("чат закрыл соединение"),
                        Some(l) => match chat::parse_line(&l) {
                            Line::Ping(p) => w.write_all(format!("PONG :{p}\r\n").as_bytes()).await?,
                            Line::Message { author, text, emotes } => { bucket.lock().unwrap().add(&author, &text, emotes); backoff = 2; }
                            Line::Other => {}
                        },
                    }
                }
            }
        }
        .await;
        if res.is_ok() || *stop.borrow() {
            return;
        }
        tokio::select! { _ = stop.changed() => return, _ = tokio::time::sleep(Duration::from_secs(backoff)) => {} }
        backoff = (backoff * 2).min(60);
    }
}

async fn probe_duration(ffmpeg: &str, out: &Path) -> Result<Option<f64>> {
    let name = format!("ffprobe{}", std::env::consts::EXE_SUFFIX);
    let sibling = Path::new(ffmpeg).parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.join(&name));
    for program in sibling.into_iter().chain([PathBuf::from(name)]) {
        let probe = Command::new(program)
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(out)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await;
        let probe = match probe {
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            result => result.context("Не удалось запустить ffprobe для проверки клипа")?,
        };
        anyhow::ensure!(probe.status.success(), "ffprobe не смог прочитать клип: {}", String::from_utf8_lossy(&probe.stderr).trim());
        let duration = String::from_utf8_lossy(&probe.stdout).trim().parse::<f64>().context("ffprobe не определил длительность клипа")?;
        return Ok(Some(duration));
    }
    Ok(None)
}

async fn validate_clip(ffmpeg: &str, out: &Path, expected: f64) -> Result<()> {
    let meta = tokio::fs::metadata(out).await.context("Вырезанный клип не найден")?;
    anyhow::ensure!(meta.is_file() && meta.len() >= 10 * 1024, "Вырезанный клип слишком маленький ({} байт)", meta.len());
    if let Some(duration) = probe_duration(ffmpeg, out).await? {
        anyhow::ensure!(duration.is_finite() && duration >= expected / 2.0, "Вырезанный клип слишком короткий ({duration:.2} с, ожидалось {expected:.2} с)");
    }
    Ok(())
}

async fn cut(e: &Engine, tr: &Trigger, n: usize) -> Result<PathBuf> {
    let text = tokio::fs::read_to_string(e.dir.join("ring.m3u8")).await.context("Кольцо ещё не создано")?;
    let segs = ring::parse_playlist_with_segment_length(&text, &e.dir, e.seg_s);
    let (picked, offset) = ring::select(&segs, tr.start, tr.end)?;
    // длину берём из реально доступного: окно могло упереться в край кольца
    let from = picked[0].start + offset;
    let to = tr.end.min(picked[picked.len() - 1].end());
    let expected = (to - from).max(1.0);
    tokio::fs::create_dir_all(&e.clips_dir).await?;
    let stamp = (tr.peak_t as i64).rem_euclid(1_000_000_000);
    let out = e.clips_dir.join(format!("live_{stamp}_{n}.mp4"));
    let list = e.clips_dir.join(format!(".list_{stamp}_{n}.txt"));
    tokio::fs::write(&list, ring::concat_list(&picked)).await?;
    let st = Command::new(&e.ffmpeg).args(ring::cut_args(&list, offset, expected, &out)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).output().await;
    let _ = tokio::fs::remove_file(&list).await;
    let result = match st {
        Ok(st) if st.status.success() => validate_clip(&e.ffmpeg, &out, expected).await,
        Ok(st) => Err(anyhow::anyhow!("Резка не удалась: {}", String::from_utf8_lossy(&st.stderr).lines().last().unwrap_or("без сообщения"))),
        Err(err) => Err(err).context("ffmpeg не запустился"),
    };
    if let Err(err) = result {
        let _ = tokio::fs::remove_file(&out).await;
        return Err(err);
    }
    Ok(out)
}

async fn read_ring(e: &Engine) -> Vec<ring::Seg> {
    match tokio::fs::read_to_string(e.dir.join("ring.m3u8")).await {
        Ok(text) => ring::parse_playlist_with_segment_length(&text, &e.dir, e.seg_s),
        Err(_) => Vec::new(),
    }
}

const MAX_RESTARTS: u32 = 3;
const STALL_MESSAGE: &str = "запись остановилась: нет новых сегментов";

pub async fn run(e: Engine, tx: mpsc::UnboundedSender<Event>, mut cmds: mpsc::UnboundedReceiver<Cmd>, mut stop: watch::Receiver<bool>) -> Result<()> {
    let mut rec = spawn_recorder(&e).await?;
    let mut started_at = Instant::now();
    let mut last_progress_at = started_at;
    let mut latest_end = None;
    let mut restarts = 0;
    let bucket = Arc::new(Mutex::new(Bucket::default()));
    let chat = e.chat.clone().map(|t| tokio::spawn(chat_task(t, bucket.clone(), stop.clone())));
    let _ = tx.send(Event::Started);

    let mut st = State::default();
    let mut pending: Vec<(Trigger, f64, bool)> = Vec::new(); // (окно, когда возникло, вручную)
    let mut measured: Option<PathBuf> = None;
    let mut audio = -60.0;
    let mut n = 0usize;
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(e.params.step_s));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watchdog = tokio::time::interval(Duration::from_secs_f64((e.stall_s / 2.0).clamp(0.01, 1.0)));
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let ended: String;

    loop {
        let sample_due = tokio::select! {
            _ = stop.changed() => { ended = "остановлено".into(); break; }
            Some(Cmd::CutNow { back_s }) = cmds.recv() => {
                let t = now_s();
                pending.push((Trigger { start: t - back_s, end: t, peak_t: t - back_s / 2.0, score: 0.0 }, t, true));
                false
            }
            r = rec.wait() => {
                ended = match r { Ok(s) if s.success() => "эфир закончился".into(), Ok(s) => format!("запись остановилась ({s})"), Err(er) => er.to_string() };
                // что накопилось — дорезаем ниже
                break;
            }
            _ = ticker.tick() => true,
            _ = watchdog.tick() => false,
        };
        let segs = read_ring(&e).await;
        let end = segs.last().map(ring::Seg::end);
        let clock = Instant::now();
        if let Some(end) = end {
            if latest_end.is_none_or(|prev| end > prev) {
                last_progress_at = clock;
            }
            latest_end = Some(end);
        }
        let stalled = ring::is_stalled((clock - started_at).as_secs_f64(), (clock - last_progress_at).as_secs_f64(), e.stall_s);
        if stalled {
            let _ = rec.kill().await;
            if restarts == MAX_RESTARTS {
                ended = STALL_MESSAGE.into();
                break;
            }
        } else if sample_due {
            if let Some(last) = segs.last() {
                if measured.as_deref() != Some(last.path.as_path()) {
                    audio = segment_db(&e.ffmpeg, &last.path).await;
                    measured = Some(last.path.clone());
                }
            }
            let c = bucket.lock().unwrap().take();
            let t = now_s();
            let sample = Sample { t, msgs: c.msgs, authors: c.authors, emotes: c.emotes, audio_db: audio, hints: c.hints };
            let trig = detector::step(&mut st, &e.params, sample);
            let _ = tx.send(Event::Signal { t, z: st.last_z, msgs: c.msgs, authors: c.authors, audio_db: audio });
            if let Some(tr) = trig {
                pending.push((tr, t, false));
            }
        }
        // готовые к резке: кольцо дошло до конца окна или ждали слишком долго
        let now = now_s();
        let (ready, rest): (Vec<_>, Vec<_>) = pending.drain(..).partition(|(tr, born, _)| stalled || end.is_some_and(|x| x >= tr.end) || now - born > e.cut_wait_s);
        pending = rest;
        for (tr, _, manual) in ready {
            n += 1;
            match cut(&e, &tr, n).await {
                Ok(file) => { let _ = tx.send(Event::Moment { trigger: tr, file, manual }); }
                Err(er) => { let _ = tx.send(Event::CutFailed { trigger: tr, error: format!("{er:#}") }); }
            }
        }
        if stalled {
            rec = match spawn_recorder(&e).await {
                Ok(child) => child,
                Err(err) => {
                    ended = format!("Не удалось перезапустить запись: {err:#}");
                    break;
                }
            };
            restarts += 1;
            started_at = Instant::now();
            last_progress_at = started_at;
            latest_end = None;
            measured = None;
            audio = -60.0;
            let _ = tx.send(Event::Restarted { reason: format!("Нет новых сегментов: перезапуск записи ({restarts}/{MAX_RESTARTS})") });
        }
    }

    // конец: дорезаем то, что ждало, пока кольцо ещё на диске
    for (tr, _, manual) in std::mem::take(&mut pending) {
        n += 1;
        match cut(&e, &tr, n).await {
            Ok(file) => { let _ = tx.send(Event::Moment { trigger: tr, file, manual }); }
            Err(er) => { let _ = tx.send(Event::CutFailed { trigger: tr, error: format!("{er:#}") }); }
        }
    }
    let _ = rec.start_kill();
    let _ = rec.wait().await;
    if let Some(c) = chat {
        c.abort();
    }
    let _ = tx.send(Event::Ended(ended));
    Ok(())
}
