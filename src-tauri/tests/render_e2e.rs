//! Сквозная проверка на настоящем ffmpeg: синтетические видео → ролик → разбор кадров.
//! Баннер — зелёный фон с пурпурным квадратом, поэтому «зелёный остался» и «баннер виден»
//! проверяются по пикселям.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use avtonarezka_lib::render::*;

const W: usize = 540;
const H: usize = 960;
const H_TOP: usize = 480;

struct Media {
    dir: PathBuf,
    source: PathBuf,
    banner: PathBuf,
    gameplay: PathBuf,
    music: PathBuf,
}

fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-y", "-v", "error"]).args(args).status().expect("ffmpeg не запустился");
    assert!(st.success(), "ffmpeg {args:?}");
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("avto-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = Media { source: dir.join("source.mp4"), banner: dir.join("banner.mp4"), gameplay: dir.join("gameplay.mp4"), music: dir.join("music.m4a"), dir };
        let enc = ["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"];

        // Исходник: быстро меняющийся фон без чистого зелёного + тон 440 Гц.
        ffmpeg(&[
            "-f", "lavfi", "-i", "color=c=0x203060:s=1280x720:r=30:d=12,format=gbrp,geq=r='128+100*sin(T*4+X/90)':g=40:b='100+50*sin(T*2)'",
            "-f", "lavfi", "-i", "sine=f=440:d=12",
        ].iter().copied().chain(enc.iter().copied()).chain([m.source.to_str().unwrap()]).collect::<Vec<_>>());

        // Баннер: зелёный фон, пурпурный квадрат в центре, тон 880 Гц.
        ffmpeg(&[
            "-f", "lavfi", "-i", "color=c=0x00ff00:s=640x360:r=30:d=3,drawbox=x=170:y=90:w=300:h=180:color=0xff00ff:t=fill",
            "-f", "lavfi", "-i", "sine=f=880:d=3",
        ].iter().copied().chain(enc.iter().copied()).chain([m.banner.to_str().unwrap()]).collect::<Vec<_>>());

        // Геймплей: вертикальный, тоже без чистого зелёного.
        ffmpeg(&[
            "-f", "lavfi", "-i", "color=c=0x305020:s=720x1280:r=30:d=6,format=gbrp,geq=r='60+60*sin(T*5+Y/80)':g=90:b='120+80*sin(T*3+X/70)'",
        ].iter().copied().chain(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-an", m.gameplay.to_str().unwrap()]).collect::<Vec<_>>());
        // Музыка короче ролика: должна зацикливаться.
        ffmpeg(&["-f", "lavfi", "-i", "sine=f=220:d=4", "-c:a", "aac", m.music.to_str().unwrap()]);
        m
    })
}

fn mref(path: &Path, dur: f64, audio: bool) -> MediaRef {
    MediaRef { path: path.to_path_buf(), duration: dur, has_audio: audio }
}

fn job(name: &str) -> RenderJob {
    let m = media();
    RenderJob {
        source: mref(&m.source, 12.0, true),
        source_aspect: 1280.0 / 720.0,
        start: 2.0,
        end: 8.0,
        banner: Some(mref(&m.banner, 3.0, true)),
        banner_at: Some(2.5),
        gameplay: mref(&m.gameplay, 6.0, false),
        gameplay_offset: 0.0,
        music: None,
        music_offset: 0.0,
        words: vec![],
        part: None,
        style: Style::default(),
        profile: Profile::Draft,
        encoder: Encoder::X264,
        out: m.dir.join(format!("{name}.mp4")),
    }
}

async fn run(j: &RenderJob) {
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let fonts = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../assets/fonts"));
    let work = media().dir.join("work");
    render(j, &work, Some(&fonts), rx, |_| {}).await.unwrap_or_else(|e| panic!("рендер упал: {e:#}"));
    assert!(j.out.exists(), "нет файла {:?}", j.out);
}

fn probe(path: &Path) -> (usize, usize, f64, bool) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height:format=duration", "-of", "json"])
        .arg(path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let streams = v["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    (
        video["width"].as_u64().unwrap() as usize,
        video["height"].as_u64().unwrap() as usize,
        v["format"]["duration"].as_str().unwrap().parse().unwrap(),
        streams.iter().any(|s| s["codec_type"] == "audio"),
    )
}

/// Кадр целиком в RGB24; заодно сохраняем PNG рядом, чтобы на него можно было посмотреть глазами.
fn frame(path: &Path, t: f64, label: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &t.to_string(), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert_eq!(out.stdout.len(), W * H * 3, "кадр на t={t}: {}", String::from_utf8_lossy(&out.stderr));
    let png = path.with_file_name(format!("{}-{label}.png", path.file_stem().unwrap().to_string_lossy()));
    let _ = Command::new("ffmpeg").args(["-v", "error", "-y", "-ss", &t.to_string(), "-i"]).arg(path).args(["-frames:v", "1"]).arg(png).status();
    out.stdout
}

fn region(buf: &[u8], x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<[u8; 3]> {
    let mut v = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * W + x) * 3;
            v.push([buf[i], buf[i + 1], buf[i + 2]]);
        }
    }
    v
}

fn count(px: &[[u8; 3]], f: impl Fn(u8, u8, u8) -> bool) -> usize {
    px.iter().filter(|p| f(p[0], p[1], p[2])).count()
}

fn mean_abs_diff(a: &[[u8; 3]], b: &[[u8; 3]]) -> f64 {
    let s: u64 = a.iter().zip(b).flat_map(|(p, q)| (0..3).map(move |c| (p[c] as i32 - q[c] as i32).unsigned_abs() as u64)).sum();
    s as f64 / (a.len() * 3) as f64
}

// Полосы слева, куда баннер (x ≥ 76) не заходит.
fn top_strip(f: &[u8]) -> Vec<[u8; 3]> {
    region(f, 0, 10, 60, H_TOP - 10)
}
fn bottom_strip(f: &[u8]) -> Vec<[u8; 3]> {
    region(f, 0, H_TOP + 10, 60, H - 10)
}

#[tokio::test(flavor = "multi_thread")]
async fn banner_freezes_top_and_green_is_keyed_out() {
    let j = job("banner");
    run(&j).await;

    let (w, h, dur, audio) = probe(&j.out);
    assert_eq!((w, h), (W, H));
    assert!((dur - 9.0).abs() < 0.2, "длительность {dur}, ждали 9.0 (клип 6 + баннер 3)");
    assert!(audio, "в ролике нет звука");

    // Пока баннер играет (2.5..5.5), он виден, а зелёного фона нет.
    let during = frame(&j.out, 4.0, "banner");
    let banner_area = region(&during, 76, 371, 464, 589);
    let green = count(&banner_area, |r, g, b| g > 200 && r < 90 && b < 90);
    let magenta = count(&banner_area, |r, g, b| r > 200 && b > 200 && g < 70);
    assert!(green * 200 < banner_area.len(), "зелёный фон не вырезан: {green} из {}", banner_area.len());
    assert!(magenta * 10 > banner_area.len(), "баннер не виден: {magenta} из {}", banner_area.len());

    // До баннера и после баннера пурпурного нет.
    for (t, label) in [(1.0, "before"), (7.0, "after")] {
        let f = frame(&j.out, t, label);
        let area = region(&f, 76, 371, 464, 589);
        assert_eq!(count(&area, |r, g, b| r > 200 && b > 200 && g < 70), 0, "баннер на t={t}");
    }

    // Верх замирает на время баннера и двигается до и после.
    let a = top_strip(&frame(&j.out, 3.0, "freeze-a"));
    let b = top_strip(&frame(&j.out, 5.0, "freeze-b"));
    assert!(mean_abs_diff(&a, &b) < 2.0, "верх не замёрз: {}", mean_abs_diff(&a, &b));
    let c = top_strip(&frame(&j.out, 1.0, "move-a"));
    let d = top_strip(&frame(&j.out, 1.8, "move-b"));
    assert!(mean_abs_diff(&c, &d) > 8.0, "верх не двигается до баннера: {}", mean_abs_diff(&c, &d));
    let e = top_strip(&frame(&j.out, 6.0, "move-c"));
    let f = top_strip(&frame(&j.out, 6.8, "move-d"));
    assert!(mean_abs_diff(&e, &f) > 8.0, "верх не продолжился после баннера");

    // Геймплей по умолчанию продолжает играть.
    let g1 = bottom_strip(&frame(&j.out, 3.0, "gp-a"));
    let g2 = bottom_strip(&frame(&j.out, 5.0, "gp-b"));
    assert!(mean_abs_diff(&g1, &g2) > 8.0, "геймплей замер, хотя не должен");
}

#[tokio::test(flavor = "multi_thread")]
async fn gameplay_freezes_with_banner_when_enabled() {
    let mut j = job("gpfreeze");
    j.style.gameplay_freeze = true;
    run(&j).await;
    let g1 = bottom_strip(&frame(&j.out, 3.0, "a"));
    let g2 = bottom_strip(&frame(&j.out, 5.0, "b"));
    assert!(mean_abs_diff(&g1, &g2) < 2.0, "геймплей не замёрз: {}", mean_abs_diff(&g1, &g2));
    let g3 = bottom_strip(&frame(&j.out, 6.0, "c"));
    let g4 = bottom_strip(&frame(&j.out, 6.8, "d"));
    assert!(mean_abs_diff(&g3, &g4) > 8.0, "геймплей не продолжился после баннера");
}

#[tokio::test(flavor = "multi_thread")]
async fn subtitles_hide_during_banner_and_part_label_stays() {
    let mut j = job("subs");
    j.part = Some(3);
    // слова в секундах исходника: клип начинается с 2.0, баннер на 2.5 клипа (= 4.5 источника)
    j.words = vec![
        Word { text: "привет".into(), start: 3.0, end: 3.6 },
        Word { text: "мир".into(), start: 6.0, end: 6.6 },
    ];
    run(&j).await;

    let lime = |r: u8, g: u8, b: u8| r > 140 && g > 220 && b < 110;
    let white = |r: u8, g: u8, b: u8| r > 245 && g > 245 && b > 245;
    // подсветка слова — на линии раздела (y ≈ 480)
    let sub = |f: &[u8]| count(&region(f, 0, 400, W, 560), lime);
    // «Часть 3» — у верха (y ≈ 0.11·960)
    let label = |f: &[u8]| count(&region(f, 0, 70, W, 140), white);

    let before = frame(&j.out, 1.2, "sub-before"); // слово «привет»: клип 1.0..1.6
    let during = frame(&j.out, 4.0, "sub-during"); // баннер
    let after = frame(&j.out, 7.2, "sub-after"); //   слово «мир»: клип 4.0..4.6 → ролик 7.0..7.6

    assert!(sub(&before) > 40, "нет субтитров до баннера: {}", sub(&before));
    assert_eq!(sub(&during), 0, "субтитры видны во время баннера");
    assert!(sub(&after) > 40, "нет субтитров после баннера (сдвиг на длину баннера): {}", sub(&after));
    for (f, name) in [(&before, "до"), (&during, "во время"), (&after, "после")] {
        assert!(label(f) > 40, "нет надписи «Часть 3» {name} баннера: {}", label(f));
    }
}

/// 16:9 с красной меткой у левого края и синей у правого: видно, обрезаны ли края.
fn edge_marked_source() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let path = media().dir.join("edges.mp4");
        ffmpeg(&[
            "-f", "lavfi", "-i",
            "color=c=0x808080:s=1280x720:r=30:d=12,drawbox=x=0:y=0:w=60:h=720:color=0xff0000:t=fill,drawbox=x=1220:y=0:w=60:h=720:color=0x0000ff:t=fill",
            "-f", "lavfi", "-i", "sine=f=440:d=12",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", path.to_str().unwrap(),
        ]);
        path
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn wide_source_is_fully_visible_by_default_and_cropped_only_when_asked() {
    let red = |r: u8, g: u8, b: u8| r > 180 && g < 80 && b < 80;
    let blue = |r: u8, g: u8, b: u8| b > 180 && r < 80 && g < 80;

    let mut j = job("wide");
    j.source = mref(edge_marked_source(), 12.0, true);
    j.banner = None;
    run(&j).await;
    let f = frame(&j.out, 1.0, "wide");
    // видео 16:9 в зоне 540×480 занимает y = 88..392 на всю ширину: метки у обоих краёв на месте
    let (left, right) = (region(&f, 2, 200, 20, 280), region(&f, 520, 200, 538, 280));
    assert!(count(&left, red) * 10 > left.len() * 9, "левый край обрезан: {} из {}", count(&left, red), left.len());
    assert!(count(&right, blue) * 10 > right.len() * 9, "правый край обрезан: {} из {}", count(&right, blue), right.len());
    // над и под видео — размытая копия кадра, а не чёрные поля
    for (y0, y1) in [(10, 60), (420, 470)] {
        let band = region(&f, 100, y0, 440, y1);
        let mean = band.iter().map(|p| p[0] as u32 + p[1] as u32 + p[2] as u32).sum::<u32>() / (band.len() as u32 * 3);
        assert!(mean > 30, "чёрная полоса на y={y0}: яркость {mean}");
    }

    // «Заполнить кадр» по-прежнему режет края: проверка различает режимы
    let mut fill = job("wide-fill");
    fill.source = mref(edge_marked_source(), 12.0, true);
    fill.banner = None;
    fill.style.top_fit = TopFit::Fill;
    run(&fill).await;
    let cropped = region(&frame(&fill.out, 1.0, "wide-fill"), 2, 200, 20, 280);
    assert_eq!(count(&cropped, red), 0, "в режиме «Заполнить» красная метка должна быть срезана");
}

/// Геймплей, где каждая секунда имеет свою яркость: по кадру видно, откуда взят кусок.
fn steps_gameplay() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let path = media().dir.join("steps.mp4");
        ffmpeg(&[
            "-f", "lavfi", "-i",
            "color=c=black:s=270x480:r=30:d=6,geq=lum='if(lt(T,1),20,if(lt(T,2),60,if(lt(T,3),100,if(lt(T,4),140,if(lt(T,5),180,220)))))':cb=128:cr=128",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-an", path.to_str().unwrap(),
        ]);
        path
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn gameplay_starts_at_its_offset_and_a_long_clip_wraps_to_the_file_start() {
    let mut j = job("gp-offset");
    j.banner = None;
    j.gameplay = mref(steps_gameplay(), 6.0, false);
    j.gameplay_offset = 3.0;
    run(&j).await; // клип 6 с, фон с 3-й секунды

    // яркость кадра фона, переведённая из студийного диапазона в 0..255
    let level = |t: f64| {
        let f = frame(&j.out, t, "gp");
        let px = region(&f, 20, H_TOP + 40, W - 20, H - 40);
        px.iter().map(|p| p[0] as f64).sum::<f64>() / px.len() as f64
    };
    let studio = |y: f64| (y - 16.0) * 255.0 / 219.0;
    // секунды 3, 4, 5 файла, затем цикл возвращается в начало файла (а не в смещение)
    for (t, y) in [(0.5, 140.0), (1.5, 180.0), (2.5, 220.0), (3.5, 20.0), (4.5, 60.0)] {
        let got = level(t);
        assert!((got - studio(y)).abs() < 10.0, "t={t}: яркость {got:.0}, ждали {:.0}", studio(y));
    }
}

fn nvenc_works() -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "color=s=256x256:d=0.2", "-c:v", "h264_nvenc", "-f", "null", "-"])
        .status()
        .is_ok_and(|s| s.success())
}

/// Ветки, которых не касаются остальные тесты: любая опечатка в них ломает синтаксис фильтра.
#[tokio::test(flavor = "multi_thread")]
async fn every_style_variant_renders_with_the_right_length() {
    let m = media();
    let mut variants: Vec<(String, RenderJob)> = Vec::new();

    for (name, anim) in [("none", BannerAnim::None), ("fade", BannerAnim::Fade), ("slide", BannerAnim::SlideUp), ("pop", BannerAnim::Pop)] {
        let mut j = job(&format!("v-anim-{name}"));
        j.style.banner.anim = anim;
        variants.push((format!("анимация {name}"), j));
    }

    let mut j = job("v-contain");
    j.style.top_fit = TopFit::Contain;
    j.style.split = 0.6;
    variants.push(("Contain + split 0.6".into(), j));

    let mut j = job("v-offset");
    j.style.banner.offset_x = 0.1;
    j.style.banner.offset_y = -0.05;
    j.style.banner.width_pct = 0.5;
    j.style.banner.despill = false;
    variants.push(("смещение баннера, без despill".into(), j));

    let mut j = job("v-music");
    j.music = Some(mref(&m.music, 4.0, true));
    j.style.audio.gameplay = 0.5;
    j.gameplay.has_audio = false;
    variants.push(("музыка".into(), j));

    let mut j = job("v-nobanner");
    j.banner = None;
    j.part = Some(2);
    variants.push(("без баннера".into(), j));

    let mut j = job("v-nosound");
    j.source.has_audio = false;
    j.banner.as_mut().unwrap().has_audio = false;
    variants.push(("совсем без звука".into(), j));

    for (name, j) in &variants {
        run(j).await;
        let (w, h, dur, _) = probe(&j.out);
        assert_eq!((w, h), (W, H), "{name}");
        assert!((dur - j.out_len()).abs() < 0.25, "{name}: длительность {dur}, ждали {}", j.out_len());
    }

    if nvenc_works() {
        let mut j = job("v-final-nvenc");
        j.profile = Profile::Final;
        j.encoder = Encoder::Nvenc;
        j.music = Some(mref(&m.music, 4.0, true));
        j.part = Some(1);
        j.words = vec![Word { text: "тест".into(), start: 3.0, end: 3.5 }];
        run(&j).await;
        let (w, h, dur, audio) = probe(&j.out);
        assert_eq!((w, h), (1080, 1920));
        assert!((dur - 9.0).abs() < 0.25, "финал: длительность {dur}");
        assert!(audio);
    } else {
        eprintln!("NVENC недоступен — финальный профиль не проверен");
    }
}
