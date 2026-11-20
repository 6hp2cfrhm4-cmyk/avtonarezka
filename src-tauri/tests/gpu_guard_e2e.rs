//! Отказы GPU подменяются только в дочернем тестовом процессе; CPU-файл кодирует настоящий ffmpeg.
#![cfg(target_os = "linux")]
use avtonarezka_lib::{
    operations::gpu,
    render::{self, *},
};
use std::{path::Path, process::Command};

fn job(dir: &Path) -> RenderJob {
    let source = MediaRef {
        path: dir.join("source.mp4"),
        duration: 2.0,
        has_audio: true,
    };
    RenderJob {
        source: source.clone(),
        source_aspect: 16.0 / 9.0,
        start: 0.0,
        end: 2.0,
        banner: None,
        banner_at: None,
        gameplay: MediaRef {
            has_audio: false,
            ..source
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
        out: dir.join("out.mp4"),
    }
}

#[tokio::test]
async fn gpu_fault_fallback_and_cpu_output_are_isolated() {
    let Ok(mode) = std::env::var("AVTO_GPU_TEST_CASE") else {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("avto-gpu-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        for mode in ["low", "busy", "nvdec", "cancel", "bad_media"] {
            let dir = base.join(mode);
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let probe = bin.join("nvidia-smi");
            std::fs::write(
                &probe,
                format!(
                    "#!/bin/sh\nprintf '4096, {}\\n'\n",
                    if mode == "low" { 100 } else { 3811 }
                ),
            )
            .unwrap();
            let ffmpeg = bin.join("ffmpeg");
            std::fs::write(&ffmpeg, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$AVTO_GPU_TEST_LOG\"\ncase \"$*\" in *-hwaccel*|*h264_nvenc*) printf 'GPU out of memory (fixture)\\n' >&2; exit 1;; esac\nexec /usr/bin/ffmpeg \"$@\"\n").unwrap();
            for p in [&probe, &ffmpeg] {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let paths = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )))
            .unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gpu_fault_fallback_and_cpu_output_are_isolated",
                    "--nocapture",
                ])
                .env("AVTO_GPU_TEST_CASE", mode)
                .env("AVTO_GPU_TEST_DIR", &dir)
                .env("AVTO_GPU_TEST_LOG", dir.join("attempts.log"))
                .env("PATH", paths)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::remove_dir_all(base).unwrap();
        return;
    };
    let dir = std::path::PathBuf::from(std::env::var_os("AVTO_GPU_TEST_DIR").unwrap());
    assert!(Command::new("/usr/bin/ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x180:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=2",
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
        .arg(dir.join("source.mp4"))
        .status()
        .unwrap()
        .success());
    let mut j = job(&dir);
    let _model = if mode == "busy" {
        Some(gpu::model().await)
    } else {
        None
    };
    if mode == "bad_media" {
        j.source.path = dir.join("missing.mp4");
        j.encoder = Encoder::X264;
        j.decode = Decode::Cpu;
    }
    std::fs::write(&j.out, b"existing final").unwrap();
    let (_tx, cancel) = tokio::sync::watch::channel(mode == "cancel");
    let limits = OutputLimits {
        max_seconds: Some(1.0),
        video_kbps: 600,
        audio_kbps: 96,
    };
    let result =
        render::ffmpeg::render_limited(&j, &dir.join("work"), None, cancel, Some(&limits), |_| {})
            .await;
    let attempts = std::fs::read_to_string(dir.join("attempts.log")).unwrap_or_default();
    assert!(!j.out.with_extension("part.mp4").exists());
    match mode.as_str() {
        "cancel" | "bad_media" => {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&j.out).unwrap(), b"existing final");
            assert_eq!(attempts.lines().count(), usize::from(mode == "bad_media"));
        }
        _ => {
            result.unwrap();
            assert_eq!(
                attempts.lines().count(),
                if mode == "nvdec" { 3 } else { 1 }
            );
            let last = attempts.lines().last().unwrap();
            assert!(last.contains("libx264"));
            assert!(!last.contains("-hwaccel"));
            let output = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration:stream=codec_name",
                    "-of",
                    "json",
                ])
                .arg(&j.out)
                .output()
                .unwrap();
            assert!(output.status.success());
            let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(
                (info["format"]["duration"]
                    .as_str()
                    .unwrap()
                    .parse::<f64>()
                    .unwrap()
                    - 1.0)
                    .abs()
                    < 0.001
            );
            assert_eq!(info["streams"][0]["codec_name"], "h264");
            assert_eq!(info["streams"][1]["codec_name"], "aac");
        }
    }
}
