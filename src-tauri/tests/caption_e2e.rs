//! Проверяет установленную модель: генерируются только хэштеги.
use avtonarezka_lib::{
    analyze::{
        llm::{find_worker, LlmClient},
        pipeline::Options,
    },
    caption::{self, CaptionSpec, Fields, DEFAULT_TEMPLATE},
    models::ModelPaths,
    publish::PlatformId,
};
#[tokio::test]
#[ignore = "нужны установленная LLM и llm-worker; ничего не скачивает"]
async fn local_hashtags_preserve_optional_manual_text_and_platform_metadata() {
    let path = ModelPaths::in_dir(&ModelPaths::default_dir().expect("папка моделей")).llm;
    assert!(path.is_file() && find_worker().is_some());
    let opt = Options::default();
    let mut client = LlmClient::start(&path, opt.threads, opt.llm_gpu_layers)
        .await
        .unwrap();
    let cases=[
        ("Возвращение на трассу","Участник повторил заезд, сбросил скорость перед поворотом, исправил траекторию и улучшил время на две секунды. Затем он удивился результату и засмеялся.",vec!["трасса","автомобиль","спорт","заезд","скорость","поворот","результат","смех","автоспорт","гонки"]),
        ("Как приготовить борщ","Нарезаем свёклу, морковь и лук. Варим бульон, добавляем капусту и картофель. Подаём борщ со сметаной.",vec!["борщ","кулинария","рецепт","овощи","еда","суп","готовка"]),
        ("Кот и пылесос","Кот внимательно смотрел на пылесос, потом подпрыгнул и спрятался за диваном. Хозяин рассмеялся.",vec!["кот","кошка","пылесос","диван","животные","юмор","смех","хозяин"]),
        ("Зимняя прогулка по лесу","",vec!["зима","лес","прогулка","природа","снег","погода"]),
        ("Разбираем ошибку в Python","Нашли неверное условие в цикле, исправили код и запустили программу. Теперь тест проходит.",vec!["ошибка","цикл","код","программирование","питон","программа","тест","отладка"]),
        ("Соло на гитаре","Играю новую мелодию на электрогитаре. Сегодня разбираем аккорды и быстрый переход между ними.",vec!["гитара","музыка","аккорды","мелодия","электрогитара","соло"]),
    ];
    let mut evaluations = vec![];
    let mut tags = vec![];
    for (title, speech, allowed) in cases {
        let generated = caption::generate::tags(&mut client, title, speech, 77)
            .await
            .unwrap();
        assert_eq!(generated.len(), 3);
        assert!(
            generated
                .iter()
                .all(|tag| allowed.contains(&tag.trim_start_matches('#'))),
            "{title}: {generated:?}"
        );
        if tags.is_empty() {
            tags = generated.clone();
        }
        evaluations.push(serde_json::json!({"title":title,"tags":generated}));
    }
    let mut outputs = vec![];
    for platform in [
        PlatformId::Telegram,
        PlatformId::Youtube,
        PlatformId::Instagram,
        PlatformId::Tiktok,
    ] {
        for body in ["", "Мой короткий текст без обязательной длины."]
        {
            let fields = Fields {
                title: "Возвращение на трассу".into(),
                body: body.into(),
                credit: "Автор записи".into(),
                link: "https://example.com/?utm_source=test".into(),
                promo: "ТЕСТ".into(),
                hashtags: tags.clone(),
            };
            let composed = caption::compose(
                &CaptionSpec {
                    platform,
                    template: DEFAULT_TEMPLATE.into(),
                },
                &fields,
            )
            .unwrap();
            assert_eq!(composed.min, 0);
            assert!(!composed.body_trimmed);
            assert!(
                composed.text.contains("Источник: Автор записи")
                    && composed.text.contains("Промокод: ТЕСТ")
            );
            assert_eq!(fields.body, body);
            outputs
                .push(serde_json::json!({"platform":platform,"fields":fields,"composed":composed}));
        }
    }
    client.shutdown().await;
    if let Some(path) = std::env::var_os("AVTO_CAPTION_REPORT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &serde_json::json!({"evaluations":evaluations,"platformOutputs":outputs}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}
