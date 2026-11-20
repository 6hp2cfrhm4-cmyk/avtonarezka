//! Дешёвый проход и внешние сигналы: реальный ffmpeg и локальный HTTP, без моделей/сети.
use avtonarezka_lib::{
    analyze::{
        chat,
        clips::TwitchApi,
        pipeline::{self, Options},
        score::Params,
        scout, Candidate,
    },
    models::ModelPaths,
};
use axum::{
    extract::{Form, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[tokio::test]
async fn synthetic_long_video_chat_and_audio_peak_reaches_top_inside_scout_windows() {
    let dir = std::env::temp_dir().join(format!("avto-scout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("long.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=1:d=1200",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=16000:duration=1200",
            "-af",
            "volume='if(between(t,840,846),1,0.03)':eval=frame",
            "-vf",
            "drawbox=x=0:y=0:w=32:h=36:color=white:t=fill:enable='between(t,840,846)'",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let comments:Vec<_>=(0..30).map(|i|json!({"content_offset_seconds":840.0+i as f64/10.0,"commenter":{"_id":i.to_string()},"message":{"body":"KEKW Pog"}})).collect();
    let features =
        chat::parse(json!({"comments":comments}).to_string().as_bytes(), 1200.0).unwrap();
    let opt = Options {
        camera: Some(avtonarezka_lib::analyze::face::Rect {
            x: 0.0,
            y: 0.0,
            width: 0.2,
            height: 0.4,
        }),
        use_events: false,
        use_speech: false,
        use_llm: false,
        chat: Some(features.curve),
        params: Params {
            top_k: 4,
            ..Default::default()
        },
        ..Default::default()
    };
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let result = pipeline::run(
        &path,
        1200.0,
        true,
        &ModelPaths::in_dir(&dir.join("no-models")),
        &opt,
        rx,
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let report = result.signals.scout.unwrap();
    assert!(!report.full);
    assert!(report.selected_seconds <= 300.0);
    assert_eq!(report.speech_seconds, 0.0);
    assert!(
        result
            .moments
            .iter()
            .take(2)
            .any(|m| m.start <= 840.0 && m.end >= 843.0),
        "{:?}",
        result.moments
    );
    assert!(result
        .moments
        .iter()
        .all(|m| report.windows.iter().any(|w| w.contains(m.start, m.end))));
    assert!(result.signals.chat.unwrap()[1680] > 3.0);
    assert!(result.signals.loudness[1682] > 3.0);
    assert!(result.signals.camera.as_ref().unwrap()[1680] > 3.0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[derive(Default)]
struct Rec {
    queries: Vec<HashMap<String, String>>,
    bad_auth: bool,
    limit: bool,
}
type Shared = Arc<Mutex<Rec>>;
async fn token(
    State(r): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("client_credentials")
    );
    if r.lock().unwrap().bad_auth {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid_client"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"access_token":"fake-only","expires_in":100})),
    )
}
async fn videos(headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer fake-only");
    assert_eq!(headers.get("client-id").unwrap(), "fake-id");
    assert_eq!(q.get("id").unwrap(), "123");
    Json(json!({"data":[{"id":"123","user_id":"b1","created_at":"2026-11-13T00:00:00Z"}]}))
}
async fn clips(
    State(r): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let mut r = r.lock().unwrap();
    r.queries.push(q.clone());
    assert!(!q.contains_key("video_id"));
    assert_eq!(q.get("broadcaster_id").unwrap(), "b1");
    assert!(q.contains_key("ended_at"));
    if r.limit {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"rate limit"})),
        );
    }
    let page = if q.contains_key("after") {
        json!({"data":[{"id":"b","video_id":"123","vod_offset":500,"duration":20},{"id":"null","video_id":"123","vod_offset":null,"duration":10}],"pagination":{}})
    } else {
        json!({"data":[{"id":"a","video_id":"123","vod_offset":120,"duration":20},{"id":"other","video_id":"999","vod_offset":100,"duration":10}],"pagination":{"cursor":"page2"}})
    };
    (StatusCode::OK, Json(page))
}
#[tokio::test]
async fn helix_pagination_vod_filter_nullable_offsets_auth_and_rate_limit() {
    let rec = Shared::default();
    let app = Router::new()
        .route("/token", post(token))
        .route("/videos", get(videos))
        .route("/clips", get(clips))
        .with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = TwitchApi {
        base: base.clone(),
        token_url: format!("{base}/token"),
        client: reqwest::Client::new(),
    };
    let f = api
        .collect("fake-id", "fake-secret", "123", 1200.0)
        .await
        .unwrap();
    assert_eq!((f.clips, f.missing_offset, f.truncated), (2, 1, false));
    assert!(f.curve[240] > 3.0);
    assert_eq!(rec.lock().unwrap().queries.len(), 2);
    rec.lock().unwrap().limit = true;
    assert!(api
        .collect("fake-id", "fake-secret", "123", 1200.0)
        .await
        .unwrap_err()
        .to_string()
        .contains("429"));
    rec.lock().unwrap().bad_auth = true;
    assert!(api
        .collect("fake-id", "fake-secret", "123", 1200.0)
        .await
        .unwrap_err()
        .to_string()
        .contains("401"));
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "Нужен предоставленный пользователем AVTO_BENCH_VOD (3 часа) и существующие модели; ничего не загружает"]
async fn benchmark_three_hour_vod_scout_vs_full_top_ten() {
    let path = std::path::PathBuf::from(
        std::env::var_os("AVTO_BENCH_VOD").expect("Укажи путь AVTO_BENCH_VOD"),
    );
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .unwrap();
    let duration: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    assert!(duration >= 10740.0, "Нужен настоящий VOD не короче 3 часов");
    let models = ModelPaths::in_dir(&ModelPaths::default_dir().expect("Нет папки моделей"));
    assert!(
        models.missing().is_empty(),
        "Нужны уже установленные модели"
    );
    let language = std::env::var("AVTO_BENCH_LANGUAGE").unwrap_or_else(|_| "auto".into());
    let baseline = std::env::var_os("AVTO_BENCH_BASELINE").map(std::path::PathBuf::from);
    if let (Some(b), Some(out)) = (&baseline, std::env::var_os("AVTO_BENCH_REPORT")) {
        assert_ne!(
            b,
            &std::path::PathBuf::from(out),
            "Исходный baseline нельзя перезаписывать"
        );
    }
    let mut rows: Vec<Value> = Vec::new();
    let mut results: Vec<Vec<Candidate>> = Vec::new();
    if let Some(ref baseline) = baseline {
        let old: Value = serde_json::from_slice(&std::fs::read(baseline).unwrap()).unwrap();
        assert_eq!(
            old["source"].as_str(),
            path.to_str(),
            "Baseline другого исходника"
        );
        assert!(
            (old["duration"].as_f64().unwrap() - duration).abs() < 0.1,
            "Длительность исходника изменилась"
        );
        assert_eq!(
            old.get("language")
                .and_then(Value::as_str)
                .unwrap_or("auto"),
            language,
            "Язык baseline отличается"
        );
        assert_eq!(old["runs"][0]["scout"], false, "Нужен полный baseline");
        assert!(old["runs"][0]["elapsedSeconds"].as_f64().unwrap() > 0.0);
        rows.push(old["runs"][0].clone());
        results.push(serde_json::from_value(old["candidates"][0].clone()).unwrap());
        assert!(!results[0].is_empty());
        eprintln!("Использую полный baseline из {}", baseline.display());
    }
    let passes = if baseline.is_some() {
        vec![true]
    } else {
        vec![false, true]
    };
    for scout_enabled in passes {
        let options = Options {
            scout_enabled,
            language: language.clone(),
            params: Params {
                top_k: 10,
                ..Default::default()
            },
            ..Default::default()
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let start = std::time::Instant::now();
        let stages = Arc::new(Mutex::new((String::new(), Vec::<Value>::new(), 0usize)));
        let record = stages.clone();
        let a = pipeline::run(
            &path,
            duration,
            true,
            &models,
            &options,
            rx,
            Arc::new(move |p| {
                let mut stages = record.lock().unwrap();
                if stages.0 != p.stage {
                    stages.0 = p.stage.into();
                    stages.1.push(
                        json!({"stage":p.stage,"elapsedSeconds":start.elapsed().as_secs_f64()}),
                    );
                    stages.2 = usize::MAX;
                }
                let percent = (p.fraction * 10.0).floor() as usize;
                if stages.2 != percent {
                    stages.2 = percent;
                    eprintln!(
                        "{} {:.0}% {} ({:.1}с)",
                        p.stage,
                        p.fraction * 100.0,
                        p.message,
                        start.elapsed().as_secs_f64()
                    );
                }
            }),
        )
        .await
        .unwrap();
        rows.push(json!({"scout":scout_enabled,"elapsedSeconds":start.elapsed().as_secs_f64(),"report":a.signals.scout,"notes":a.notes,"stageStarts":stages.lock().unwrap().1}));
        if let Some(out) = std::env::var_os("AVTO_BENCH_SIGNALS") {
            std::fs::write(out, serde_json::to_vec(&a.signals).unwrap()).unwrap();
        }
        results.push(
            a.moments
                .into_iter()
                .map(|m| Candidate {
                    start: m.start,
                    end: m.end,
                    score: m.score,
                    climax: m.climax,
                })
                .collect::<Vec<_>>(),
        );
        // Сохраняем полный замер сразу: сравнение не теряет его при прерывании второго прохода.
        if let Some(out) = std::env::var_os("AVTO_BENCH_REPORT") {
            let report = json!({"source":path,"duration":duration,"language":language,"baselineReused":baseline,"runs":rows,"candidates":results,
                "top10Overlap":(results.len()==2).then(||scout::top_overlap(&results[0],&results[1],10))});
            std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
    }
    let report = json!({"source":path,"duration":duration,"language":language,"baselineReused":baseline,"runs":rows,"candidates":results,"top10Overlap":scout::top_overlap(&results[0],&results[1],10),"speedup":rows[0]["elapsedSeconds"].as_f64().unwrap()/rows[1]["elapsedSeconds"].as_f64().unwrap()});
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(out) = std::env::var_os("AVTO_BENCH_REPORT") {
        std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
