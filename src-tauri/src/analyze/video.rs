use std::path::Path;

use anyhow::{bail, Context};
use tokio::io::AsyncReadExt;

use super::HOP;

const W: usize = 160;
const H: usize = 90;
const FRAME: usize = W * H;

pub struct VideoSignals {
    /// Средняя разница соседних кадров (0..255) на каждый отрезок `HOP`.
    pub motion: Vec<f32>,
    /// Резкие смены кадра, секунды.
    pub cuts: Vec<f64>,
}

/// Читает видео как серые кадры 160×90 с частотой 1/HOP и считает разницу между соседними.
/// Кадры не копятся в памяти: в каждый момент держим только два.
pub async fn analyze(path: &Path, mut on_progress: impl FnMut(usize)) -> anyhow::Result<VideoSignals> {
    let mut child = crate::platform::command("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-an", "-vf", &format!("fps={},scale={W}:{H}:flags=area,format=gray", 1.0 / HOP)])
        .args(["-f", "rawvideo", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("не удалось запустить ffmpeg")?;

    let mut out = child.stdout.take().unwrap();
    let (mut prev, mut cur) = (vec![0u8; FRAME], vec![0u8; FRAME]);
    let mut diffs: Vec<f32> = Vec::new();
    let mut have_prev = false;
    loop {
        match out.read_exact(&mut cur).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if have_prev {
            let sum: u64 = prev.iter().zip(&cur).map(|(a, b)| a.abs_diff(*b) as u64).sum();
            diffs.push(sum as f32 / FRAME as f32);
        } else {
            diffs.push(0.0);
        }
        std::mem::swap(&mut prev, &mut cur);
        have_prev = true;
        if diffs.len() % 120 == 0 {
            on_progress(diffs.len());
        }
    }
    let status = child.wait_with_output().await?;
    if !status.status.success() {
        bail!("ffmpeg не смог прочитать видео: {}", String::from_utf8_lossy(&status.stderr).trim());
    }
    Ok(VideoSignals { cuts: detect_cuts(&diffs), motion: diffs })
}

/// Смена сцены: разница заметно больше, чем обычно вокруг, и велика сама по себе.
/// Первые кадры не оцениваем: у них нет истории, а `diffs[0]` — служебный ноль.
pub fn detect_cuts(diffs: &[f32]) -> Vec<f64> {
    let mut cuts = Vec::new();
    for i in 4..diffs.len() {
        let mut local: Vec<f32> = diffs[i.saturating_sub(10).max(1)..i].to_vec();
        local.sort_by(|a, b| a.total_cmp(b));
        let med = local[local.len() / 2];
        let far_from_last = cuts.last().is_none_or(|&t: &f64| i as f64 * HOP - t > 1.0);
        if diffs[i] > 28.0 && diffs[i] > 4.0 * med.max(1.0) && far_from_last {
            cuts.push(i as f64 * HOP);
        }
    }
    cuts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spike_in_calm_footage_is_a_cut() {
        let mut d = vec![2.0; 40];
        d[20] = 90.0;
        assert_eq!(detect_cuts(&d), vec![10.0]);
    }

    #[test]
    fn constant_motion_has_no_cuts() {
        assert!(detect_cuts(&vec![35.0; 40]).is_empty());
    }

    #[test]
    fn busy_footage_from_the_first_frame_has_no_false_cut() {
        let mut d = vec![0.0]; // служебный ноль первого кадра, как у `analyze`
        d.extend(vec![40.0; 30]);
        assert!(detect_cuts(&d).is_empty());
    }

    #[test]
    fn neighbouring_spikes_count_once() {
        let mut d = vec![2.0; 40];
        d[20] = 90.0;
        d[21] = 80.0;
        assert_eq!(detect_cuts(&d).len(), 1);
    }

    #[tokio::test]
    async fn finds_black_to_white_cut_in_real_video() {
        let path = std::env::temp_dir().join(format!("avto-cut-{}.mp4", std::process::id()));
        let st = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-filter_complex"])
            .arg("color=c=black:s=320x180:r=25:d=6[a];color=c=white:s=320x180:r=25:d=6[b];[a][b]concat=n=2:v=1:a=0[v]")
            .args(["-map", "[v]", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(st.success());
        let sig = analyze(&path, |_| {}).await.unwrap();
        assert_eq!(sig.cuts.len(), 1, "{:?}", sig.cuts);
        assert!((sig.cuts[0] - 6.0).abs() <= 1.0, "{:?}", sig.cuts);
        assert!((sig.motion.len() as f64 * HOP - 12.0).abs() <= 1.0);
    }
}
