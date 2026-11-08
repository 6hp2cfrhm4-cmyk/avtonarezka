use std::path::Path;

use anyhow::{bail, Context};
use tokio::io::AsyncReadExt;

use super::HOP;

pub const SAMPLE_RATE: usize = 16_000;

/// Звук видео как моно PCM 16 кГц — формат, который нужен и анализу, и Whisper.
pub async fn decode_pcm16k(path: &Path) -> anyhow::Result<Vec<i16>> {
    let mut child = crate::platform::command("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-f", "s16le", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("не удалось запустить ffmpeg")?;

    let mut bytes = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut bytes).await?;
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("ffmpeg не смог прочитать звук: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

/// Громкость в dBFS на каждый отрезок `HOP`.
pub fn rms_db(pcm: &[i16]) -> Vec<f32> {
    let win = (HOP * SAMPLE_RATE as f64) as usize;
    pcm.chunks(win)
        .map(|c| {
            let ms = c.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / c.len() as f64;
            (10.0 * ms.max(1e-9).log10()) as f32
        })
        .collect()
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Устойчивая z-оценка (медиана и MAD): редкие всплески не растягивают шкалу.
/// Результат ограничен диапазоном [-3, 8].
pub fn robust_z(v: &[f32]) -> Vec<f32> {
    let mut tmp = v.to_vec();
    let med = median(&mut tmp);
    let mut dev: Vec<f32> = v.iter().map(|x| (x - med).abs()).collect();
    let mad = (1.4826 * median(&mut dev)).max(1e-3);
    v.iter().map(|x| ((x - med) / mad).clamp(-3.0, 8.0)).collect()
}

/// Секунды, в которые тихо: подходят для границ между частями.
pub fn quiet_points(db: &[f32], below_db: f32) -> Vec<f64> {
    db.iter().enumerate().filter(|(_, d)| **d < below_db).map(|(i, _)| (i as f64 + 0.5) * HOP).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f64, amp: f64) -> Vec<i16> {
        (0..(secs * SAMPLE_RATE as f64) as usize)
            .map(|i| (amp * 32767.0 * (i as f64 * 0.05).sin()) as i16)
            .collect()
    }

    #[test]
    fn rms_has_one_value_per_hop() {
        assert_eq!(rms_db(&tone(10.0, 0.1)).len(), 20);
    }

    #[test]
    fn loud_burst_stands_out_after_normalisation() {
        let mut pcm = tone(4.0, 0.02);
        pcm.extend(tone(1.0, 0.8)); // 4.0 .. 5.0 с
        pcm.extend(tone(5.0, 0.02));
        let z = robust_z(&rms_db(&pcm));
        // отрезки 8 и 9 — это секунды 4.0..5.0
        assert!(z[8] > 3.0 && z[9] > 3.0, "{:?}", &z[6..12]);
        assert!(z[2].abs() < 1.0 && z[15].abs() < 1.0);
    }

    #[test]
    fn robust_z_survives_flat_signal() {
        let z = robust_z(&vec![-40.0; 20]);
        assert!(z.iter().all(|v| v.is_finite() && v.abs() < 1e-3));
    }

    #[test]
    fn quiet_points_are_segment_centres() {
        let p = quiet_points(&[-50.0, -10.0, -60.0], -30.0);
        assert_eq!(p, vec![0.25, 1.25]);
    }
}
