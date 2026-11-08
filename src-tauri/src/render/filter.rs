//! Сборка команды ffmpeg. Чистая функция: никаких запусков, поэтому её удобно тестировать.
//!
//! Раскладка кадра (W×H, по умолчанию 1080×1920):
//!   верх  — момент из исходника, высота H·split
//!   низ   — геймплей
//!   баннер — по центру линии раздела; пока он играет, верх замирает на кадре момента.

use std::path::Path;

use super::types::*;

pub struct Plan {
    pub args: Vec<String>,
    /// Длина готового ролика, секунд (для прогресса и `-t`).
    pub duration: f64,
    pub filter: String,
}

pub const FPS: u32 = 30;

fn f3(x: f64) -> String {
    format!("{x:.3}")
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32).max(1) * 2
}

/// Два уровня экранирования, как в документации ffmpeg: значение опции, затем граф фильтров.
pub fn esc(value: &str) -> String {
    let opt = value.replace('\\', "\\\\").replace(':', "\\:").replace('\'', "\\'");
    let mut out = String::with_capacity(opt.len());
    for c in opt.chars() {
        if matches!(c, '\\' | '\'' | ',' | ';' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Путь для значения опции фильтра. На Windows обратные слэши заменяются прямыми: ffmpeg принимает оба,
/// а лишний `\\` в пути пришлось бы экранировать ещё раз. Двоеточие после буквы диска `esc` экранирует сам.
fn filter_path(p: &Path, windows: bool) -> String {
    let s = p.to_string_lossy();
    esc(&if windows { s.replace('\\', "/") } else { s.into_owned() })
}

/// Верх или низ с заморозкой: на `tb` кадр замирает на `d` секунд, затем поток идёт дальше.
fn freeze_video(g: &mut Vec<String>, inp: &str, out: &str, tb: f64, d: f64) {
    g.push(format!(
        "[{inp}]split=2[{inp}_a][{inp}_b];\
         [{inp}_a]trim=end={},setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop_duration={}[{inp}_c];\
         [{inp}_b]trim=start={},setpts=PTS-STARTPTS[{inp}_d];\
         [{inp}_c][{inp}_d]concat=n=2:v=1:a=0[{out}]",
        f3(tb), f3(d), f3(tb)
    ));
}

/// Заливка кадра в область W×h: с обрезкой краёв или с размытой подложкой.
/// `Auto` сюда не приходит: его раскрывает `RenderJob::top_fit`.
fn fit(g: &mut Vec<String>, inp: &str, out: &str, w: u32, h: u32, mode: TopFit) {
    match mode {
        TopFit::Fill | TopFit::Auto => g.push(format!(
            "[{inp}]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1[{out}]"
        )),
        TopFit::Contain => g.push(format!(
            "[{inp}]split=2[{inp}_bg][{inp}_fg];\
             [{inp}_bg]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},boxblur=luma_radius=28:luma_power=2,eq=brightness=-0.12[{inp}_bgb];\
             [{inp}_fg]scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2[{inp}_fgs];\
             [{inp}_bgb][{inp}_fgs]overlay=(W-w)/2:(H-h)/2,setsar=1[{out}]"
        )),
    }
}

pub fn build(job: &RenderJob, ass_path: Option<&Path>, fonts_dir: Option<&Path>) -> Plan {
    let st = &job.style;
    let (w, h) = job.profile.size();
    let split = st.split.clamp(0.3, 0.75);
    let h_top = even(h as f64 * split);
    let h_bot = h - h_top;
    let len = job.clip_len();
    let freeze = job.freeze();
    let out_len = job.out_len();

    // --- входы ---
    let mut args: Vec<String> = ["-y", "-hide_banner", "-nostats", "-loglevel", "error", "-progress", "pipe:1"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut next = 0usize;
    let mut add = |args: &mut Vec<String>, pre: Vec<String>, path: &Path| -> usize {
        args.extend(pre);
        args.push("-i".into());
        args.push(path.to_string_lossy().into_owned());
        next += 1;
        next - 1
    };

    let i_src = add(&mut args, vec!["-ss".into(), f3(job.start), "-t".into(), f3(len)], &job.source.path);
    let i_ban = freeze.and_then(|(_, d)| {
        job.banner
            .as_ref()
            .map(|b| add(&mut args, vec!["-t".into(), f3(d)], &b.path))
    });
    let i_gp = add(
        &mut args,
        vec!["-stream_loop".into(), "-1".into(), "-ss".into(), f3(job.gameplay_offset)],
        &job.gameplay.path,
    );
    let i_mus = job.music.as_ref().filter(|_| st.audio.music > 0.0).map(|m| {
        add(&mut args, vec!["-stream_loop".into(), "-1".into(), "-ss".into(), f3(job.music_offset)], &m.path)
    });

    // --- видео ---
    let mut g: Vec<String> = Vec::new();

    // нижняя полоса исходника (там обычно вшитые субтитры) срезается до вписывания в верхнюю половину
    let crop = st.top_crop_bottom.clamp(0.0, 0.4);
    let cut = if crop > 0.001 { format!(",crop=iw:trunc(ih*{}/2)*2:0:0", f3(1.0 - crop)) } else { String::new() };
    g.push(format!("[{i_src}:v]fps={FPS},setpts=PTS-STARTPTS,setsar=1{cut}[src]"));
    fit(&mut g, "src", "top0", w, h_top, job.top_fit());
    match freeze {
        Some((tb, d)) => freeze_video(&mut g, "top0", "top", tb, d),
        None => g.push("[top0]null[top]".into()),
    }

    g.push(format!("[{i_gp}:v]fps={FPS},setpts=PTS-STARTPTS,setsar=1[gp]"));
    fit(&mut g, "gp", "bot0", w, h_bot, TopFit::Fill);
    match freeze {
        Some((tb, d)) if st.gameplay_freeze => freeze_video(&mut g, "bot0", "bot", tb, d),
        _ => g.push("[bot0]null[bot]".into()),
    }
    g.push("[top][bot]vstack=inputs=2[stack]".into());

    let mut vlabel = "stack".to_string();
    if let (Some(ib), Some((tb, d))) = (i_ban, freeze) {
        let b = &st.banner;
        let bw = even(w as f64 * b.width_pct.clamp(0.1, 1.0));
        let key = b.key_color.trim_start_matches('#');
        let mut chain = format!(
            "[{ib}:v]fps={FPS},setpts=PTS-STARTPTS,chromakey=color=0x{key}:similarity={}:blend={}",
            f3(b.similarity), f3(b.blend)
        );
        if b.despill {
            chain.push_str(",despill=type=green:mix=0.6:expand=0");
        }
        match b.anim {
            // Рост от 60% до 100% за 0.25 с с замедлением в конце.
            BannerAnim::Pop => chain.push_str(&format!(
                ",scale=w='trunc({bw}*(0.6+0.4*(1-pow(1-min(1,t/0.25),3)))/2)*2':h=-2:eval=frame"
            )),
            _ => chain.push_str(&format!(",scale={bw}:-2")),
        }
        if matches!(b.anim, BannerAnim::Fade | BannerAnim::Pop) {
            let fo = (d - 0.2).max(0.0);
            chain.push_str(&format!(
                ",fade=t=in:st=0:d=0.2:alpha=1,fade=t=out:st={}:d=0.2:alpha=1",
                f3(fo)
            ));
        }
        chain.push_str(&format!(",setpts=PTS-STARTPTS+{}/TB[ban]", f3(tb)));
        g.push(chain);

        let ox = (w as f64 * b.offset_x).round() as i64;
        let oy = (h as f64 * b.offset_y).round() as i64;
        let slide = if b.anim == BannerAnim::SlideUp {
            format!("+pow(max(0,1-(t-{})/0.3),2)*{}", f3(tb), (h as f64 * 0.08).round())
        } else {
            String::new()
        };
        g.push(format!(
            "[stack][ban]overlay=x='(W-overlay_w)/2+({ox})':y='{h_top}-overlay_h/2+({oy}){slide}':\
             eof_action=pass:enable='between(t,{},{})'[ov]",
            f3(tb), f3(tb + d)
        ));
        vlabel = "ov".into();
    }

    match ass_path {
        Some(p) => {
            let mut f = format!("[{vlabel}]ass=filename={}", filter_path(p, cfg!(windows)));
            if let Some(fd) = fonts_dir {
                f.push_str(&format!(":fontsdir={}", filter_path(fd, cfg!(windows))));
            }
            f.push_str(",format=yuv420p[vout]");
            g.push(f);
        }
        None => g.push(format!("[{vlabel}]format=yuv420p[vout]")),
    }

    // --- звук ---
    let a = &st.audio;
    let stereo = "aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo";
    let mut mix: Vec<String> = Vec::new();

    if job.source.has_audio && a.main > 0.0 {
        match freeze {
            // Пока играет баннер, звук исходника заменяется тишиной.
            Some((tb, d)) => g.push(format!(
                "[{i_src}:a]{stereo},volume={m},asplit=2[sa][sb];\
                 [sa]atrim=end={},asetpts=PTS-STARTPTS,apad=pad_dur={}[sc];\
                 [sb]atrim=start={},asetpts=PTS-STARTPTS[sd];\
                 [sc][sd]concat=n=2:v=0:a=1[a_main]",
                f3(tb), f3(d), f3(tb), m = f3(a.main)
            )),
            None => g.push(format!("[{i_src}:a]{stereo},volume={}[a_main]", f3(a.main))),
        }
        mix.push("a_main".into());
    }
    if let (Some(ib), Some((tb, _))) = (i_ban, freeze) {
        if job.banner.as_ref().is_some_and(|b| b.has_audio) && a.banner > 0.0 {
            let ms = (tb * 1000.0).round() as i64;
            g.push(format!("[{ib}:a]{stereo},volume={},adelay=delays={ms}:all=1[a_ban]", f3(a.banner)));
            mix.push("a_ban".into());
        }
    }
    if job.gameplay.has_audio && a.gameplay > 0.0 {
        g.push(format!("[{i_gp}:a]{stereo},volume={}[a_gp]", f3(a.gameplay)));
        mix.push("a_gp".into());
    }
    if let Some(im) = i_mus {
        let fade_at = (out_len - 1.5).max(0.0);
        g.push(format!(
            "[{im}:a]{stereo},volume={},afade=t=out:st={}:d=1.5[a_mus]",
            f3(a.music), f3(fade_at)
        ));
        mix.push("a_mus".into());
    }

    let normalize = a.loudnorm && job.profile == Profile::Final;
    let has_audio = !mix.is_empty();
    if has_audio {
        let tail = if normalize { ",loudnorm=I=-14:TP=-1.5:LRA=11" } else { "" };
        if mix.len() == 1 {
            g.push(format!("[{}]anull{tail}[aout]", mix[0]));
        } else {
            let ins: String = mix.iter().map(|m| format!("[{m}]")).collect();
            g.push(format!("{ins}amix=inputs={}:duration=longest:normalize=0{tail}[aout]", mix.len()));
        }
    }

    let filter = g.join(";");
    args.push("-filter_complex".into());
    args.push(filter.clone());
    args.extend(["-map", "[vout]"].map(String::from));
    if has_audio {
        args.extend(["-map", "[aout]"].map(String::from));
    }

    // --- кодирование ---
    let draft = job.profile == Profile::Draft;
    let v: &[&str] = match (job.encoder, draft) {
        (Encoder::Nvenc, true) => &["-c:v", "h264_nvenc", "-preset", "p1", "-rc", "vbr", "-cq", "30", "-b:v", "2500k"],
        (Encoder::Nvenc, false) => &[
            "-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "21", "-b:v", "10M", "-maxrate", "14M",
            "-bufsize", "20M", "-profile:v", "high",
        ],
        (Encoder::X264, true) => &["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30"],
        (Encoder::X264, false) => &["-c:v", "libx264", "-preset", "medium", "-crf", "19", "-maxrate", "14M", "-bufsize", "20M"],
    };
    args.extend(v.iter().map(|s| s.to_string()));
    args.extend(["-pix_fmt", "yuv420p", "-r", "30", "-g", "60"].map(String::from));
    if has_audio {
        args.extend(["-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2"].map(String::from));
    }
    args.extend(["-movflags", "+faststart", "-t"].map(String::from));
    args.push(f3(out_len));
    args.push(job.out.to_string_lossy().into_owned());

    Plan { args, duration: out_len, filter }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn media(name: &str, dur: f64, audio: bool) -> MediaRef {
        MediaRef { path: PathBuf::from(name), duration: dur, has_audio: audio }
    }

    fn job() -> RenderJob {
        RenderJob {
            source: media("/v/src.mp4", 600.0, true),
            source_aspect: 16.0 / 9.0,
            start: 100.0,
            end: 130.0,
            banner: Some(media("/v/banner.mp4", 4.0, true)),
            banner_at: Some(12.0),
            gameplay: media("/v/gp.mp4", 300.0, false),
            gameplay_offset: 20.0,
            music: None,
            music_offset: 0.0,
            words: vec![],
            part: Some(1),
            style: Style::default(),
            profile: Profile::Final,
            encoder: Encoder::X264,
            out: PathBuf::from("/o/out.mp4"),
        }
    }

    #[test]
    fn duration_is_clip_plus_banner() {
        let p = build(&job(), None, None);
        assert!((p.duration - 34.0).abs() < 1e-9);
        assert!(p.args.windows(2).any(|w| w[0] == "-t" && w[1] == "34.000"));
    }

    #[test]
    fn source_bottom_is_cut_before_fitting_only_when_asked() {
        let mut j = job();
        assert!(!build(&j, None, None).filter.contains("ih*"), "по умолчанию кадр виден целиком");
        j.style.top_crop_bottom = 0.2;
        let p = build(&j, None, None);
        assert!(p.filter.contains("setsar=1,crop=iw:trunc(ih*0.800/2)*2:0:0[src]"), "{}", p.filter);
    }

    #[test]
    fn wide_source_is_fitted_whole_over_a_blurred_copy_by_default() {
        let f = build(&job(), None, None).filter;
        assert!(f.contains("scale=1080:960:force_original_aspect_ratio=decrease:force_divisible_by=2"), "{f}");
        assert!(f.contains("boxblur") && f.contains("overlay=(W-w)/2:(H-h)/2"), "{f}");
    }

    #[test]
    fn vertical_square_and_unknown_sources_still_fill_the_zone() {
        for aspect in [9.0 / 16.0, 1.0, 0.0] {
            let mut j = job();
            j.source_aspect = aspect;
            assert_eq!(j.top_fit(), TopFit::Fill, "aspect {aspect}");
            assert!(!build(&j, None, None).filter.contains("boxblur"), "aspect {aspect}");
        }
    }

    #[test]
    fn auto_looks_at_the_shape_left_after_the_bottom_cut() {
        let mut j = job();
        j.source_aspect = 1.0;
        assert_eq!(j.top_fit(), TopFit::Fill);
        j.style.top_crop_bottom = 0.2; // 1,0 / 0,8 = 1,25: теперь кадр широкий
        assert_eq!(j.top_fit(), TopFit::Contain);
    }

    #[test]
    fn explicit_fit_overrides_the_shape_of_the_source() {
        let mut j = job();
        j.style.top_fit = TopFit::Fill;
        assert!(!build(&j, None, None).filter.contains("boxblur"));
        j.source_aspect = 9.0 / 16.0;
        j.style.top_fit = TopFit::Contain;
        assert!(build(&j, None, None).filter.contains("boxblur"));
    }

    #[test]
    fn banner_freezes_top_only_by_default() {
        let p = build(&job(), None, None);
        assert_eq!(p.filter.matches("tpad=stop_mode=clone:stop_duration=4.000").count(), 1);
        assert!(p.filter.contains("trim=end=12.000"));
        assert!(p.filter.contains("overlay=x="));
        assert!(p.filter.contains("between(t,12.000,16.000)"));
        assert!(p.filter.contains("adelay=delays=12000:all=1"));
    }

    #[test]
    fn gameplay_freezes_when_enabled() {
        let mut j = job();
        j.style.gameplay_freeze = true;
        let p = build(&j, None, None);
        assert_eq!(p.filter.matches("tpad=").count(), 2);
    }

    #[test]
    fn no_banner_means_no_freeze_and_no_overlay() {
        let mut j = job();
        j.banner = None;
        let p = build(&j, None, None);
        assert!((p.duration - 30.0).abs() < 1e-9);
        assert!(!p.filter.contains("tpad"));
        assert!(!p.filter.contains("overlay=x="), "наложения баннера нет");
        assert!(!p.filter.contains("chromakey"));
    }

    #[test]
    fn disabled_banner_style_is_ignored() {
        let mut j = job();
        j.style.banner.enabled = false;
        assert!(build(&j, None, None).filter.find("chromakey").is_none());
    }

    #[test]
    fn split_gives_even_heights_that_sum_to_frame() {
        for split in [0.4, 0.45, 0.5, 0.55, 0.6, 0.7] {
            let mut j = job();
            j.style.split = split;
            let top = even(1920.0 * split);
            assert_eq!(top % 2, 0, "split {split}");
            let f = build(&j, None, None).filter;
            assert!(f.contains(&format!("crop=1080:{top}")), "верх, split {split}");
            assert!(f.contains(&format!("crop=1080:{}", 1920 - top)), "низ, split {split}");
        }
    }

    #[test]
    fn draft_is_half_size_and_skips_loudnorm() {
        let mut j = job();
        j.profile = Profile::Draft;
        let p = build(&j, None, None);
        assert!(p.filter.contains("crop=540:480"));
        assert!(!p.filter.contains("loudnorm"));
        assert!(build(&job(), None, None).filter.contains("loudnorm"));
    }

    #[test]
    fn music_adds_input_and_fade() {
        let mut j = job();
        j.music = Some(media("/v/m.mp3", 120.0, true));
        let p = build(&j, None, None);
        assert!(p.filter.contains("afade=t=out:st=32.500:d=1.5"));
        assert!(p.args.iter().filter(|a| *a == "-i").count() == 4);
    }

    #[test]
    fn banner_longer_than_limit_is_capped() {
        let mut j = job();
        j.style.banner.max_seconds = Some(2.5);
        assert_eq!(j.freeze(), Some((12.0, 2.5)));
    }

    #[test]
    fn banner_time_is_kept_inside_clip() {
        let mut j = job();
        j.banner_at = Some(999.0);
        let (tb, _) = j.freeze().unwrap();
        assert!(tb < j.clip_len());
    }

    #[test]
    fn escaping_leaves_plain_paths_alone() {
        assert_eq!(esc("/home/u/.local/share/app/tmp/job-1.ass"), "/home/u/.local/share/app/tmp/job-1.ass");
        assert_eq!(esc("/a:b"), "/a\\\\:b");
        assert_eq!(esc("/a,b[1]"), "/a\\,b\\[1\\]");
    }

    #[test]
    fn windows_paths_use_forward_slashes_and_an_escaped_drive_colon() {
        let p = Path::new(r"C:\Users\Пример Имени\AppData\Local\app\tmp\job-1.ass");
        assert_eq!(filter_path(p, true), "C\\\\:/Users/Пример Имени/AppData/Local/app/tmp/job-1.ass");
        // на Linux обратный слэш — обычный символ имени и экранируется как есть
        assert_eq!(filter_path(Path::new(r"/a\b"), false), "/a\\\\\\\\b");
    }
}
