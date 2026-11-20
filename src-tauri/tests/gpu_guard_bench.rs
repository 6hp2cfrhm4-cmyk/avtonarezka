//! Настоящая установленная LLM + NVENC и десять одновременно поставленных черновиков.
//! AVTO_GPU_BENCH_DIR — изолированная папка отчёта, без сети/скачивания/публикаций.
#![cfg(target_os = "linux")]
use avtonarezka_lib::{
    analyze::{
        llm::{build_prompt, parse_verdict, LlmClient, GRAMMAR, MAX_TOKENS},
        Sentence,
    },
    models::ModelPaths,
    operations::gpu,
    render::*,
};
use std::{
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

fn probe(path: &std::path::Path) -> serde_json::Value {
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_name,width,height:stream_tags=encoder",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(o.status.success());
    serde_json::from_slice(&o.stdout).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "реальные GPU4ГБ/LLM/NVENC и10ffmpeg; запускать отдельно без других benchmark"]
async fn real_llm_nvenc_and_ten_queued_drafts() {
    let dir = std::env::var_os("AVTO_GPU_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("avto-gpu-bench-{}", std::process::id()))
        });
    std::fs::create_dir_all(&dir).unwrap();
    let model =
        ModelPaths::in_dir(&ModelPaths::default_dir().expect("установленная папка моделей")).llm;
    assert!(
        model.is_file(),
        "ничего не скачиваем: нужна уже установленная модель"
    );
    if std::env::var_os("AVTO_GPU_BENCH_CPU_PREFLIGHT").is_some() {
        let mut model = LlmClient::start(&model, 4, 99).await.unwrap();
        assert!(
            !model.on_gpu(),
            "низкая свободная VRAM должна выбрать CPU до загрузки"
        );
        let text=model.complete("<|im_start|>user\nОтветь: готово<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n","root ::= \"готово\"",16).await.unwrap();
        assert_eq!(text.trim(), "готово");
        model.shutdown().await;
        return;
    }
    let source = dir.join("source.mp4");
    assert!(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1280x720:r=30:d=3",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=3",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-threads",
            "2",
            "-c:a",
            "aac",
            "-shortest"
        ])
        .arg(&source)
        .status()
        .unwrap()
        .success());
    let before = gpu::memory()
        .await
        .expect("эта живая проверка требует NVIDIA и nvidia-smi");
    assert_eq!(
        before.total_mib, 4096,
        "зафиксированный benchmark именно4ГБ; для другой карты отдельный отчёт"
    );
    let started = Instant::now();
    let mut llm = LlmClient::start(&model, 4, 99).await.unwrap();
    assert!(
        llm.on_gpu(),
        "перед GPU benchmark освободи память; CPU fallback проверяется отдельно"
    );
    let loaded = gpu::memory().await.unwrap();
    assert!(
        before.free_mib.saturating_sub(loaded.free_mib) > 1024,
        "модель должна действительно занять GPU"
    );
    let prompt=build_prompt(&[Sentence{start:0.0,end:3.0,text:"Игрок вернулся на трассу, заметил ошибку, сбросил скорость и прошёл поворот. Потом он рассмеялся.".into()}],0.0,3.0);
    // Прямая проба совместимости только в benchmark; production всегда проходит guard.
    let raw_out = dir.join("raw-overlap.mp4");
    let raw = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&source)
        .args([
            "-t",
            "2",
            "-vf",
            "scale=1080:1920",
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-c:a",
            "aac",
        ])
        .arg(&raw_out)
        .kill_on_drop(true)
        .output();
    let (raw, answer) = tokio::join!(raw, llm.complete(&prompt, GRAMMAR, MAX_TOKENS));
    let raw = raw.unwrap();
    let overlap_answer = answer
        .map(|s| parse_verdict(&s, 3.0).map(|_| ()))
        .and_then(|r| r);
    let overlap_memory = gpu::memory().await;
    let lane = Arc::new(tokio::sync::Semaphore::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut set = tokio::task::JoinSet::new();
    let queued_at = Instant::now();
    for i in 0..10 {
        let (dir, source, lane, active, peak) = (
            dir.clone(),
            source.clone(),
            lane.clone(),
            active.clone(),
            peak.clone(),
        );
        set.spawn(async move {
            let _permit = lane.acquire_owned().await.unwrap();
            let n = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            let mref = MediaRef {
                path: source,
                duration: 3.0,
                has_audio: true,
            };
            let job = RenderJob {
                source: mref.clone(),
                source_aspect: 16.0 / 9.0,
                start: 0.0,
                end: 3.0,
                banner: None,
                banner_at: None,
                gameplay: MediaRef {
                    has_audio: false,
                    ..mref
                },
                gameplay_offset: 0.0,
                music: None,
                music_offset: 0.0,
                words: vec![],
                part: None,
                hook: None,
                cam_rect: None,
                style: Style::default(),
                profile: Profile::Draft,
                encoder: Encoder::Nvenc,
                decode: Decode::Nvidia,
                out: dir.join(format!("draft-{i}.mp4")),
            };
            let (_tx, rx) = tokio::sync::watch::channel(false);
            let started = Instant::now();
            let result = render(&job, &dir.join("work"), None, rx, |_| {}).await;
            active.fetch_sub(1, Ordering::SeqCst);
            result.unwrap();
            assert!(!job.out.with_extension("part.mp4").exists());
            let info = probe(&job.out);
            assert_eq!(info["streams"][0]["codec_name"], "h264");
            assert_eq!(info["streams"][0]["width"], 540);
            assert_eq!(info["streams"][0]["height"], 960);
            assert!(
                info["streams"][0]["tags"]["encoder"]
                    .as_str()
                    .unwrap()
                    .contains("libx264"),
                "ML держит карту: guarded render обязан быть CPU"
            );
            serde_json::json!({"draft":i,"seconds":started.elapsed().as_secs_f64(),"probe":info})
        });
    }
    let mut outputs = vec![];
    while let Some(result) = set.join_next().await {
        outputs.push(result.unwrap());
    }
    let ten_seconds = queued_at.elapsed().as_secs_f64();
    assert_eq!(outputs.len(), 10);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let held = gpu::memory().await;
    llm.shutdown().await;
    let after = gpu::memory().await;
    let after_nvenc = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&source)
        .args([
            "-t",
            "2",
            "-vf",
            "scale=1080:1920",
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-c:a",
            "aac",
        ])
        .arg(dir.join("nvenc-after.mp4"))
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    let mut report = serde_json::json!({"gpuMiB":4096,"before":before,"modelLoaded":loaded,"directOverlap":{"nvencOk":raw.status.success(),"stderr":String::from_utf8_lossy(&raw.stderr),"llmOk":overlap_answer.is_ok()},"afterOverlap":overlap_memory,"guard":"exclusive model; renders use CPU while model is alive","queued":10,"maxActive":peak.load(Ordering::SeqCst),"tenSeconds":ten_seconds,"whileModelHeld":held,"afterShutdown":after,"nvencAfterShutdown":{"ok":after_nvenc.status.success(),"stderr":String::from_utf8_lossy(&after_nvenc.stderr)},"totalSeconds":started.elapsed().as_secs_f64(),"outputs":outputs});
    std::fs::write(
        dir.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(
        after_nvenc.status.success(),
        "NVENC должен работать после освобождения модели: {}",
        String::from_utf8_lossy(&after_nvenc.stderr)
    );
    // Даже если сырая совместная проба отказала, production обязан сохранить модель рабочей.
    // Полную исправность GPU-ответа проверяем отдельной короткой генерацией после нагрузки.
    let mut llm = LlmClient::start(&model, 4, 99).await.unwrap();
    parse_verdict(
        &llm.complete(&prompt, GRAMMAR, MAX_TOKENS).await.unwrap(),
        3.0,
    )
    .unwrap();
    llm.shutdown().await;
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("cpu-preflight-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let probe = bin.join("nvidia-smi");
    std::fs::write(&probe, "#!/bin/sh\nprintf '4096, 100\\n'\n").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let paths = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "real_llm_nvenc_and_ten_queued_drafts",
            "--ignored",
            "--nocapture",
        ])
        .env("AVTO_GPU_BENCH_CPU_PREFLIGHT", "1")
        .env("PATH", paths)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    std::fs::write(
        dir.join("cpu-preflight.log"),
        [child.stdout.as_slice(), child.stderr.as_slice()].concat(),
    )
    .unwrap();
    assert!(
        child.status.success(),
        "CPU preflight: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    report["modelHealthyAfter"] = true.into();
    report["cpuPreflightOk"] = true.into();
    std::fs::write(
        dir.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
