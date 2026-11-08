//! YAMNet на настоящей модели. Пропускается, если модель не скачана.

use std::process::Command;

use avtonarezka_lib::{
    analyze::{audio, events::EventModel},
    models::ModelPaths,
};

fn model() -> Option<EventModel> {
    let p = ModelPaths::in_dir(&ModelPaths::default_dir()?);
    if !p.yamnet.is_file() {
        eprintln!("модель YAMNet не скачана — тест пропущен");
        return None;
    }
    Some(EventModel::load(&p.yamnet, &p.yamnet_classes).expect("модель не загрузилась"))
}

fn wav(name: &str, args: &[&str]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("avto-ev-{}-{name}.wav", std::process::id()));
    let st = Command::new("ffmpeg").args(["-y", "-v", "error"]).args(args).args(["-ac", "1", "-ar", "16000"]).arg(&p).status().unwrap();
    assert!(st.success());
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn curve_has_one_value_per_hop_and_speech_is_not_an_event() {
    let Some(m) = model() else { return };
    if Command::new("espeak-ng").arg("--version").output().is_err() {
        return;
    }
    let speech = std::env::temp_dir().join(format!("avto-ev-{}-speech.wav", std::process::id()));
    Command::new("espeak-ng").args(["-v", "ru", "-s", "150", "-w"]).arg(&speech).arg("Сегодня мы расскажем о новой программе, которая нарезает длинные видео на короткие ролики.").status().unwrap();
    let pcm = audio::decode_pcm16k(&speech).await.unwrap();

    let curve = m.scan(&pcm, 4, |_| {}).unwrap();
    assert_eq!(curve.len(), (pcm.len() as f64 / 16000.0 / 0.5).ceil() as usize);
    assert!(curve.iter().all(|v| (0.0..=4.0).contains(v)));
    let mean = curve.iter().sum::<f32>() / curve.len() as f32;
    assert!(mean < 1.0, "обычная речь не должна выглядеть событием: среднее {mean}");
}

#[tokio::test(flavor = "multi_thread")]
async fn long_audio_is_split_into_chunks_without_gaps() {
    let Some(m) = model() else { return };
    // 100 секунд шума — это больше трёх чанков по 31 с; кривая должна покрыть всё без дыр и сбоев
    let noise = wav("noise", &["-f", "lavfi", "-i", "anoisesrc=d=100:c=white:a=0.2:r=16000"]);
    let pcm = audio::decode_pcm16k(&noise).await.unwrap();
    let curve = m.scan(&pcm, 4, |_| {}).unwrap();
    assert_eq!(curve.len(), 200);
    assert!(curve.iter().all(|v| v.is_finite()));
    // все чанки одинаково шумные → кривая ровная, без провалов на стыках
    let (min, max) = curve.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!(max - min < 1.0, "кривая рвётся: {min}..{max}");
}
