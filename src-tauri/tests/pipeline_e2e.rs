//! Весь анализ на настоящих моделях: речь → события → Whisper → кандидаты → LLM.
//! Пропускается, если чего-то нет (модели, воркер, espeak-ng).

use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};

use avtonarezka_lib::{
    analyze::{
        llm::find_worker,
        pipeline::{self, Options, Progress},
        score::Params,
    },
    models::ModelPaths,
};

const SENTENCES: &[&str] = &[
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
    "Ремонт ноутбука без гарантии может стоить как половина его цены.",
    "Подведём итог: смотрите на охлаждение, экран, автономность и клавиатуру.",
    "Если было полезно, подписывайтесь, дальше будет ещё много интересного.",
    "Спасибо, что дошли до конца, увидимся в следующем выпуске!",
];

fn run(cmd: &str, args: &[&str]) {
    let st = Command::new(cmd).args(args).status().unwrap_or_else(|e| panic!("{cmd}: {e}"));
    assert!(st.success(), "{cmd} {args:?}");
}

/// Русская речь ~90 с + громкий всплеск шума на 60-й секунде + видео.
fn make_video() -> (PathBuf, f64) {
    let tmp: PathBuf = std::env::temp_dir().join(format!("avto-pipe-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let (speech, video) = (tmp.join("speech.wav"), tmp.join("video.mp4"));
    run("espeak-ng", &["-v", "ru", "-s", "150", "-w", speech.to_str().unwrap(), &SENTENCES.join(" ")]);
    // громкий всплеск на 60-й секунде: розовый шум 1.5 с поверх речи
    run("ffmpeg", &[
        "-y", "-v", "error", "-i", speech.to_str().unwrap(),
        "-f", "lavfi", "-i", "anoisesrc=d=1.5:c=pink:a=0.9:r=22050",
        "-f", "lavfi", "-i", "testsrc2=s=640x360:r=25",
        "-filter_complex", "[1]adelay=60000|60000[b];[0][b]amix=inputs=2:duration=first:normalize=0,aresample=44100[a]",
        "-map", "2:v", "-map", "[a]", "-shortest", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac",
        video.to_str().unwrap(),
    ]);
    let out = Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(&video).output().unwrap();
    let duration: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    eprintln!("видео {duration:.0} с");
    assert!(duration > 80.0, "речь вышла короче 80 с: {duration}");
    (video, duration)
}

/// Заодно держит под присмотром ошибку, которую уже ловили: Whisper после YAMNet в одном потоке пула падал
/// с «failed to encode». Запуск: `cargo test --test pipeline_e2e -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "медленный: настоящие модели, около 2.5 минут"]
async fn full_pipeline_finds_the_loud_moment_and_names_it() {
    let Some(dir) = ModelPaths::default_dir() else { return };
    let models = ModelPaths::in_dir(&dir);
    let missing = models.missing();
    if !missing.is_empty() || find_worker().is_none() || Command::new("espeak-ng").arg("--version").output().is_err() {
        eprintln!("не хватает {missing:?}, llm-worker или espeak-ng — тест пропущен");
        return;
    }

    let (video, duration) = make_video();

    let opt = Options {
        threads: 8,
        llm_top: 3,
        params: Params { min_len: 20.0, max_len: 40.0, top_k: 3, ..Default::default() },
        ..Default::default()
    };
    let started = Instant::now();
    let log: Arc<Mutex<Vec<(f64, Progress)>>> = Arc::default();
    let sink = log.clone();
    let report: pipeline::Report = Arc::new(move |p| sink.lock().unwrap().push((started.elapsed().as_secs_f64(), p)));
    let (_tx, rx) = tokio::sync::watch::channel(false);

    let a = pipeline::run(&video, duration, true, &models, &opt, rx, report).await.expect("конвейер упал");
    eprintln!("всего {:.0} с", started.elapsed().as_secs_f64());

    // время начала каждого этапа
    let mut seen = Vec::new();
    for (t, p) in log.lock().unwrap().iter() {
        if !seen.contains(&p.stage) {
            seen.push(p.stage);
            eprintln!("  {:>6.1} с  {} ({:.0}%)", t, p.stage, p.fraction * 100.0);
        }
    }
    eprintln!("заметки: {:?}", a.notes);
    for m in &a.moments {
        eprintln!("  {:.1}–{:.1} счёт {:.2} (сигн. {:.2}, LLM {:.2}) баннер {:.1} «{}» {:?}", m.start, m.end, m.score, m.signal_score, m.llm_score, m.climax, m.title, m.hashtags);
    }

    assert!(a.notes.is_empty(), "что-то пропущено: {:?}", a.notes);
    assert!(a.words.len() > 100, "распознано слов: {}", a.words.len());
    assert!((1..=3).contains(&a.moments.len()), "моментов: {}", a.moments.len());
    for m in &a.moments {
        assert!(m.start >= 0.0 && m.end <= duration + 0.5 && m.end - m.start >= 19.0 && m.end - m.start <= 41.0, "{m:?}");
        assert!(m.climax > m.start && m.climax < m.end, "баннер вне момента: {m:?}");
        assert!(!m.title.is_empty() && m.llm_score > 0.0, "нет оценки LLM: {m:?}");
        assert!(!m.transcript.is_empty());
    }
    assert!(a.moments.windows(2).all(|w| w[0].score >= w[1].score), "не по убыванию оценки");
    // момент со всплеском (59.5–61.5 с) должен попасть в выдачу
    assert!(a.moments.iter().any(|m| m.start <= 60.0 && m.end >= 61.0), "момент со всплеском не найден");
    // кривая громкости видит всплеск
    let i = (60.5 / 0.5) as usize;
    assert!(a.signals.loudness[i] > 2.0, "всплеск не виден в громкости: {}", a.signals.loudness[i]);
}
