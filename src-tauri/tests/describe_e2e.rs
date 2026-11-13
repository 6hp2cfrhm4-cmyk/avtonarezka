//! Описание ролика на настоящей модели через воркер: грамматика, секции, подгонка длины.
//! Долгий (минуты, в зависимости от видеокарты), поэтому по умолчанию пропущен:
//!   cargo build -p llm-worker && cargo test --test describe_e2e -- --ignored --nocapture
//! Видеокарта: слои задаёт `AVTO_GPU_LAYERS` (по умолчанию как в анализе).

use avtonarezka_lib::{
    analyze::{
        describe::{describe, parse_tags, similarity, tags_prompt, MAX_LEN, MAX_TAGS, MIN_LEN, TAGS_GRAMMAR},
        llm::{find_worker, LlmClient, Sampling},
        pipeline::Options,
    },
    models::ModelPaths,
    tiktok::api::{len_utf16, make_caption},
};

/// Расшифровка так, как её хранит база (`moments.transcript`): подряд идущие фразы речи без таймкодов.
const TRANSCRIPT: &str = "Вы не поверите, что случилось вчера на нашей кухне. Я поставил чайник и ушёл за телефоном. \
Возвращаюсь, а кот сидит на плите и смотрит на меня как ни в чём не бывало! Хорошо, что плита была выключена, иначе это был бы не смешной ролик. \
Я стою и не знаю, что делать: то ли смеяться, то ли снимать. Жена зашла на кухню и спросила, почему я молчу. \
Я просто показал пальцем на кота. Она посмотрела и сказала, что это третий раз за неделю. \
Оказывается, он любит греться на плите после того, как мы её выключаем. Теперь мы кладём туда подставку, чтобы ему было удобнее. \
Вот такая у нас теперь жизнь с этим котом.";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "долгий: нужна модель LLM и собранный llm-worker"]
async fn a_real_model_writes_descriptions_that_fit_the_window() {
    let Some(dir) = ModelPaths::default_dir() else { return };
    let model = ModelPaths::in_dir(&dir).llm;
    if !model.is_file() || find_worker().is_none() {
        eprintln!("нет модели LLM или llm-worker не собран — тест пропущен");
        return;
    }
    let opt = Options::default();
    let gpu: u32 = std::env::var("AVTO_GPU_LAYERS").ok().and_then(|v| v.parse().ok()).unwrap_or(opt.llm_gpu_layers);
    eprintln!("слоёв на видеокарте: {gpu}");
    let mut client = LlmClient::start(&model, opt.threads, gpu).await.expect("воркер не стартовал");

    let mut texts = Vec::new();
    for (i, seed) in [11u32].into_iter().enumerate() {
        let t = std::time::Instant::now();
        let d = describe(&mut client, "Кот на плите", TRANSCRIPT, seed, &mut |m| eprintln!("  {m}")).await.unwrap_or_else(|e| panic!("описание {i}: {e:#}"));
        let full = make_caption(&d.caption, &d.hashtags, usize::MAX);
        eprintln!("описание {i} за {:.0} с, {} знаков, хэштеги {:?}\n{}\n", t.elapsed().as_secs_f64(), d.len, d.hashtags, d.caption.chars().take(1800).collect::<String>());
        assert_eq!(len_utf16(&full), d.len);
        assert!((MIN_LEN..=MAX_LEN).contains(&d.len), "длина {}", d.len);
        assert!(!d.hashtags.is_empty() && d.hashtags.len() <= MAX_TAGS, "{:?}", d.hashtags);
        assert!(!d.caption.contains('#'), "хэштеги только отдельным блоком");
        assert!(full.chars().all(|c| c.len_utf16() == 1), "только символы BMP: длина не зависит от способа подсчёта");
        texts.push(d.caption);
    }
    if texts.len() > 1 {
        eprintln!("сходство двух описаний одного ролика: {:.2}", similarity(&texts[0], &texts[1]));
    }
    client.shutdown().await;
}

/// Быстрая проверка грамматики хэштегов на настоящей модели (секунды): llama.cpp принимает грамматику, теги русские и разные.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "нужна модель LLM и собранный llm-worker"]
async fn a_real_model_returns_russian_hashtags_in_the_grammar() {
    let Some(dir) = ModelPaths::default_dir() else { return };
    let model = ModelPaths::in_dir(&dir).llm;
    if !model.is_file() || find_worker().is_none() {
        eprintln!("нет модели LLM или llm-worker не собран — тест пропущен");
        return;
    }
    let opt = Options::default();
    let mut client = LlmClient::start(&model, opt.threads, opt.llm_gpu_layers).await.expect("воркер не стартовал");
    for seed in [11u32, 4242] {
        let raw = client.complete_with(&tags_prompt("Кот на плите", TRANSCRIPT), TAGS_GRAMMAR, 120, Sampling { temperature: 0.5, seed: Some(seed), repeat_penalty: None }).await.expect("ошибка генерации");
        let tags = parse_tags(&raw);
        eprintln!("зерно {seed}: {raw} -> {tags:?}");
        assert!((3..=8).contains(&tags.len()), "{tags:?}");
        assert!(tags.iter().all(|t| t[1..].chars().all(|c| matches!(c, 'а'..='я' | 'ё'))), "только русские буквы: {tags:?}");
    }
    client.shutdown().await;
}
