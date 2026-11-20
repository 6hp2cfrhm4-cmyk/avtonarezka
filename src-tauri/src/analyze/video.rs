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
    pub camera_motion: Option<Vec<f32>>,
}

/// Читает видео как серые кадры 160×90 с частотой 1/HOP и считает разницу между соседними.
/// Кадры не копятся в памяти: в каждый момент держим только два.
pub async fn analyze(path: &Path, on_progress: impl FnMut(usize)) -> anyhow::Result<VideoSignals> {
    analyze_camera(path, None, on_progress).await
}

/// Отдельная разница кадров в подтверждённой области камеры: геймплей за её
/// пределами не создаёт ложную «реакцию». Детекция/распознавание эмоций не требуется.
pub async fn analyze_camera(
    path: &Path,
    camera: Option<super::face::Rect>,
    mut on_progress: impl FnMut(usize),
) -> anyhow::Result<VideoSignals> {
    let camera = camera
        .map(|r| r.validate())
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let filter = if let Some(r) = camera {
        format!("[0:v]fps={},split=2[whole][face];[whole]scale={W}:{H}:flags=area,format=gray[full];[face]crop=iw*{}:ih*{}:iw*{}:ih*{},scale={W}:{H}:flags=area,format=gray[roi];[full][roi]hstack=inputs=2,format=gray[v]",1.0/HOP,r.width,r.height,r.x,r.y)
    } else {
        format!(
            "[0:v]fps={},scale={W}:{H}:flags=area,format=gray[v]",
            1.0 / HOP
        )
    };
    let mut cmd = crate::platform::command("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .arg("-an");
    if camera.is_some() {
        cmd.args([
            "-filter_complex_threads",
            "1",
            "-filter_complex",
            &filter,
            "-map",
            "[v]",
        ]);
    } else {
        cmd.args([
            "-vf",
            &format!("fps={},scale={W}:{H}:flags=area,format=gray", 1.0 / HOP),
        ]);
    }
    let mut child = cmd
        .args(["-f", "rawvideo", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("не удалось запустить ffmpeg")?;

    let mut out = child.stdout.take().unwrap();
    let frame = FRAME * if camera.is_some() { 2 } else { 1 };
    let (mut prev, mut cur) = (vec![0u8; frame], vec![0u8; frame]);
    let mut camera_motion = camera.map(|_| Vec::new());
    let mut diffs: Vec<f32> = Vec::new();
    let mut have_prev = false;
    loop {
        match out.read_exact(&mut cur).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if have_prev {
            let stride = if camera.is_some() { W * 2 } else { W };
            let mut sum = 0u64;
            let mut face_sum = 0u64;
            for (row_a, row_b) in prev.chunks_exact(stride).zip(cur.chunks_exact(stride)) {
                sum += row_a[..W]
                    .iter()
                    .zip(&row_b[..W])
                    .map(|(a, b)| a.abs_diff(*b) as u64)
                    .sum::<u64>();
                if camera.is_some() {
                    face_sum += row_a[W..]
                        .iter()
                        .zip(&row_b[W..])
                        .map(|(a, b)| a.abs_diff(*b) as u64)
                        .sum::<u64>();
                }
            }
            diffs.push(sum as f32 / FRAME as f32);
            if let Some(ref mut values) = camera_motion {
                values.push(face_sum as f32 / FRAME as f32);
            }
        } else {
            diffs.push(0.0);
            if let Some(ref mut values) = camera_motion {
                values.push(0.0);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        have_prev = true;
        if diffs.len() % 120 == 0 {
            on_progress(diffs.len());
        }
    }
    let status = child.wait_with_output().await?;
    if !status.status.success() {
        bail!(
            "ffmpeg не смог прочитать видео: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        );
    }
    Ok(VideoSignals {
        cuts: detect_cuts(&diffs),
        motion: diffs,
        camera_motion,
    })
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
    #[tokio::test]
    async fn camera_motion_ignores_content_outside_the_confirmed_region() {
        let dir =
            std::env::temp_dir().join(format!("avto-camera-motion-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reaction.mkv");
        let result=std::process::Command::new("ffmpeg").args(["-v","error","-y","-f","lavfi","-i","color=black:s=640x360:r=2:d=12","-vf","drawbox=x=0:y=0:w=128:h=144:color=white:t=fill:enable='between(t,3,5)',drawbox=x=320:y=0:w=320:h=360:color=white:t=fill:enable='between(t,8,10)'","-c:v","ffv1","-threads","1"]).arg(&path).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let region = super::super::face::Rect {
            x: 0.0,
            y: 0.0,
            width: 0.2,
            height: 0.4,
        };
        let signal = analyze_camera(&path, Some(region), |_| {}).await.unwrap();
        let camera = signal.camera_motion.unwrap();
        assert_eq!(camera.len(), 24);
        assert!(camera[6] > 200.0, "Движение камеры отсутствует");
        assert!(
            camera[16] < 1.0,
            "Изменение контента за камерой попало в её сигнал"
        );
        assert!(signal.motion[16] > 100.0);
        assert!(analyze(&path, |_| {})
            .await
            .unwrap()
            .camera_motion
            .is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

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
