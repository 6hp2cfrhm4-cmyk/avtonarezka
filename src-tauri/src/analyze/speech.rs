//! Распознавание речи (whisper.cpp): слова с таймкодами → фразы → темп речи.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Arc,
    },
};

use anyhow::{bail, Context};
use whisper_rs::{DtwModelPreset, DtwMode, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{Sentence, HOP};
use crate::render::Word;

pub struct Speech {
    pub words: Vec<Word>,
    pub sentences: Vec<Sentence>,
}

/// Связь с вызывающим кодом: прогресс в процентах и флаг отмены.
#[derive(Clone, Default)]
pub struct Control {
    pub progress: Arc<AtomicI32>,
    pub cancel: Arc<AtomicBool>,
}

/// Блокирующий вызов (минуты на длинном видео) — запускать на отдельном потоке.
/// Идёт на видеокарте (Vulkan); если она отказала, один раз повторяет на процессоре.
pub fn transcribe(model: &Path, pcm: Vec<i16>, language: &str, threads: i32, ctl: Control) -> anyhow::Result<Speech> {
    if pcm.is_empty() {
        return Ok(Speech { words: vec![], sentences: vec![] });
    }
    let mut audio = vec![0.0f32; pcm.len()];
    whisper_rs::convert_integer_to_float_audio(&pcm, &mut audio).context("конвертация звука")?;
    drop(pcm);

    let gpu = if std::env::var_os("AVTO_NO_GPU").is_some() { None } else { super::gpu::choose() };
    match run(model, &audio, language, threads, &ctl, gpu) {
        Err(e) if gpu.is_some() && !ctl.cancel.load(Ordering::Relaxed) => {
            tracing::warn!("Whisper на видеокарте не удался ({e:#}), повторяю на процессоре");
            ctl.progress.store(0, Ordering::Relaxed);
            run(model, &audio, language, threads, &ctl, None)
        }
        r => r,
    }
}

fn run(model: &Path, audio: &[f32], language: &str, threads: i32, ctl: &Control, gpu: Option<i32>) -> anyhow::Result<Speech> {
    let mut cp = WhisperContextParameters::default();
    // DTW даёт заметно более точные границы слов, чем штатные временные метки токенов.
    cp.dtw_parameters.mode = DtwMode::ModelPreset { model_preset: DtwModelPreset::LargeV3Turbo };
    cp.use_gpu(gpu.is_some());
    if let Some(dev) = gpu {
        cp.gpu_device(dev);
    }
    let ctx = WhisperContext::new_with_params(model.to_str().context("путь к модели не в UTF-8")?, cp)
        .context("не удалось загрузить модель Whisper")?;
    let mut state = ctx.create_state().context("не удалось создать состояние Whisper")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(threads);
    params.set_language(Some(language));
    params.set_token_timestamps(true);
    params.set_suppress_blank(true);
    params.set_no_speech_thold(0.6);
    // Без контекста Whisper реже зацикливается на повторах в длинных записях.
    params.set_no_context(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    let progress = ctl.progress.clone();
    params.set_progress_callback_safe(move |p| progress.store(p, Ordering::Relaxed));

    // Не `set_abort_callback_safe`: в whisper-rs 0.16.0 его трамплин приводит указатель на
    // `Box<dyn FnMut>` к типу самого замыкания, флаг отмены читается из мусора и иногда
    // оказывается `true` — энкодер тогда возвращает «failed to encode» (код −6).
    unsafe extern "C" fn abort_flag(data: *mut std::ffi::c_void) -> bool {
        // SAFETY: `data` — указатель из `Arc::into_raw` ниже, он жив, пока идёт `full`
        unsafe { &*(data as *const AtomicBool) }.load(Ordering::Relaxed)
    }
    let flag = Arc::into_raw(ctl.cancel.clone());
    // SAFETY: функция и указатель выше соответствуют ожидаемому whisper.cpp типу колбэка
    unsafe {
        params.set_abort_callback(Some(abort_flag));
        params.set_abort_callback_user_data(flag as *mut std::ffi::c_void);
    }
    let result = state.full(params, audio);
    // SAFETY: парный вызов к `Arc::into_raw`; `full` уже вернулся и колбэк больше не вызывается
    drop(unsafe { Arc::from_raw(flag) });
    result.context("ошибка распознавания")?;
    if ctl.cancel.load(Ordering::Relaxed) {
        bail!("распознавание отменено");
    }

    let mut words: Vec<Word> = Vec::new();
    for seg in state.as_iter() {
        if seg.no_speech_probability() > 0.85 {
            continue;
        }
        let seg_end = seg.end_timestamp() as f64 / 100.0;
        let first = words.len();
        for k in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(k) else { continue };
            let Ok(text) = tok.to_str_lossy() else { continue };
            if text.starts_with("[_") || text.starts_with("<|") {
                continue; // служебные токены
            }
            let d = tok.token_data();
            let t = if d.t_dtw >= 0 { d.t_dtw } else { d.t0 } as f64 / 100.0;
            if text.trim().is_empty() {
                continue;
            }
            // Токен без пробела в начале продолжает слово (знаки препинания, окончания).
            let continues = words.len() > first && !text.starts_with(' ');
            if continues {
                words.last_mut().unwrap().text.push_str(&text);
            } else {
                words.push(Word { text: text.trim().to_string(), start: t, end: t });
            }
        }
        fix_word_ends(&mut words[first..], seg_end);
    }
    let sentences = sentences_from_words(&words);
    Ok(Speech { words, sentences })
}

/// Конец слова — начало следующего, но не дольше 1.2 с; последнее слово доходит до конца сегмента.
fn fix_word_ends(words: &mut [Word], seg_end: f64) {
    for i in 0..words.len() {
        let limit = words.get(i + 1).map_or(seg_end, |n| n.start);
        words[i].end = limit.min(words[i].start + 1.2).max(words[i].start + 0.12);
    }
}

/// Режет слова на фразы: по знакам конца предложения, по паузе больше 0.8 с и не длиннее 14 с.
pub fn sentences_from_words(words: &[Word]) -> Vec<Sentence> {
    let mut out = Vec::new();
    let mut from = 0;
    for i in 0..words.len() {
        let ends = words[i].text.ends_with(['.', '!', '?', '…']);
        let pause = words.get(i + 1).is_some_and(|n| n.start - words[i].end > 0.8);
        let too_long = words[i].end - words[from].start > 14.0;
        if ends || pause || too_long || i + 1 == words.len() {
            let text = words[from..=i].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            out.push(Sentence { start: words[from].start, end: words[i].end, text });
            from = i + 1;
        }
    }
    out
}

/// Слов в секунду на каждый отрезок `HOP`, сглаженно по ~2.5 с.
pub fn speech_rate(words: &[Word], n_hops: usize) -> Vec<f32> {
    let mut counts = vec![0.0f32; n_hops];
    for w in words {
        let i = (((w.start + w.end) / 2.0) / HOP) as usize;
        if i < n_hops {
            counts[i] += 1.0;
        }
    }
    (0..n_hops)
        .map(|i| {
            let (a, b) = (i.saturating_sub(2), (i + 3).min(n_hops));
            counts[a..b].iter().sum::<f32>() / ((b - a) as f64 * HOP) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end }
    }

    #[test]
    fn sentences_split_on_punctuation_and_pauses() {
        let words = vec![w("Привет.", 0.0, 0.5), w("Как", 0.6, 0.8), w("дела", 0.8, 1.1), w("слушай", 3.0, 3.4), w("дальше", 3.4, 3.9)];
        let s = sentences_from_words(&words);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].text, "Привет.");
        assert_eq!(s[1].text, "Как дела"); // пауза 1.9 с отделила следующую фразу
        assert_eq!((s[2].start, s[2].end), (3.0, 3.9));
    }

    #[test]
    fn endless_run_without_punctuation_is_cut() {
        let words: Vec<Word> = (0..60).map(|i| w("слово", i as f64 * 0.5, i as f64 * 0.5 + 0.4)).collect();
        let s = sentences_from_words(&words);
        assert!(s.len() >= 2);
        assert!(s.iter().all(|x| x.end - x.start <= 14.6), "{:?}", s.iter().map(|x| x.end - x.start).collect::<Vec<_>>());
    }

    #[test]
    fn word_ends_follow_next_word_but_are_capped() {
        let mut ws = vec![w("а", 1.0, 1.0), w("б", 1.4, 1.4), w("в", 5.0, 5.0)];
        fix_word_ends(&mut ws, 6.0);
        assert!((ws[0].end - 1.4).abs() < 1e-9);
        assert!((ws[1].end - 2.6).abs() < 1e-9); // следующее слово через 3.6 с, но не дольше 1.2 с
        assert!((ws[2].end - 6.0).abs() < 1e-9 || ws[2].end <= 6.2);
    }

    #[test]
    fn speech_rate_counts_words_per_second() {
        // 4 слова в секунду на отрезке 0..10 с
        let words: Vec<Word> = (0..40).map(|i| w("х", i as f64 * 0.25, i as f64 * 0.25 + 0.2)).collect();
        let r = speech_rate(&words, 20);
        assert_eq!(r.len(), 20);
        assert!((r[10] - 4.0).abs() < 0.5, "{}", r[10]);
    }
}
