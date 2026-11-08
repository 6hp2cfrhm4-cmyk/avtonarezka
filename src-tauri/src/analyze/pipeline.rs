//! Весь анализ видео от файла до списка моментов. Каждый этап можно отключить или он
//! сам отключается, если нет модели: тогда конвейер идёт дальше и записывает это в `notes`.

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::bail;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::{
    audio,
    events::EventModel,
    llm::{self, LlmClient},
    score::{self, Params, Weights},
    speech::{self, Control},
    video, Candidate, Sentence, Signals, HOP,
};
use crate::{models::ModelPaths, render::Word};

#[derive(Clone, Debug)]
pub struct Options {
    pub threads: usize,
    pub language: String,
    pub use_events: bool,
    pub use_speech: bool,
    pub use_llm: bool,
    /// Сколько лучших кандидатов отдать на оценку LLM (и столько останется в итоге).
    pub llm_top: usize,
    pub llm_gpu_layers: u32,
    /// Доля LLM в итоговой оценке (остальное — сигналы).
    pub llm_weight: f64,
    pub params: Params,
    pub weights: Weights,
    /// Видео уже размечено: режем его целиком подряд (`score::tight_split`) вместо выбора лучших мест.
    pub tight: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()).min(12),
            language: "ru".into(),
            use_events: true,
            use_speech: true,
            use_llm: true,
            llm_top: 16,
            llm_gpu_layers: 99,
            llm_weight: 0.5,
            params: Params::default(),
            weights: Weights::default(),
            tight: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub stage: &'static str,
    /// Общий прогресс 0..1.
    pub fraction: f64,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Moment {
    pub start: f64,
    pub end: f64,
    pub score: f64,
    pub signal_score: f64,
    pub llm_score: f64,
    pub title: String,
    pub caption: String,
    pub hashtags: Vec<String>,
    /// Лучшая секунда для баннера, время исходника.
    pub climax: f64,
    pub transcript: String,
}

pub struct Analysis {
    pub signals: Signals,
    pub words: Vec<Word>,
    pub sentences: Vec<Sentence>,
    pub moments: Vec<Moment>,
    /// Что было пропущено или не удалось: показывается пользователю.
    pub notes: Vec<String>,
}

pub type Report = Arc<dyn Fn(Progress) + Send + Sync>;

/// Тяжёлая работа ML — на отдельном потоке ОС: она долгая и не должна занимать общий пул `spawn_blocking`,
/// которым пользуется остальной код (ввод-вывод, запросы к БД).
fn on_new_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> tokio::sync::oneshot::Receiver<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("ml-stage".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .expect("не удалось создать поток");
    rx
}

async fn ask_llm(c: &mut LlmClient, prompt: &str, clip_len: f64) -> anyhow::Result<llm::Verdict> {
    let text = c.complete(prompt, llm::GRAMMAR, llm::MAX_TOKENS).await?;
    llm::parse_verdict(&text, clip_len)
}

fn pad(mut v: Vec<f32>, n: usize) -> Vec<f32> {
    v.resize(n, 0.0);
    v
}

fn text_in(sentences: &[Sentence], start: f64, end: f64) -> String {
    sentences.iter().filter(|s| s.end > start && s.start < end).map(|s| s.text.trim()).collect::<Vec<_>>().join(" ")
}

fn first_words(text: &str, max: usize) -> String {
    let t: String = text.chars().take(max).collect();
    if text.chars().count() > max { format!("{}…", t.trim_end()) } else { t }
}

pub async fn run(
    path: &Path,
    duration: f64,
    has_audio: bool,
    models: &ModelPaths,
    opt: &Options,
    cancel: watch::Receiver<bool>,
    report: Report,
) -> anyhow::Result<Analysis> {
    let say = |stage: &'static str, fraction: f64, message: &str| report(Progress { stage, fraction, message: message.into() });
    let check = || if *cancel.borrow() { bail!("анализ отменён") } else { Ok(()) };
    let n_hops = (duration / HOP).ceil() as usize;
    let mut notes = Vec::new();

    // --- звук ---
    say("audio", 0.0, "Читаю звук");
    let pcm = if has_audio { audio::decode_pcm16k(path).await? } else { Vec::new() };
    let loud_db = if pcm.is_empty() { vec![-90.0; n_hops] } else { audio::rms_db(&pcm) };
    let quiet = audio::quiet_points(&loud_db, -45.0);
    let loudness = pad(audio::robust_z(&loud_db), n_hops);
    if !has_audio {
        notes.push("В видео нет звука: оценка идёт только по картинке".into());
    }
    check()?;

    // --- картинка: идёт параллельно со всем остальным ---
    let vpath = path.to_path_buf();
    let video_task = tokio::spawn(async move { video::analyze(&vpath, |_| {}).await });

    // --- звуковые события ---
    let pcm = Arc::new(pcm);
    let mut events = None;
    if opt.use_events && !pcm.is_empty() {
        if models.yamnet.is_file() && models.yamnet_classes.is_file() {
            say("events", 0.06, "Ищу смех, крики и аплодисменты");
            let (y, c, data, threads, rep) = (models.yamnet.clone(), models.yamnet_classes.clone(), pcm.clone(), opt.threads, report.clone());
            let res = on_new_thread(move || {
                let m = EventModel::load(&y, &c)?;
                m.scan(&data, threads, |f| rep(Progress { stage: "events", fraction: 0.06 + 0.09 * f, message: "Ищу смех, крики и аплодисменты".into() }))
            })
            .await?;
            match res {
                Ok(curve) => events = Some(pad(curve, n_hops)),
                Err(e) => notes.push(format!("События звука пропущены: {e:#}")),
            }
        } else {
            notes.push("Модель YAMNet не найдена: смех и аплодисменты не учитываются".into());
        }
    }
    check()?;

    // --- речь ---
    let mut words: Vec<Word> = Vec::new();
    let mut sentences: Vec<Sentence> = Vec::new();
    if opt.use_speech && !pcm.is_empty() {
        if models.whisper.is_file() {
            let data = Arc::try_unwrap(pcm.clone()).unwrap_or_else(|a| (*a).clone());
            let ctl = Control::default();
            let (model, lang, threads, c2) = (models.whisper.clone(), opt.language.clone(), opt.threads.saturating_sub(2).max(2) as i32, ctl.clone());
            let mut job = on_new_thread(move || speech::transcribe(&model, data, &lang, threads, c2));
            let res = loop {
                tokio::select! {
                    r = &mut job => break r?,
                    _ = tokio::time::sleep(Duration::from_millis(400)) => {
                        let p = ctl.progress.load(std::sync::atomic::Ordering::Relaxed).clamp(0, 100) as f64 / 100.0;
                        say("speech", 0.15 + 0.60 * p, &format!("Распознаю речь: {:.0}%", p * 100.0));
                        if *cancel.borrow() {
                            ctl.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            };
            match res {
                Ok(sp) => {
                    words = sp.words;
                    sentences = sp.sentences;
                }
                Err(e) if *cancel.borrow() => return Err(e),
                Err(e) => notes.push(format!("Речь не распознана: {e:#}")),
            }
        } else {
            notes.push("Модель Whisper не найдена: фразы не распознаны, границы по паузам".into());
        }
    }
    check()?;

    // --- картинка: дождёмся ---
    say("video", 0.76, "Считаю динамику кадра");
    let vs = video_task.await??;
    check()?;

    // --- кривая интереса и кандидаты ---
    say("score", 0.80, "Выбираю лучшие места");
    let speech_curve = (!words.is_empty()).then(|| pad(audio::robust_z(&speech::speech_rate(&words, n_hops)), n_hops));
    let signals = Signals {
        loudness,
        motion: pad(audio::robust_z(&vs.motion), n_hops),
        events,
        speech: speech_curve,
        cuts: vs.cuts,
    };
    let curve = score::interest_curve(&signals, &opt.weights);
    let mut params = opt.params.clone();
    params.top_k = if opt.use_llm { opt.llm_top.max(opt.params.top_k.min(40)) } else { opt.params.top_k };
    let cands: Vec<Candidate> = if opt.tight {
        if sentences.is_empty() {
            notes.push("Фраз нет: границы фрагментов ставятся по сменам сцены и тишине".into());
        }
        score::tight_split(duration, &sentences, &signals.cuts, &quiet, &curve, &params)
    } else if sentences.is_empty() {
        score::window_candidates(duration, &curve, &quiet, &params)
    } else {
        score::candidates(&sentences, &curve, &params)
    };

    let mut moments: Vec<Moment> = cands
        .iter()
        .map(|c| {
            let text = text_in(&sentences, c.start, c.end);
            Moment {
                start: c.start,
                end: c.end,
                score: c.score,
                signal_score: c.score,
                llm_score: 0.0,
                title: if text.is_empty() { format!("Момент {:.0}:{:02.0}", (c.start / 60.0).floor(), c.start % 60.0) } else { first_words(&text, 60) },
                caption: String::new(),
                hashtags: vec![],
                climax: c.climax,
                transcript: text,
            }
        })
        .collect();

    // --- LLM: заголовок, подпись, лучшая секунда и оценка ---
    if opt.use_llm && !sentences.is_empty() && !moments.is_empty() {
        match (models.llm.is_file(), llm::find_worker()) {
            (true, Some(_)) => {
                say("llm", 0.84, "Загружаю языковую модель");
                let mut gpu = opt.llm_gpu_layers;
                let mut client = match LlmClient::start(&models.llm, opt.threads, gpu).await {
                    Err(e) if gpu > 0 => {
                        notes.push(format!("Языковая модель не запустилась на видеокарте ({e:#}), работаю на процессоре"));
                        gpu = 0;
                        LlmClient::start(&models.llm, opt.threads, 0).await
                    }
                    r => r,
                };
                match client.as_mut() {
                    Err(e) => notes.push(format!("Языковая модель недоступна: {e:#}")),
                    // на процессоре ~18 с на фрагмент: всё видео так не оценить
                    Ok(_) if opt.tight && gpu == 0 => {
                        notes.push("Языковая модель работает только на процессоре: для нарезки всего видео это слишком долго, заголовки взяты из текста".into());
                        if let Ok(c) = client {
                            c.shutdown().await;
                        }
                    }
                    Ok(_) => {
                        // в режиме «вплотную» оцениваем каждый фрагмент, иначе только лучшие
                        let n = if opt.tight { moments.len() } else { moments.len().min(opt.llm_top) };
                        moments.truncate(n);
                        let mut failed = 0;
                        let mut client = client.ok();
                        for (i, m) in moments.iter_mut().enumerate() {
                            check()?;
                            let what = if opt.tight { "фрагмент" } else { "момент" };
                            say("llm", 0.86 + 0.13 * i as f64 / n as f64, &format!("Оцениваю {what} {} из {n}", i + 1));
                            let prompt = llm::build_prompt(&sentences, m.start, m.end);
                            let len = m.end - m.start;
                            let mut verdict = match client.as_mut() {
                                Some(c) => ask_llm(c, &prompt, len).await,
                                None => Err(anyhow::anyhow!("языковая модель остановлена")),
                            };
                            if verdict.is_err() && gpu > 0 {
                                // Видеокарта могла не потянуть запрос (обычно не хватает памяти): переходим на процессор.
                                if let Some(c) = client.take() {
                                    c.shutdown().await;
                                }
                                gpu = 0;
                                if opt.tight {
                                    notes.push("Видеокарта не справилась с запросом: на процессоре всё видео не оценить, остальные фрагменты идут без языковой модели".into());
                                    failed += n - i;
                                    break;
                                }
                                notes.push("Видеокарта не справилась с запросом, языковая модель продолжила на процессоре".into());
                                client = LlmClient::start(&models.llm, opt.threads, 0).await.ok();
                                if let Some(c) = client.as_mut() {
                                    verdict = ask_llm(c, &prompt, len).await;
                                }
                            }
                            match verdict {
                                Ok(v) => {
                                    m.llm_score = v.score();
                                    m.score = (1.0 - opt.llm_weight) * m.signal_score + opt.llm_weight * m.llm_score;
                                    m.title = v.title;
                                    m.caption = v.caption;
                                    m.hashtags = v.hashtags;
                                    let at = m.start + v.climax_t;
                                    if at >= m.start + 0.2 * len && at <= m.end - 0.15 * len {
                                        m.climax = at;
                                    }
                                }
                                Err(_) => failed += 1,
                            }
                        }
                        if let Some(c) = client {
                            c.shutdown().await;
                        }
                        if failed > 0 {
                            notes.push(format!("Языковая модель не справилась с {failed} из {n} моментов: у них только оценка по сигналам"));
                        }
                    }
                }
            }
            (false, _) => notes.push("Модель LLM не найдена: заголовки берутся из текста, подписей нет".into()),
            (_, None) => notes.push("Не собран llm-worker: заголовки берутся из текста, подписей нет".into()),
        }
    }

    if opt.tight {
        moments.sort_by(|a, b| a.start.total_cmp(&b.start));
    } else {
        moments.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    say("done", 1.0, "Готово");
    Ok(Analysis { signals, words, sentences, moments, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_fallback_is_cut_on_a_char_boundary() {
        assert_eq!(first_words("коротко", 60), "коротко");
        let t = first_words(&"ы".repeat(100), 60);
        assert_eq!(t.chars().count(), 61); // 60 символов и многоточие
        assert!(t.ends_with('…'));
    }

    #[test]
    fn text_in_picks_only_overlapping_sentences() {
        let s = vec![
            Sentence { start: 0.0, end: 5.0, text: "раз".into() },
            Sentence { start: 10.0, end: 15.0, text: " два ".into() },
            Sentence { start: 20.0, end: 25.0, text: "три".into() },
        ];
        assert_eq!(text_in(&s, 9.0, 21.0), "два три");
    }
}
