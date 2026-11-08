//! LLM на настоящей модели через воркер. Пропускается, если нет модели или воркер не собран.

use avtonarezka_lib::{
    analyze::{
        llm::{build_prompt, find_worker, parse_verdict, LlmClient, GRAMMAR, MAX_TOKENS},
        Sentence,
    },
    models::ModelPaths,
};

#[tokio::test(flavor = "multi_thread")]
async fn model_answers_in_the_grammar_and_the_answer_parses() {
    let Some(dir) = ModelPaths::default_dir() else { return };
    let model = ModelPaths::in_dir(&dir).llm;
    if !model.is_file() || find_worker().is_none() {
        eprintln!("нет модели LLM или llm-worker не собран — тест пропущен");
        return;
    }
    let sentences: Vec<Sentence> = [
        (0.0, 6.0, "Вы не поверите, что случилось вчера на нашей кухне."),
        (6.0, 12.0, "Я поставил чайник и ушёл за телефоном."),
        (12.0, 19.0, "Возвращаюсь, а кот сидит на плите и смотрит на меня как ни в чём не бывало!"),
        (19.0, 25.0, "Хорошо, что плита была выключена, иначе это был бы не смешной ролик."),
    ]
    .iter()
    .map(|(s, e, t)| Sentence { start: *s, end: *e, text: t.to_string() })
    .collect();

    let started = std::time::Instant::now();
    let gpu_layers: u32 = std::env::var("AVTO_GPU_LAYERS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    eprintln!("слоёв на видеокарте: {gpu_layers}");
    let mut client = LlmClient::start(&model, 8, gpu_layers).await.expect("воркер не стартовал");
    eprintln!("модель загружена за {:.1} с", started.elapsed().as_secs_f64());

    let t = std::time::Instant::now();
    let text = client.complete(&build_prompt(&sentences, 0.0, 25.0), GRAMMAR, MAX_TOKENS).await.expect("ошибка генерации");
    eprintln!("ответ за {:.1} с: {text}", t.elapsed().as_secs_f64());
    let v = parse_verdict(&text, 25.0).expect("ответ не разобрался");
    assert!(v.climax_t >= 0.0 && v.climax_t <= 25.0);
    assert!((3..=5).contains(&v.hashtags.len()), "{:?}", v.hashtags);
    assert!(v.title.chars().count() <= 60 && v.caption.chars().count() <= 160);

    // второй запрос в том же процессе: воркер живой и не путает ответы
    let again = client.complete(&build_prompt(&sentences[..2], 0.0, 12.0), GRAMMAR, MAX_TOKENS).await.expect("второй запрос");
    parse_verdict(&again, 12.0).expect("второй ответ не разобрался");
    client.shutdown().await;
}
