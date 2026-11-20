//! Кольцо сегментов эфира: запись HLS без перекодирования и вырезание окна без перекодирования.
//! Чистая часть: аргументы ffmpeg, разбор плейлиста, выбор сегментов под окно. Запуск процессов — в `engine.rs`.
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub const SEG_SECONDS: u32 = 4;
/// ≈ 20 минут кольца: хватает, чтобы пережить задержку принятия решения и долгий пик.
pub const RING_SEGMENTS: u32 = 300;

/// Запись входного потока в кольцо. `input` — адрес HLS из yt-dlp (или файл для тестов с `-re`).
pub fn record_args(input: &str, dir: &Path, realtime_file: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "warning".into()];
    if realtime_file {
        a.push("-re".into());
    } else {
        // обрыв сети не должен убивать запись: ffmpeg сам переподключается
        a.extend(["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "10"].map(String::from));
    }
    a.extend(["-i".into(), input.into(), "-map".into(), "0:v:0".into(), "-map".into(), "0:a:0?".into(), "-c".into(), "copy".into(), "-f".into(), "hls".into()]);
    a.extend(["-hls_time".into(), SEG_SECONDS.to_string(), "-hls_list_size".into(), RING_SEGMENTS.to_string()]);
    a.extend(["-hls_flags".into(), "delete_segments+program_date_time+independent_segments".into()]);
    a.extend(["-hls_segment_filename".into(), dir.join("seg_%08d.ts").to_string_lossy().into_owned()]);
    a.push(dir.join("ring.m3u8").to_string_lossy().into_owned());
    a
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub path: PathBuf,
    /// Секунды по часам (unix), из `EXT-X-PROGRAM-DATE-TIME`.
    pub start: f64,
    pub dur: f64,
}
impl Seg {
    pub fn end(&self) -> f64 {
        self.start + self.dur
    }
}

fn parse_pdt(s: &str) -> Option<f64> {
    // 2026-11-19T10:00:04.123+0000 или с двоеточием в смещении
    let s = s.trim();
    chrono::DateTime::parse_from_rfc3339(s).or_else(|_| chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z")).ok().map(|d| d.timestamp_millis() as f64 / 1000.0)
}

/// Плейлист ffmpeg → сегменты с временем по часам. Сегмент без своей метки получает конец предыдущего.
pub fn parse_playlist(text: &str, dir: &Path) -> Vec<Seg> {
    parse_playlist_with_segment_length(text, dir, SEG_SECONDS)
}

pub(crate) fn parse_playlist_with_segment_length(text: &str, dir: &Path, seg_s: u32) -> Vec<Seg> {
    let max_dur = 30.0_f64.max(3.0 * f64::from(seg_s));
    let mut out = Vec::new();
    let mut dur: Option<f64> = None;
    let mut pdt: Option<f64> = None;
    let mut cursor: Option<f64> = None;
    for line in text.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("#EXTINF:") {
            dur = v.split(',').next().and_then(|d| d.parse().ok());
        } else if let Some(v) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            pdt = parse_pdt(v);
        } else if !line.is_empty() && !line.starts_with('#') {
            let start = pdt.take().or(cursor.take());
            if let (Some(d), Some(start), Some(name)) = (dur.take().filter(|&d| d > 0.0 && d <= max_dur), start, Path::new(line).file_name()) {
                out.push(Seg { path: dir.join(name), start, dur: d });
                cursor = Some(start + d);
            }
        }
    }
    out
}

/// Сегменты под окно `[t0, t1]` из последней непрерывной серии и смещение внутри первого сегмента.
pub fn select(segs: &[Seg], t0: f64, t1: f64) -> Result<(Vec<&Seg>, f64)> {
    if !(t1 > t0) {
        bail!("Пустое окно");
    }
    let run_start = segs.windows(2).rposition(|pair| {
        let (prev, next) = (&pair[0], &pair[1]);
        (next.start - prev.end()).abs() > 1.0 || next.end() <= prev.end()
    }).map_or(0, |i| i + 1);
    let segs = &segs[run_start..];
    let first = segs.first().context("Кольцо пусто")?;
    let last = segs.last().context("Кольцо пусто")?;
    // Начало успело выпасть из кольца: режем с самого раннего, но не короче половины окна.
    let from = t0.max(first.start);
    let to = t1.min(last.end());
    if to - from < (t1 - t0) / 2.0 {
        bail!("Окно момента почти целиком вне кольца ({:.0}–{:.0}, в кольце {:.0}–{:.0})", t0, t1, first.start, last.end());
    }
    let picked: Vec<&Seg> = segs.iter().filter(|s| s.end() > from && s.start < to).collect();
    let offset = (from - picked.first().context("Нет сегментов под окно")?.start).max(0.0);
    Ok((picked, offset))
}

/// Зависание: истекло время и от запуска записи, и от последнего продвижения кольца.
pub fn is_stalled(since_start: f64, since_progress: f64, stall_s: f64) -> bool {
    since_start >= stall_s && since_progress >= stall_s
}

/// Конкатенация сегментов без перекодирования; `-ss` после `-i` режет до ключевого кадра сегмента.
pub fn cut_args(list_file: &Path, offset: f64, duration: f64, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(), "-loglevel".into(), "error".into(), "-y".into(),
        "-f".into(), "concat".into(), "-safe".into(), "0".into(),
        "-i".into(), list_file.to_string_lossy().into_owned(),
        "-ss".into(), format!("{offset:.3}"), "-t".into(), format!("{duration:.3}"),
        "-c".into(), "copy".into(), "-avoid_negative_ts".into(), "make_zero".into(),
        "-movflags".into(), "+faststart".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Файл списка для concat-демультиплексора; кавычки в путях экранируются.
pub fn concat_list(segs: &[&Seg]) -> String {
    segs.iter().map(|s| format!("file '{}'\n", s.path.to_string_lossy().replace('\'', "'\\''"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PL: &str = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:10\n\
#EXT-X-PROGRAM-DATE-TIME:2026-11-19T10:00:00.000+0000\n#EXTINF:4.000000,\nseg_00000010.ts\n\
#EXTINF:4.000000,\nseg_00000011.ts\n\
#EXT-X-PROGRAM-DATE-TIME:2026-11-19T10:00:08.500+0000\n#EXTINF:3.500000,\nseg_00000012.ts\n\
#EXTINF:4.000000,\n../../etc/seg_00000013.ts\n";

    #[test]
    fn parses_times_and_ignores_foreign_paths() {
        let d = Path::new("/ring");
        let s = parse_playlist(PL, d);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].path, Path::new("/ring/seg_00000010.ts"));
        let t0 = s[0].start;
        assert_eq!(s[1].start, t0 + 4.0, "метка не указана: конец предыдущего");
        assert_eq!(s[2].start, t0 + 8.5, "метка есть: берём её");
        assert_eq!(s[3].path, Path::new("/ring/seg_00000013.ts"), "только имя файла, без ../");
    }

    #[test]
    fn rejects_invalid_durations_and_does_not_infer_time_through_them() {
        let playlist = "#EXT-X-PROGRAM-DATE-TIME:2026-11-19T10:00:00Z\n#EXTINF:4,\nfirst.ts\n\
                        #EXTINF:6056.032667,\nlong.ts\n#EXTINF:-4,\nnegative.ts\n#EXTINF:0,\nzero.ts\n\
                        #EXTINF:NaN,\nnan.ts\n#EXTINF:4,\nunknown_time.ts\n\
                        #EXT-X-PROGRAM-DATE-TIME:2026-11-19T10:00:08Z\n#EXTINF:4,\nlast.ts\n";
        let segs = parse_playlist(playlist, Path::new("/ring"));
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].path, Path::new("/ring/first.ts"));
        assert_eq!(segs[1].path, Path::new("/ring/last.ts"));
        assert_eq!(segs[1].start - segs[0].start, 8.0);
    }

    #[test]
    fn duration_limit_uses_configured_segment_length() {
        let playlist = "#EXT-X-PROGRAM-DATE-TIME:2026-11-19T10:00:00Z\n#EXTINF:36,\nvalid.ts\n#EXTINF:37,\nlong.ts\n";
        let dir = Path::new("/ring");
        assert!(parse_playlist(playlist, dir).is_empty());
        let segs = parse_playlist_with_segment_length(playlist, dir, 12);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].dur, 36.0);
    }

    #[test]
    fn selects_covering_segments_and_offset() {
        let s = parse_playlist(PL, Path::new("/r"));
        let t0 = s[0].start;
        let (picked, off) = select(&s, t0 + 5.0, t0 + 9.0).unwrap();
        assert_eq!(picked.len(), 2, "второй и третий сегменты");
        assert_eq!(picked[0].path, Path::new("/r/seg_00000011.ts"));
        assert!((off - 1.0).abs() < 1e-9);
    }

    #[test]
    fn window_partly_before_ring_is_clamped_but_fully_outside_fails() {
        let s = parse_playlist(PL, Path::new("/r"));
        let t0 = s[0].start;
        let (picked, off) = select(&s, t0 - 3.0, t0 + 9.0).unwrap();
        assert_eq!((picked.len(), off), (3, 0.0));
        assert!(select(&s, t0 - 100.0, t0 - 50.0).is_err());
        assert!(select(&s, t0 + 4.0, t0 + 4.0).is_err());
        assert!(select(&[], 0.0, 10.0).is_err());
    }

    #[test]
    fn selects_only_the_newest_continuous_run() {
        let mut segs = parse_playlist(PL, Path::new("/ring"));
        let start = segs[0].start;
        segs.push(Seg { path: "/ring/new.ts".into(), start: start + 2.0, dur: 4.0 });
        segs.push(Seg { path: "/ring/new_next.ts".into(), start: start + 6.0, dur: 4.0 });

        let (picked, offset) = select(&segs, start + 3.0, start + 9.0).unwrap();
        assert_eq!(picked, [&segs[4], &segs[5]]);
        assert_eq!(offset, 1.0);
        assert!(select(&segs, start + 10.0, start + 14.0).is_err());

        segs.push(Seg { path: "/ring/after_gap.ts".into(), start: start + 16.0, dur: 4.0 });
        assert!(select(&segs, start + 8.0, start + 18.0).is_err());
        let (picked, _) = select(&segs, start + 16.0, start + 20.0).unwrap();
        assert_eq!(picked, [&segs[6]]);
    }

    #[test]
    fn stall_requires_both_startup_grace_and_no_progress() {
        assert!(!is_stalled(44.0, 44.0, 45.0));
        assert!(is_stalled(45.0, 45.0, 45.0));
        assert!(!is_stalled(120.0, 44.0, 45.0));
        assert!(is_stalled(121.0, 45.0, 45.0));
    }

    #[test]
    fn record_args_copy_without_reencode() {
        let args = record_args("https://x/live.m3u8", Path::new("/ring"), false);
        let maps: Vec<_> = args.windows(2).filter(|pair| pair[0] == "-map").map(|pair| pair[1].as_str()).collect();
        assert_eq!(maps, ["0:v:0", "0:a:0?"]);
        let a = args.join(" ");
        assert!(a.contains("-c copy") && a.contains("-f hls") && a.contains("delete_segments"));
        assert!(a.contains("-reconnect 1") && !a.contains("-re "));
        let f = record_args("/tmp/t.mp4", Path::new("/ring"), true).join(" ");
        assert!(f.contains("-re ") && !f.contains("-reconnect"));
    }

    #[test]
    fn concat_list_escapes_quotes() {
        let s = Seg { path: PathBuf::from("/a/it's.ts"), start: 0.0, dur: 4.0 };
        assert_eq!(concat_list(&[&s]), "file '/a/it'\\''s.ts'\n");
    }
}
