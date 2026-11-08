//! Распознавание на настоящей модели Whisper. Пропускается, если модель не скачана.

use std::{path::PathBuf, process::Command};

use avtonarezka_lib::{
    analyze::{audio, speech},
    models::ModelPaths,
};

#[tokio::test(flavor = "multi_thread")]
async fn whisper_transcribes_russian_speech_with_ordered_word_times() {
    let Some(dir) = ModelPaths::default_dir() else { return };
    let model = ModelPaths::in_dir(&dir).whisper;
    if !model.is_file() {
        eprintln!("модель Whisper не скачана — тест пропущен");
        return;
    }
    if Command::new("espeak-ng").arg("--version").output().is_err() {
        eprintln!("нет espeak-ng — тест пропущен");
        return;
    }

    let wav: PathBuf = std::env::temp_dir().join(format!("avto-speech-{}.wav", std::process::id()));
    let text = "Сегодня мы расскажем о новой программе. Она автоматически нарезает длинные видео на короткие ролики. \
                Попробуйте её прямо сейчас, это действительно удобно.";
    let st = Command::new("espeak-ng").args(["-v", "ru", "-s", "150", "-w"]).arg(&wav).arg(text).status().unwrap();
    assert!(st.success());

    let pcm = audio::decode_pcm16k(&wav).await.unwrap();
    let secs = pcm.len() as f64 / 16_000.0;
    let started = std::time::Instant::now();
    let model2 = model.clone();
    let result = tokio::task::spawn_blocking(move || speech::transcribe(&model2, pcm, "ru", 8, Default::default()))
        .await
        .unwrap()
        .unwrap();
    eprintln!("распознано за {:.1} с ({secs:.1} с звука): {:?}", started.elapsed().as_secs_f64(), result.sentences.iter().map(|s| &s.text).collect::<Vec<_>>());

    let all = result.words.iter().map(|w| w.text.to_lowercase()).collect::<Vec<_>>().join(" ");
    for key in ["программе", "нарезает", "ролики"] {
        assert!(all.contains(key), "в тексте нет «{key}»: {all}");
    }
    // время идёт вперёд, не выходит за звук, слово не длиннее 1.2 с
    for w in result.words.windows(2) {
        assert!(w[0].start <= w[1].start + 1e-9, "{:?} → {:?}", w[0], w[1]);
    }
    assert!(result.words.iter().all(|w| w.start >= 0.0 && w.end <= secs + 0.5 && w.end - w.start <= 1.21 && w.end > w.start));
    assert!(result.sentences.len() >= 2, "{:?}", result.sentences);
}

/// Длинное аудио — несколько окон по 30 с, а не одно, как в первом тесте.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "медленный: около минуты на CPU"]
async fn whisper_handles_audio_longer_than_one_window() {
    let Some(dir) = ModelPaths::default_dir() else { return };
    let model = ModelPaths::in_dir(&dir).whisper;
    if !model.is_file() || Command::new("espeak-ng").arg("--version").output().is_err() {
        return;
    }
    let text = [
        "Сегодня мы поговорим о том, как правильно выбирать ноутбук для работы.",
        "Многие думают, что главное — это процессор, но это не совсем так.",
        "Оперативная память тоже очень важна, особенно если вы работаете с видео.",
        "Я лично проверял это на десяти разных моделях за последний год.",
        "И вот что оказалось: самое слабое место почти всегда охлаждение!",
        "Когда ноутбук перегревается, он сбрасывает частоту и теряет половину скорости.",
        "Поэтому перед покупкой обязательно смотрите обзоры с замерами температуры.",
        "Второй важный момент — это экран, ведь вы смотрите на него весь день.",
        "Яркость, цветопередача и частота обновления влияют на усталость глаз.",
        "Теперь давайте поговорим про автономность, потому что это болезненная тема.",
        "Производители пишут пятнадцать часов, а на деле выходит от силы пять.",
        "Я не шучу, мы сняли реальные замеры в обычной офисной работе.",
        "Третье, на что стоит обратить внимание, это клавиатура и тачпад.",
        "Если вы много печатаете, плохая клавиатура превратит работу в мучение.",
        "Хороший тачпад позволяет вообще не пользоваться мышью в дороге.",
        "И последнее, но очень важное: проверьте гарантию и сервисные центры.",
    ]
    .join(" ");
    let wav: PathBuf = std::env::temp_dir().join(format!("avto-speech-long-{}.wav", std::process::id()));
    assert!(Command::new("espeak-ng").args(["-v", "ru", "-s", "150", "-w"]).arg(&wav).arg(text).status().unwrap().success());
    let pcm = audio::decode_pcm16k(&wav).await.unwrap();
    eprintln!("аудио {:.1} с", pcm.len() as f64 / 16000.0);
    assert!(pcm.len() > 16000 * 60, "нужно больше двух окон");

    let started = std::time::Instant::now();
    let r = tokio::task::spawn_blocking(move || speech::transcribe(&model, pcm, "ru", 6, Default::default())).await.unwrap();
    eprintln!("за {:.1} с", started.elapsed().as_secs_f64());
    let r = r.expect("распознавание длинного аудио упало");
    assert!(r.words.len() > 40, "слов: {}", r.words.len());
    assert!(r.words.last().unwrap().end > 30.0, "распознана только первая часть: до {:.1} с", r.words.last().unwrap().end);
}
