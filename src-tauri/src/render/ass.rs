//! Субтитры по словам (с подсветкой текущего) и надпись «Часть N» в формате ASS.

use super::types::{RenderJob, Word};

const PLAY_W: u32 = 1080;
const PLAY_H: u32 = 1920;

/// `RRGGBB` → `&H00BBGGRR` (в ASS порядок каналов обратный).
fn color(rgb: &str) -> String {
    let c: String = rgb.trim_start_matches('#').chars().filter(char::is_ascii_hexdigit).collect();
    let c = if c.len() == 6 { c } else { "ffffff".to_string() };
    format!("&H00{}{}{}", &c[4..6], &c[2..4], &c[0..2]).to_uppercase()
}

fn ts(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '{' | '}' | '\\' | '\n' | '\r')).collect()
}

/// Переносит интервал из времени исходника во время ролика. Пока играет баннер, субтитров нет:
/// интервал, который накрывает заморозку, делится на две части.
pub fn map_interval(s: f64, e: f64, clip_start: f64, clip_len: f64, freeze: Option<(f64, f64)>) -> Vec<(f64, f64)> {
    let (s, e) = ((s - clip_start).max(0.0), (e - clip_start).min(clip_len));
    if e - s < 0.02 {
        return vec![];
    }
    let Some((tb, d)) = freeze else { return vec![(s, e)] };
    let mut out = Vec::new();
    if s < tb {
        out.push((s, e.min(tb)));
    }
    if e > tb {
        out.push((s.max(tb) + d, e + d));
    }
    out.retain(|(a, b)| b - a >= 0.02);
    out
}

struct Line<'a> {
    words: &'a [Word],
}

fn group<'a>(words: &'a [Word], per_line: usize) -> Vec<Line<'a>> {
    let per_line = per_line.max(1);
    let mut lines = Vec::new();
    let mut from = 0;
    for i in 0..words.len() {
        let n = i + 1 - from;
        let end_of_sentence = words[i].text.ends_with(['.', '!', '?', '…']);
        let long_pause = words.get(i + 1).is_some_and(|w| w.start - words[i].end > 0.9);
        if n >= per_line || end_of_sentence || long_pause || i + 1 == words.len() {
            lines.push(Line { words: &words[from..=i] });
            from = i + 1;
        }
    }
    lines
}

/// Обрезает каждое событие по началу следующего: в любой момент видна одна строка субтитров.
/// События с одинаковым началом схлопываются в последнее, слишком короткие остатки отбрасываются.
fn non_overlapping(mut events: Vec<(f64, f64, String)>) -> Vec<(f64, f64, String)> {
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let starts: Vec<f64> = events.iter().map(|e| e.0).collect();
    let mut out = Vec::with_capacity(events.len());
    for (i, (from, to, text)) in events.into_iter().enumerate() {
        let to = starts.get(i + 1).map_or(to, |next| to.min(*next));
        if to - from >= 0.02 {
            out.push((from, to, text));
        }
    }
    out
}

pub fn build(job: &RenderJob) -> String {
    let st = &job.style;
    let (clip_start, clip_len) = (job.start, job.clip_len());
    let freeze = job.freeze();
    let out_len = job.out_len();
    let (_, h) = job.profile.size();
    let split_y = (PLAY_H as f64 * st.split).round().min(h as f64 * 10.0);

    let sub = &st.subtitles;
    let part = &st.part_label;
    let mut s = String::new();
    s.push_str(&format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {PLAY_W}\nPlayResY: {PLAY_H}\nWrapStyle: 2\nScaledBorderAndShadow: yes\n\n"
    ));
    s.push_str("[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    s.push_str(&format!(
        "Style: Sub,{},{},{},{},{},&H00000000,0,0,0,0,100,100,0,0,1,{},0,5,60,60,60,1\n",
        clean(&sub.font), sub.size, color(&sub.color), color(&sub.color), color(&sub.outline_color), sub.outline
    ));
    s.push_str(&format!(
        "Style: Part,{},{},{},{},{},&H00000000,0,0,0,0,100,100,0,0,1,{},0,5,60,60,60,1\n\n",
        clean(&part.font), part.size, color(&part.color), color(&part.color), color(&part.outline_color), part.outline
    ));
    s.push_str("[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");

    if part.enabled {
        if let Some(n) = job.part {
            let text = clean(&part.template.replace("{n}", &n.to_string()));
            let y = (PLAY_H as f64 * part.y_pct).round();
            s.push_str(&format!(
                "Dialogue: 1,{},{},Part,,0,0,0,,{{\\an5\\pos({},{})}}{}\n",
                ts(0.0), ts(out_len), PLAY_W / 2, y, text
            ));
        }
    }

    if sub.enabled {
        let words: Vec<Word> = job
            .words
            .iter()
            .filter(|w| w.end > clip_start && w.start < job.end && !w.text.trim().is_empty())
            .cloned()
            .collect();
        let y = sub.y_pct.map_or(split_y, |p| (PLAY_H as f64 * p).round());
        let hi = color(&sub.highlight);
        let base = color(&sub.color);
        let scale = sub.highlight_scale;

        // Сначала события всех слов по порядку, потом обрезка: у Whisper слова бывают внахлёст, а хвост последнего слова строки
        // (+0.25 с) заходит на начало следующей. Два события в одно время рисуются друг поверх друга: субтитры «двоятся».
        let mut events: Vec<(f64, f64, String)> = Vec::new();
        for line in group(&words, sub.words_per_line) {
            let ws = line.words;
            for (k, active) in ws.iter().enumerate() {
                // Событие слова длится до начала следующего, чтобы строка не мигала в паузах.
                let from = active.start;
                let to = ws.get(k + 1).map_or(active.end + 0.25, |n| n.start.max(active.end));
                let text = ws
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let t = clean(w.text.trim());
                        let t = if sub.uppercase { t.to_uppercase() } else { t };
                        if i == k {
                            format!("{{\\1c{hi}\\fscx{scale}\\fscy{scale}}}{t}{{\\1c{base}\\fscx100\\fscy100}}")
                        } else {
                            t
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                events.push((from, to, text));
            }
        }
        for (from, to, text) in non_overlapping(events) {
            for (a, b) in map_interval(from, to, clip_start, clip_len, freeze) {
                s.push_str(&format!(
                    "Dialogue: 0,{},{},Sub,,0,0,0,,{{\\an5\\pos({},{})}}{}\n",
                    ts(a), ts(b), PLAY_W / 2, y, text
                ));
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_is_bgr() {
        assert_eq!(color("b6ff3c"), "&H003CFFB6");
        assert_eq!(color("#FF0000"), "&H000000FF");
        assert_eq!(color("zzz"), "&H00FFFFFF");
    }

    fn label_job(part: Option<u32>) -> RenderJob {
        use crate::render::{Encoder, MediaRef, Profile, RenderJob, Style};
        let media = |name: &str| MediaRef { path: std::path::PathBuf::from(name), duration: 60.0, has_audio: true };
        RenderJob {
            decode: Default::default(),
            source: media("/v/src.mp4"),
            source_aspect: 16.0 / 9.0,
            start: 0.0,
            end: 20.0,
            banner: None,
            banner_at: None,
            gameplay: media("/v/gp.mp4"),
            gameplay_offset: 0.0,
            music: None,
            music_offset: 0.0,
            words: vec![],
            part,
            style: Style::default(),
            profile: Profile::Final,
            encoder: Encoder::X264,
            out: std::path::PathBuf::from("/o/out.mp4"),
        }
    }

    /// Надпись рисуется только при номере части: пульный вариант собирается без номера и без надписи.
    #[test]
    fn the_part_label_needs_a_part_number() {
        assert!(build(&label_job(Some(2))).contains("Часть 2"));
        assert!(!build(&label_job(None)).contains("Часть"));
    }

    #[test]
    fn timestamps() {
        assert_eq!(ts(0.0), "0:00:00.00");
        assert_eq!(ts(61.5), "0:01:01.50");
        assert_eq!(ts(3723.456), "1:02:03.46");
    }

    #[test]
    fn interval_without_freeze_is_shifted_to_clip_time() {
        assert_eq!(map_interval(12.0, 13.0, 10.0, 20.0, None), vec![(2.0, 3.0)]);
        // за границами клипа обрезается
        assert_eq!(map_interval(9.0, 11.0, 10.0, 20.0, None), vec![(0.0, 1.0)]);
        assert!(map_interval(31.0, 32.0, 10.0, 20.0, None).is_empty());
    }

    #[test]
    fn words_after_freeze_are_delayed_by_banner_length() {
        // клип с 10 с, заморозка на 5-й секунде клипа на 3 с
        let f = Some((5.0, 3.0));
        assert_eq!(map_interval(12.0, 13.0, 10.0, 20.0, f), vec![(2.0, 3.0)]);
        assert_eq!(map_interval(16.0, 17.0, 10.0, 20.0, f), vec![(9.0, 10.0)]);
    }

    #[test]
    fn interval_over_freeze_is_split_and_hidden_during_banner() {
        let parts = map_interval(14.0, 16.0, 10.0, 20.0, Some((5.0, 3.0)));
        assert_eq!(parts, vec![(4.0, 5.0), (8.0, 9.0)]);
        // ни одна часть не попадает в окно баннера 5..8
        assert!(parts.iter().all(|(a, b)| *b <= 5.0 || *a >= 8.0));
    }

    #[test]
    fn subtitle_events_never_overlap() {
        let ev = vec![
            (1.0, 2.5, "a".to_string()),
            (2.0, 3.0, "b".to_string()), // начинается раньше конца «a»
            (3.1, 3.35, "c".to_string()),
            (3.2, 4.0, "d".to_string()), // хвост «c» заходит на «d»
        ];
        let out = non_overlapping(ev);
        assert_eq!(out.iter().map(|e| e.2.as_str()).collect::<Vec<_>>(), ["a", "b", "c", "d"]);
        assert!(out.windows(2).all(|w| w[0].1 <= w[1].0 + 1e-9));
        assert_eq!(out[0].1, 2.0);
        assert_eq!(out[2].1, 3.2);
    }
}
