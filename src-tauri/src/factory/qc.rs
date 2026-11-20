//! QC не удаляет ролик: причины остаются в Inbox, решение принимает человек.
use crate::{media, platform};
pub const VERSION: u32 = 2;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    #[serde(default)]
    pub version: u32,
    pub path: String,
    #[serde(default)]
    pub fingerprint: String,
    pub issues: Vec<String>,
    pub duration: f64,
    pub checked_at: i64,
}
impl Report {
    pub fn passed(&self) -> bool {
        self.issues.is_empty()
    }
}
#[derive(Clone, Debug)]
pub struct Rules {
    pub max_duration: f64,
    pub subtitles_required: bool,
    pub subtitles_present: bool,
    pub face_required: bool,
    pub face_present: bool,
    pub final_size: bool,
    pub content_height: Option<f64>,
}

pub fn evaluate(
    info: &media::VideoInfo,
    mean_db: Option<f64>,
    black_seconds: f64,
    rules: &Rules,
) -> Vec<String> {
    let mut issues = vec![];
    if !info.duration.is_finite() || info.duration < 1.0 || info.duration > rules.max_duration + 0.2
    {
        issues.push(format!(
            "Длительность {:.1} с вне диапазона 1–{:.0} с",
            info.duration, rules.max_duration
        ));
    }
    if !info.has_audio || mean_db.is_none_or(|v| !v.is_finite() || v < -50.0) {
        issues.push("Нет звука или почти тишина".into());
    }
    if black_seconds > info.duration * 0.15 {
        issues.push("Чёрные кадры основного видео занимают более 15% ролика".into());
    }
    let size = if rules.final_size {
        (1080, 1920)
    } else {
        (540, 960)
    };
    if (info.width, info.height) != size {
        issues.push(format!(
            "Размер {}×{} вместо {}×{}",
            info.width, info.height, size.0, size.1
        ));
    }
    if info.codec != "h264" {
        issues.push(format!("Кодек {} вместо H.264", info.codec));
    }
    if info.size == 0 || info.size > 2_000_000_000 {
        issues.push("Пустой файл или размер более 2 ГБ".into());
    }
    if rules.subtitles_required && !rules.subtitles_present {
        issues.push("Субтитры включены, но нет слов в выбранном фрагменте".into());
    }
    if rules.face_required && !rules.face_present {
        issues.push("Лицо для раскладки «Реакция» не подтверждено".into());
    }
    issues
}
fn value_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_once(key)
        .and_then(|(_, v)| v.split_whitespace().next())
}
pub(crate) fn measurements(log: &str) -> (Option<f64>, f64) {
    let mean = log
        .lines()
        .find_map(|line| value_after(line, "mean_volume:").and_then(|v| v.parse().ok()));
    let black = log
        .lines()
        .filter_map(|line| value_after(line, "black_duration:").and_then(|v| v.parse::<f64>().ok()))
        .sum();
    (mean, black)
}
pub fn file_stamp(path: &Path) -> String {
    std::fs::metadata(path)
        .ok()
        .map(|m| {
            format!(
                "{}-{}",
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            )
        })
        .unwrap_or_default()
}
pub async fn check(path: &Path, rules: &Rules) -> Report {
    let mut report = Report {
        version: VERSION,
        path: path.to_string_lossy().into_owned(),
        fingerprint: file_stamp(path),
        checked_at: chrono::Utc::now().timestamp(),
        ..Default::default()
    };
    let result = async {
        let info = media::probe(path).await?;
        let region = rules
            .content_height
            .map(|v| format!("crop=iw:trunc(ih*{:.4}/2)*2:0:0,", v.clamp(0.3, 0.75)))
            .unwrap_or_default();
        let filter = format!("{region}blackdetect=d=0.2:pix_th=0.10:pic_th=0.95");
        let out = platform::command("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-i"])
            .arg(path)
            .args(["-vf", &filter, "-af", "volumedetect", "-f", "null", "-"])
            .output()
            .await?;
        if !out.status.success() {
            anyhow::bail!("ffmpeg не смог проверить кадры и звук");
        }
        let (mean, black) = measurements(&String::from_utf8_lossy(&out.stderr));
        Ok::<_, anyhow::Error>((info, mean, black))
    }
    .await;
    match result {
        Ok((info, mean, black)) => {
            report.duration = info.duration;
            report.issues = evaluate(&info, mean, black, rules);
        }
        Err(e) => report.issues.push(format!("QC не выполнен: {e}")),
    };
    report
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rules() -> Rules {
        Rules {
            max_duration: 180.0,
            subtitles_required: true,
            subtitles_present: true,
            face_required: false,
            face_present: false,
            final_size: false,
            content_height: Some(0.5),
        }
    }
    #[test]
    fn qc_returns_reasons_instead_of_rejecting_the_clip() {
        let info = media::VideoInfo {
            duration: 20.0,
            width: 540,
            height: 960,
            fps: 30.0,
            has_audio: true,
            codec: "h264".into(),
            audio_codec: Some("aac".into()),
            size: 1000,
        };
        assert!(evaluate(&info, Some(-15.0), 0.0, &rules()).is_empty());
        let mut r = rules();
        r.subtitles_present = false;
        r.face_required = true;
        let issues = evaluate(&info, Some(-70.0), 10.0, &r);
        assert_eq!(issues.len(), 4);
        assert!(issues.iter().any(|v| v.contains("Субтитры")));
    }
    #[test]
    fn parses_real_filter_logs_including_silence() {
        let (mean,black)=measurements("[a] mean_volume: -18.7 dB\n[b] black_start:0 black_end:2.5 black_duration:2.5\n[b] black_duration:1.0");
        assert_eq!((mean, black), (Some(-18.7), 3.5));
        assert_eq!(
            measurements("[a] mean_volume: -inf dB").0,
            Some(f64::NEG_INFINITY)
        );
    }
    #[tokio::test]
    async fn actual_ffmpeg_detects_black_and_silent_video() {
        let path = std::env::temp_dir().join(format!("avto-qc-black-{}.mp4", std::process::id()));
        assert!(platform::command("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=black:s=540x960:r=10:d=1.5",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=16000:cl=mono",
                "-t",
                "1.5",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac"
            ])
            .arg(&path)
            .status()
            .await
            .unwrap()
            .success());
        let report = check(&path, &rules()).await;
        assert!(
            report.issues.iter().any(|v| v.contains("тишина")),
            "{report:?}"
        );
        assert!(
            report.issues.iter().any(|v| v.contains("Чёрные")),
            "{report:?}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
