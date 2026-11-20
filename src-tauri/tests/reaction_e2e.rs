//! Настоящий ffmpeg: цвет камеры, прозрачные углы, обводка, freeze и деградация.
use avtonarezka_lib::{
    analyze::face::Rect,
    render::{self, *},
};
use std::{path::Path, process::Command};
fn ff(args: &[&str], out: &Path) {
    let r = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .args(["-threads", "1"])
        .arg(out)
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
fn pixel(path: &Path, t: f64, x: usize, y: usize) -> [u8; 3] {
    let r = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &t.to_string(), "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
            "-",
        ])
        .output()
        .unwrap();
    assert!(r.status.success());
    r.stdout[(y * 540 + x) * 3..(y * 540 + x) * 3 + 3]
        .try_into()
        .unwrap()
}
fn red(p: [u8; 3]) -> bool {
    p[0] > 150 && p[1] < 100 && p[2] < 100
}
fn green(p: [u8; 3]) -> bool {
    p[1] > 80 && p[0] < 100 && p[2] < 100
}
fn blue(p: [u8; 3]) -> bool {
    p[2] > 150 && p[0] < 90 && p[1] < 90
}
#[tokio::test]
async fn actual_reaction_circle_alpha_ring_freeze_continue_stack_and_missing_face() {
    let dir = std::env::temp_dir().join(format!("avto-reaction-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.mkv");
    let gp = dir.join("gp.mkv");
    let banner = dir.join("banner.mkv");
    ff(&["-f","lavfi","-i","color=blue:s=320x180:r=30:d=5,drawbox=x=16:y=18:w=64:h=72:color=red:t=fill:enable='lt(t,2)',drawbox=x=16:y=18:w=64:h=72:color=green:t=fill:enable='gte(t,2)'","-c:v","ffv1"],&source);
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=180x320:r=30:d=5",
            "-c:v",
            "ffv1",
        ],
        &gp,
    );
    ff(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=0x00ff00:s=120x60:r=30:d=1",
            "-c:v",
            "ffv1",
        ],
        &banner,
    );
    let media = |p: &Path, d| MediaRef {
        path: p.to_path_buf(),
        duration: d,
        has_audio: false,
    };
    let mut style = Style::default();
    style.layout = Layout::ReactionPip;
    style.top_fit = TopFit::Fill;
    style.subtitles.enabled = false;
    style.part_label.enabled = false;
    style.reaction.shadow = false;
    style.reaction.pop = false;
    style.reaction.ring_px = 12.0;
    style.banner.anim = BannerAnim::None;
    style.banner.despill = false;
    let mut job = RenderJob {
        cam_rect: Some(Rect {
            x: 0.05,
            y: 0.1,
            width: 0.2,
            height: 0.4,
        }),
        decode: Decode::Cpu,
        source: media(&source, 5.0),
        source_aspect: 320.0 / 180.0,
        start: 0.0,
        end: 4.0,
        banner: Some(media(&banner, 1.0)),
        banner_at: Some(1.0),
        gameplay: media(&gp, 5.0),
        gameplay_offset: 0.0,
        music: None,
        music_offset: 0.0,
        words: vec![],
        part: None,
        hook: None,
        style,
        profile: Profile::Draft,
        encoder: Encoder::X264,
        out: dir.join("frozen.mp4"),
    };
    async fn run(job: &RenderJob, dir: &Path) {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        render::render(job, dir, None, rx, |_| {}).await.unwrap();
    }
    run(&job, &dir).await;
    assert!(red(pixel(&job.out, 1.5, 103, 480)));
    assert!(
        red(pixel(&job.out, 2.5, 103, 480)),
        "Камера должна отставать на время баннера"
    );
    assert!(green(pixel(&job.out, 3.5, 103, 480)));
    assert!(
        blue(pixel(&job.out, 0.5, 24, 401)),
        "Углы квадрата маски должны пропускать исходное видео"
    );
    let ring = pixel(&job.out, 0.5, 103, 401);
    assert!(ring.iter().all(|v| *v > 150), "Обводка: {ring:?}");
    job.style.reaction.freeze = false;
    job.out = dir.join("continuing.mp4");
    run(&job, &dir).await;
    assert!(green(pixel(&job.out, 2.5, 103, 480)));
    job.cam_rect = None;
    job.out = dir.join("missing.mp4");
    run(&job, &dir).await;
    assert!(
        blue(pixel(&job.out, 0.5, 103, 480)),
        "Без лица кружок отсутствует"
    );
    job.cam_rect = Some(Rect {
        x: 0.05,
        y: 0.1,
        width: 0.2,
        height: 0.4,
    });
    job.style.layout = Layout::ReactionStack;
    job.out = dir.join("stack.mp4");
    run(&job, &dir).await;
    assert!(
        red(pixel(&job.out, 0.5, 270, 720)),
        "Камера занимает отдельную нижнюю область"
    );
    job.style.layout = Layout::ReactionPip;
    job.style.reaction.background = false;
    job.style.reaction.shadow = true;
    job.style.reaction.pop = true;
    job.out = dir.join("full.mp4");
    run(&job, &dir).await;
    assert!(job.out.is_file());
    std::fs::remove_dir_all(dir).unwrap();
}
