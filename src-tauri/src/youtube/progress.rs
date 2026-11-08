//! Строки прогресса yt-dlp и подсчёт общего хода загрузки. Порт `ProgressTracker` и `stream_label` из утилиты.
//!
//! yt-dlp печатает наши строки по шаблонам из `engine` (`AVTO|D|…` — ход потока, `AVTO|P|…` — этап после
//! скачивания, `AVTO|F|…` — итоговый файл). Недостающие значения он пишет как `NA`: они остаются пустыми,
//! ничего не придумывается.

use std::{collections::HashMap, path::PathBuf};

use serde::Serialize;

/// Ход одного потока (видео, аудио или готовый файл), как его прислал yt-dlp.
#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    pub status: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub total_estimate: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
    pub format_id: String,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub abr: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Download(Hook),
    /// Этап после скачивания: `Merger` (склейка), `MoveFiles` (перенос в папку) и другие.
    Post { status: String, name: String },
    File(PathBuf),
    Other(String),
}

fn field(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty() && s != "NA" && s != "None").then_some(s)
}
fn num(s: &str) -> Option<f64> {
    field(s)?.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

pub fn parse_line(line: &str) -> Line {
    let Some(rest) = line.strip_prefix("AVTO|") else { return Line::Other(line.to_string()) };
    if let Some(path) = rest.strip_prefix("F|") {
        return Line::File(PathBuf::from(path));
    }
    if let Some(body) = rest.strip_prefix("P|") {
        let mut it = body.split('|');
        return Line::Post { status: it.next().unwrap_or("").to_string(), name: it.next().unwrap_or("").to_string() };
    }
    if let Some(body) = rest.strip_prefix("D|") {
        let f: Vec<&str> = body.split('|').collect();
        if f.len() >= 12 {
            return Line::Download(Hook {
                status: f[0].to_string(),
                downloaded: num(f[1]).unwrap_or(0.0) as u64,
                total: num(f[2]).map(|v| v as u64),
                total_estimate: num(f[3]).map(|v| v as u64),
                speed: num(f[4]).filter(|v| *v > 0.0),
                eta: num(f[5]).map(|v| v as u64),
                format_id: field(f[6]).unwrap_or("stream").to_string(),
                vcodec: field(f[7]).map(str::to_owned),
                acodec: field(f[8]).map(str::to_owned),
                height: num(f[9]).map(|v| v as u32),
                fps: num(f[10]),
                abr: num(f[11]),
            });
        }
    }
    Line::Other(line.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Audio,
    Combined,
}

fn has_codec(c: &Option<String>) -> bool {
    !matches!(c.as_deref(), None | Some("none"))
}

pub fn stream_kind(vcodec: &Option<String>, acodec: &Option<String>) -> Kind {
    match (has_codec(vcodec), has_codec(acodec)) {
        (true, true) => Kind::Combined,
        (true, false) => Kind::Video,
        (false, true) => Kind::Audio,
        _ => Kind::Combined,
    }
}

/// «H.264», «AAC» и так далее; незнакомое показываем как есть.
pub fn codec_label(codec: &str) -> String {
    let base = codec.split('.').next().unwrap_or(codec);
    let name = match base {
        "avc1" => "H.264",
        "vp9" | "vp09" => "VP9",
        "av01" => "AV1",
        "mp4a" => "AAC",
        "opus" => "Opus",
        _ => return codec.to_string(),
    };
    name.to_string()
}

/// Описание потока: «1080p · H.264», «130 kbps · AAC». Чего нет в данных, того и не пишем.
pub fn stream_label(h: &Hook) -> String {
    let kind = stream_kind(&h.vcodec, &h.acodec);
    let mut parts: Vec<String> = Vec::new();
    if matches!(kind, Kind::Video | Kind::Combined) {
        if let Some(height) = h.height {
            let fps = h.fps.filter(|f| *f > 30.0).map(|f| format!("{}", f.round() as u32)).unwrap_or_default();
            parts.push(format!("{height}p{fps}"));
        }
        if let Some(c) = h.vcodec.as_deref().filter(|c| *c != "none") {
            parts.push(codec_label(c));
        }
    }
    if matches!(kind, Kind::Audio | Kind::Combined) {
        if let Some(abr) = h.abr {
            parts.push(format!("{} kbps", abr.round() as u32));
        }
        if let Some(c) = h.acodec.as_deref().filter(|c| *c != "none") {
            parts.push(codec_label(c));
        }
    }
    if parts.is_empty() {
        h.format_id.clone()
    } else {
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamView {
    pub kind: Kind,
    pub label: String,
    pub fraction: Option<f64>,
    pub finished: bool,
}

/// Общий ход загрузки. Пустые поля — yt-dlp их не сообщил.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub fraction: Option<f64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
    pub streams: Vec<StreamView>,
}

struct State {
    kind: Kind,
    label: String,
    downloaded: u64,
    total: Option<u64>,
    speed: Option<f64>,
    eta: Option<u64>,
    finished: bool,
}

/// Складывает ход потоков. У склеенного файла их два: видео и аудио.
#[derive(Default)]
pub struct Tracker {
    order: Vec<String>,
    streams: HashMap<String, State>,
    /// Наибольшая доля, которую уже показывали. Общий размер растёт, когда стартует второй поток,
    /// а полоса не должна откатываться назад; настоящие числа по потокам остаются в `streams`.
    peak: f64,
}

impl Tracker {
    pub fn update(&mut self, h: &Hook) -> Snapshot {
        if !self.streams.contains_key(&h.format_id) {
            self.order.push(h.format_id.clone());
        }
        let s = self.streams.entry(h.format_id.clone()).or_insert(State {
            kind: Kind::Combined,
            label: String::new(),
            downloaded: 0,
            total: None,
            speed: None,
            eta: None,
            finished: false,
        });
        s.kind = stream_kind(&h.vcodec, &h.acodec);
        s.label = stream_label(h);

        let total = h.total.or(h.total_estimate).filter(|t| *t > 0);
        if h.status == "finished" {
            let size = total.unwrap_or(h.downloaded);
            s.finished = true;
            s.downloaded = size;
            s.total = if size > 0 { Some(size) } else { s.total };
            s.speed = None;
            s.eta = None;
        } else {
            s.downloaded = h.downloaded;
            if total.is_some() {
                s.total = total;
            }
            s.speed = h.speed;
            s.eta = h.eta;
        }

        let states: Vec<&State> = self.order.iter().filter_map(|k| self.streams.get(k)).collect();
        let downloaded: u64 = states.iter().map(|s| s.downloaded).sum();
        // Общий размер известен, только когда его сообщил каждый поток.
        let total: Option<u64> =
            if states.iter().all(|s| s.total.is_some()) { Some(states.iter().filter_map(|s| s.total).sum()) } else { None };
        let active: Vec<&&State> = states.iter().filter(|s| !s.finished).collect();
        let speeds: Vec<f64> = active.iter().filter_map(|s| s.speed).collect();
        let speed = if speeds.is_empty() { None } else { Some(speeds.iter().sum()) };
        let eta = active.iter().filter_map(|s| s.eta).max();

        let fraction = total.filter(|t| *t > 0).map(|t| (downloaded as f64 / t as f64).min(1.0));
        let fraction = fraction.map(|f| {
            self.peak = self.peak.max(f);
            self.peak
        });

        Snapshot {
            downloaded,
            total,
            fraction,
            speed,
            eta,
            streams: states
                .iter()
                .map(|s| StreamView {
                    kind: s.kind,
                    label: s.label.clone(),
                    fraction: s.total.filter(|t| *t > 0).map(|t| (s.downloaded as f64 / t as f64).min(1.0)),
                    finished: s.finished,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(status: &str, downloaded: u64, total: Option<u64>, id: &str, v: &str, a: &str) -> Hook {
        Hook {
            status: status.into(),
            downloaded,
            total,
            total_estimate: None,
            speed: None,
            eta: None,
            format_id: id.into(),
            vcodec: Some(v.into()),
            acodec: Some(a.into()),
            height: None,
            fps: None,
            abr: None,
        }
    }

    #[test]
    fn parses_a_real_download_line() {
        let l = parse_line("AVTO|D|downloading|1047552|11829048|NA|1056109.2896501445|10|18|avc1.42001E|mp4a.40.2|360|25|NA");
        let Line::Download(h) = l else { panic!("не строка загрузки") };
        assert_eq!(h.status, "downloading");
        assert_eq!(h.downloaded, 1_047_552);
        assert_eq!(h.total, Some(11_829_048));
        assert_eq!(h.total_estimate, None);
        assert_eq!(h.eta, Some(10));
        assert_eq!(h.format_id, "18");
        assert_eq!(h.height, Some(360));
        assert_eq!(h.abr, None);
        assert!(h.speed.unwrap() > 1_000_000.0);
    }

    #[test]
    fn parses_post_and_file_lines() {
        assert_eq!(parse_line("AVTO|P|started|Merger"), Line::Post { status: "started".into(), name: "Merger".into() });
        assert_eq!(parse_line("AVTO|F|/tmp/a|b [x].mp4"), Line::File(PathBuf::from("/tmp/a|b [x].mp4")));
        assert_eq!(parse_line("ERROR: boom"), Line::Other("ERROR: boom".into()));
        assert_eq!(parse_line("AVTO|D|short"), Line::Other("AVTO|D|short".into()));
    }

    #[test]
    fn tracker_sums_streams_and_reports_speed() {
        let mut t = Tracker::default();
        let mut a = hook("downloading", 50, Some(100), "137", "avc1", "none");
        a.speed = Some(10.0);
        a.eta = Some(5);
        let first = t.update(&a);
        assert_eq!((first.downloaded, first.total, first.fraction), (50, Some(100), Some(0.5)));

        let mut b = hook("downloading", 30, None, "251", "none", "opus");
        b.total_estimate = Some(300);
        let second = t.update(&b);
        assert_eq!((second.downloaded, second.total), (80, Some(400)));
        assert_eq!((second.speed, second.eta), (Some(10.0), Some(5)));
    }

    #[test]
    fn finished_stream_counts_as_complete() {
        let mut t = Tracker::default();
        t.update(&hook("downloading", 10, Some(40), "22", "avc1", "mp4a"));
        let done = t.update(&hook("finished", 40, Some(40), "22", "avc1", "mp4a"));
        assert_eq!((done.downloaded, done.total), (40, Some(40)));
        assert!(done.streams[0].finished);
    }

    #[test]
    fn streams_are_reported_separately_in_arrival_order() {
        let mut t = Tracker::default();
        let mut v = hook("downloading", 50, Some(100), "137", "avc1.640028", "none");
        v.height = Some(1080);
        t.update(&v);
        let mut a = hook("downloading", 10, Some(20), "140", "none", "mp4a.40.2");
        a.abr = Some(129.5);
        let p = t.update(&a);
        assert_eq!(p.streams.iter().map(|s| s.kind).collect::<Vec<_>>(), [Kind::Video, Kind::Audio]);
        assert_eq!(p.streams[0].fraction, Some(0.5));
        assert_eq!(p.streams[1].fraction, Some(0.5));
        assert_eq!(p.fraction, Some(0.5));
        assert_eq!(p.streams[0].label, "1080p · H.264");
        assert_eq!(p.streams[1].label, "130 kbps · AAC");
    }

    #[test]
    fn unknown_size_has_no_fraction() {
        let mut t = Tracker::default();
        let p = t.update(&hook("downloading", 10, None, "x", "none", "none"));
        assert_eq!((p.total, p.fraction), (None, None));
    }

    #[test]
    fn bar_does_not_roll_back_when_the_second_stream_starts() {
        let mut t = Tracker::default();
        t.update(&hook("downloading", 900, Some(1000), "137", "avc1", "none"));
        let done = t.update(&hook("finished", 1000, Some(1000), "137", "avc1", "none"));
        assert_eq!(done.fraction, Some(1.0));
        let audio = t.update(&hook("downloading", 10, Some(500), "140", "none", "mp4a"));
        // настоящая доля 1010/1500, но показываем не меньше уже достигнутой
        assert_eq!(audio.fraction, Some(1.0));
        assert_eq!(audio.total, Some(1500));
    }

    #[test]
    fn label_never_invents_values() {
        let mut v = hook("downloading", 0, None, "401", "av01.0.12M.08", "none");
        v.height = Some(2160);
        v.fps = Some(60.0);
        assert_eq!(stream_label(&v), "2160p60 · AV1");
        assert_eq!(stream_label(&hook("downloading", 0, None, "x", "none", "none")), "x");
    }
}
