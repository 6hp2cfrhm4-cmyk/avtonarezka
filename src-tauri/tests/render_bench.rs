//! Замер рендера 1080×1920 на настоящем ffmpeg: процессор против NVDEC. Не выполняется по умолчанию:
//!   cargo test --test render_bench -- --ignored --nocapture
//! Папка для медиа — `AVTO_BENCH_DIR` (по умолчанию временная). Исходники создаются один раз и переиспользуются.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use avtonarezka_lib::render::*;

fn dir() -> PathBuf {
    let d = std::env::var("AVTO_BENCH_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("avto-bench"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-y", "-v", "error"]).args(args).status().expect("ffmpeg не запустился");
    assert!(st.success(), "ffmpeg {args:?}");
}

/// Длинный 1080p-исходник с движением (как живое видео) и звуком; геймплей 1080×1920.
fn media(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let source = d.join("bench_source_1080p.mp4");
    let gameplay = d.join("bench_gameplay_1080x1920.mp4");
    let banner = d.join("bench_banner.mp4");
    if !source.exists() {
        ffmpeg(&[
            "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=30:d=90", "-f", "lavfi", "-i", "sine=f=440:d=90",
            "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", source.to_str().unwrap(),
        ]);
    }
    if !gameplay.exists() {
        ffmpeg(&[
            "-f", "lavfi", "-i", "testsrc=s=1080x1920:r=30:d=90", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p", "-an", gameplay.to_str().unwrap(),
        ]);
    }
    if !banner.exists() {
        ffmpeg(&[
            "-f", "lavfi", "-i", "color=c=0x00ff00:s=640x360:r=30:d=3", "-f", "lavfi", "-i", "sine=f=880:d=3",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", banner.to_str().unwrap(),
        ]);
    }
    (source, gameplay, banner)
}

fn job(d: &Path, decode: Decode, out: &str) -> RenderJob {
    let (source, gameplay, banner) = media(d);
    let mref = |p: PathBuf, dur: f64, audio: bool| MediaRef { path: p, duration: dur, has_audio: audio };
    RenderJob {
        decode,
        source: mref(source, 90.0, true),
        source_aspect: 1920.0 / 1080.0,
        start: 5.0,
        end: 45.0,
        banner: Some(mref(banner, 3.0, true)),
        banner_at: Some(20.0),
        gameplay: mref(gameplay, 90.0, false),
        gameplay_offset: 0.0,
        music: None,
        music_offset: 0.0,
        words: vec![],
        part: None,
        style: Style::default(),
        profile: Profile::Final,
        encoder: Encoder::Nvenc,
        out: d.join(out),
    }
}

#[tokio::test]
#[ignore = "медленно: замер на настоящем ffmpeg"]
async fn cpu_decode_against_nvdec() {
    let d = dir();
    let fonts = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../assets/fonts"));
    let work = d.join("work");
    for (decode, name) in [(Decode::Cpu, "cpu.mp4"), (Decode::Nvidia, "nvdec.mp4"), (Decode::Cpu, "cpu2.mp4"), (Decode::Nvidia, "nvdec2.mp4")] {
        let j = job(&d, decode, name);
        let _ = std::fs::remove_file(&j.out);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let t = Instant::now();
        render(&j, &work, Some(&fonts), rx, |_| {}).await.unwrap_or_else(|e| panic!("{name}: рендер упал: {e:#}"));
        println!("{name:12} {:?}  decode={decode:?}  размер {} КБ", t.elapsed(), std::fs::metadata(&j.out).unwrap().len() / 1024);
    }
}
