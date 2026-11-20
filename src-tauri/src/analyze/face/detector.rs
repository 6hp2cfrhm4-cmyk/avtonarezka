//! YuNet 2023mar: BGR float32 NCHW, 640x640, stride 8/16/32.
use super::Rect;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tract_onnx::prelude::*;

pub const SIZE: usize = 640;
pub const FRAME_BYTES: usize = SIZE * SIZE * 3;
#[derive(Clone, Debug, Serialize)]
pub struct Detection {
    pub rect: Rect,
    pub confidence: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    #[serde(default)]
    pub fingerprint: String,
    pub frames: usize,
    pub found: usize,
    pub longest_gap: usize,
    pub median: Option<Rect>,
    pub error: Option<String>,
}
impl Check {
    pub fn present(&self) -> bool {
        self.error.is_none()
            && self.frames >= 3
            && self.found as f64 / self.frames as f64 >= 0.6
            && self.longest_gap <= 4
            && self.median.is_some()
    }
    pub fn unavailable(error: String) -> Self {
        Self {
            fingerprint: String::new(),
            frames: 0,
            found: 0,
            longest_gap: 0,
            median: None,
            error: Some(error),
        }
    }
}

/// Выбирается только лицо внутри подтверждённой камеры, а не лица в чужом контенте.
pub fn within_camera(faces: &[Detection], camera: Rect) -> Option<Rect> {
    faces
        .iter()
        .filter(|f| {
            let x = f.rect.x + f.rect.width / 2.0;
            let y = f.rect.y + f.rect.height / 2.0;
            x >= camera.x
                && x <= camera.x + camera.width
                && y >= camera.y
                && y <= camera.y + camera.height
        })
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
        .map(|d| d.rect)
}
pub fn summarize(samples: &[Option<Rect>]) -> Check {
    let mut gap = 0;
    let mut longest_gap = 0;
    for s in samples {
        if s.is_some() {
            gap = 0
        } else {
            gap += 1;
            longest_gap = longest_gap.max(gap);
        }
    }
    Check {
        fingerprint: String::new(),
        frames: samples.len(),
        found: samples.iter().flatten().count(),
        longest_gap,
        median: super::median_camera(samples, 0.6),
        error: None,
    }
}

/// Декодер выдаёт два кадра в секунду; в памяти максимум один кадр и список рамок.
/// Тот же тракт пригоден для полной записи и коротких HQ-секций.
pub async fn check_clip(
    path: &Path,
    model: &Path,
    start: f64,
    duration: f64,
    width: u32,
    height: u32,
    camera: Rect,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<Check> {
    scan_clip(
        path,
        model,
        start,
        duration,
        width,
        height,
        camera,
        cancel,
        |_| {},
    )
    .await
}

pub async fn scan_clip(
    path: &Path,
    model: &Path,
    start: f64,
    duration: f64,
    width: u32,
    height: u32,
    camera: Rect,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    progress: impl Fn(usize) + Send,
) -> anyhow::Result<Check> {
    use tokio::io::AsyncReadExt;
    camera.validate().map_err(anyhow::Error::msg)?;
    if !start.is_finite() || start < 0.0 || !duration.is_finite() || duration <= 0.0 {
        bail!("Неверные границы проверки камеры");
    }
    if *cancel.borrow() {
        bail!("Проверка камеры остановлена");
    }
    let model = model.to_path_buf();
    let detector =
        std::sync::Arc::new(tokio::task::spawn_blocking(move || Detector::load(&model)).await??);
    let mut child=crate::platform::command("ffmpeg").args(["-v","error","-nostdin","-ss",&format!("{start:.3}"),"-t",&format!("{duration:.3}"),"-i"]).arg(path)
        .args(["-an","-vf","fps=2,scale=640:640:force_original_aspect_ratio=decrease,pad=640:640:(ow-iw)/2:(oh-ih)/2:color=black,format=bgr24","-threads","1","-f","rawvideo","-"])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn()?;
    let mut stdout = child.stdout.take().context("Нет потока кадров камеры")?;
    let mut samples = vec![];
    loop {
        if *cancel.borrow() {
            bail!("Проверка камеры остановлена");
        }
        let mut bytes = vec![0; FRAME_BYTES];
        let read = tokio::select! {
            value=tokio::time::timeout(std::time::Duration::from_secs(30),stdout.read_exact(&mut bytes))=>value.context("Декодер камеры не отвечает")?,
            _=cancel.changed()=>{bail!("Проверка камеры остановлена");}
        };
        match read {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let detector = detector.clone();
        let faces =
            tokio::task::spawn_blocking(move || detector.detect(&bytes, width, height)).await??;
        samples.push(within_camera(&faces, camera));
        progress(samples.len());
        if samples.len() > ((duration * 2.0).ceil() as usize + 2) {
            bail!("Поток камеры превысил заданную длительность");
        }
    }
    if !child.wait().await?.success() {
        bail!("Не удалось декодировать запись для проверки лица");
    }
    if samples.len() + 2 < (duration * 2.0).floor() as usize {
        bail!(
            "Запись прочитана не полностью: {} кадров вместо примерно {}",
            samples.len(),
            (duration * 2.0).floor()
        );
    }
    Ok(summarize(&samples))
}
type Run = Box<dyn Fn(&[f32]) -> anyhow::Result<Vec<Vec<f32>>> + Send + Sync>;
pub struct Detector {
    run: Run,
}
impl Detector {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let plan = tract_onnx::onnx()
            .model_for_path(path)
            .context("Не удалось прочитать YuNet")?
            .with_input_fact(0, f32::fact([1, 3, SIZE, SIZE]).into())?
            .into_optimized()?
            .into_runnable()?;
        let run: Run = Box::new(move |data| {
            let input = tract_ndarray::Array4::from_shape_vec((1, 3, SIZE, SIZE), data.to_vec())?
                .into_tensor();
            plan.run(tvec!(input.into()))?
                .iter()
                .map(|v| Ok(v.to_array_view::<f32>()?.iter().copied().collect()))
                .collect()
        });
        Ok(Self { run })
    }
    pub fn detect(&self, bgr: &[u8], width: u32, height: u32) -> anyhow::Result<Vec<Detection>> {
        if bgr.len() != FRAME_BYTES || width == 0 || height == 0 {
            bail!("Неверный размер кадра YuNet");
        }
        let mut data = vec![0.0; FRAME_BYTES];
        let plane = SIZE * SIZE;
        for i in 0..plane {
            for c in 0..3 {
                data[c * plane + i] = bgr[3 * i + c] as f32;
            }
        }
        decode(&(self.run)(&data)?, width, height)
    }
}
pub fn decode(outputs: &[Vec<f32>], width: u32, height: u32) -> anyhow::Result<Vec<Detection>> {
    if outputs.len() != 12 || width == 0 || height == 0 {
        bail!("YuNet вернул неожиданные выходы");
    }
    let scale = (SIZE as f64 / width as f64).min(SIZE as f64 / height as f64);
    // ffmpeg scale rounds the fitted dimensions down; pad centres with integer offsets.
    let fitted_w = (width as f64 * scale).round() as usize;
    let fitted_h = (height as f64 * scale).round() as usize;
    let px = ((SIZE - fitted_w) / 2) as f64;
    let py = ((SIZE - fitted_h) / 2) as f64;
    let mut faces = vec![];
    for (level, stride) in [8, 16, 32].into_iter().enumerate() {
        let side = SIZE / stride;
        let count = side * side;
        if outputs[level].len() != count
            || outputs[level + 3].len() != count
            || outputs[level + 6].len() != count * 4
        {
            bail!("Неверные размеры выходов YuNet");
        }
        for i in 0..count {
            let score =
                (outputs[level][i].clamp(0.0, 1.0) * outputs[level + 3][i].clamp(0.0, 1.0)).sqrt();
            if !score.is_finite() || score < 0.7 {
                continue;
            }
            let b = &outputs[level + 6][i * 4..i * 4 + 4];
            if b.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let x = (b[0] as f64 + (i % side) as f64) * stride as f64;
            let y = (b[1] as f64 + (i / side) as f64) * stride as f64;
            let w = (b[2] as f64).exp() * stride as f64;
            let h = (b[3] as f64).exp() * stride as f64;
            let left = ((x - w / 2.0 - px) / scale / width as f64).clamp(0.0, 1.0);
            let top = ((y - h / 2.0 - py) / scale / height as f64).clamp(0.0, 1.0);
            let right = ((x + w / 2.0 - px) / scale / width as f64).clamp(0.0, 1.0);
            let bottom = ((y + h / 2.0 - py) / scale / height as f64).clamp(0.0, 1.0);
            if let Ok(rect) = (Rect {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            })
            .validate()
            {
                faces.push(Detection {
                    rect,
                    confidence: score,
                });
            }
        }
    }
    faces.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut kept: Vec<Detection> = vec![];
    for face in faces {
        if kept.iter().all(|k| k.rect.iou(face.rect) < 0.3) {
            kept.push(face);
            if kept.len() == 20 {
                break;
            }
        }
    }
    Ok(kept)
}
/// JPEG/видео декодируется существующим ffmpeg; файл остаётся нетронутым.
pub async fn read_frame(path: &Path, at: f64) -> anyhow::Result<Vec<u8>> {
    if !at.is_finite() || at < 0.0 {
        bail!("Неверное время кадра");
    }
    let mut cmd = crate::platform::command("ffmpeg");
    cmd.args(["-v", "error", "-nostdin"]);
    // image2 с input -ss 0 успешно завершается без кадров (на настоящем JPEG).
    if at > 0.0 {
        cmd.args(["-ss", &format!("{at:.3}")]);
    }
    let output=tokio::time::timeout(std::time::Duration::from_secs(30),cmd.arg("-i").arg(path)
        .args(["-an","-frames:v","1","-vf","scale=640:640:force_original_aspect_ratio=decrease,pad=640:640:(ow-iw)/2:(oh-ih)/2:color=black,format=bgr24","-threads","1","-f","rawvideo","-"])
        .kill_on_drop(true).output()).await.context("Кадр лица не прочитан за 30 секунд")??;
    if !output.status.success() || output.stdout.len() != FRAME_BYTES {
        bail!(
            "Не удалось прочитать кадр лица: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verification_requires_temporal_coverage_and_rejects_other_faces() {
        let camera = Rect {
            x: 0.05,
            y: 0.05,
            width: 0.2,
            height: 0.3,
        };
        let face = Rect {
            x: 0.1,
            y: 0.1,
            width: 0.05,
            height: 0.1,
        };
        let outside = Detection {
            rect: Rect { x: 0.7, ..face },
            confidence: 0.99,
        };
        assert!(within_camera(&[outside.clone()], camera).is_none());
        assert_eq!(
            within_camera(
                &[
                    outside,
                    Detection {
                        rect: face,
                        confidence: 0.9
                    }
                ],
                camera
            ),
            Some(face)
        );
        assert!(summarize(&[Some(face); 10]).present());
        assert!(!summarize(&[None; 10]).present());
        let mut samples = vec![Some(face); 20];
        for s in &mut samples[6..11] {
            *s = None;
        }
        assert!(
            !summarize(&samples).present(),
            "Пропуск лица более двух секунд требует Inbox"
        );
        assert!(!Check::unavailable("Модель недоступна".into()).present());
    }
    #[test]
    fn decoder_validates_shapes_scores_nms_and_letterbox_coordinates() {
        let mut o: Vec<Vec<f32>> = (0..12)
            .map(|i| {
                let count = (SIZE / [8, 16, 32][i % 3]).pow(2);
                vec![
                    0.0;
                    count
                        * if i >= 9 {
                            10
                        } else if i >= 6 {
                            4
                        } else {
                            1
                        }
                ]
            })
            .collect();
        let i = 32 * 80 + 20;
        o[0][i] = 1.0;
        o[3][i] = 1.0;
        o[6][i * 4..i * 4 + 4].copy_from_slice(&[0.0, 0.0, 2.0, 2.0]);
        let d = decode(&o, 1920, 1080).unwrap();
        assert_eq!(d.len(), 1);
        assert!(d[0].rect.x > 0.1 && d[0].rect.x < 0.3);
        assert!(d[0].rect.y > 0.1 && d[0].rect.y < 0.5);
        o[0][i] = f32::NAN;
        assert!(decode(&o, 1920, 1080).unwrap().is_empty());
        assert!(decode(&o[..3], 1920, 1080).is_err());
    }
    #[tokio::test]
    async fn first_jpeg_frame_does_not_seek_past_static_image() {
        let file = std::env::temp_dir().join(format!("avto-face-{}.jpg", rand::random::<u64>()));
        assert!(std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=black:s=320x180",
                "-frames:v",
                "1",
                "-threads",
                "1"
            ])
            .arg(&file)
            .status()
            .unwrap()
            .success());
        let data = read_frame(&file, 0.0).await.unwrap();
        assert_eq!(data.len(), FRAME_BYTES);
        assert!(data.iter().all(|b| *b < 5));
        let _ = std::fs::remove_file(file);
    }
    #[tokio::test]
    #[ignore = "Нужны согласованная YuNet AVTO_FACE_MODEL и настоящий кадр xQc AVTO_FACE_FRAME; ничего не скачивает"]
    async fn real_yunet_finds_streamer_and_rejects_blank_frame() {
        let model =
            std::path::PathBuf::from(std::env::var_os("AVTO_FACE_MODEL").expect("AVTO_FACE_MODEL"));
        let frame =
            std::path::PathBuf::from(std::env::var_os("AVTO_FACE_FRAME").expect("AVTO_FACE_FRAME"));
        let data = read_frame(&frame, 0.0).await.unwrap();
        let detector = Detector::load(&model).unwrap();
        let faces = detector.detect(&data, 1920, 1080).unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&faces).unwrap());
        assert!(
            faces.iter().any(|f| f.rect.x < 0.2 && f.rect.y < 0.3),
            "Не найдено лицо стримера слева сверху"
        );
        assert!(detector
            .detect(&vec![0; FRAME_BYTES], 1920, 1080)
            .unwrap()
            .is_empty());
    }
    #[tokio::test]
    #[ignore = "Нужны согласованные AVTO_FACE_MODEL и AVTO_FACE_VIDEO; проверяет 10 секунд без загрузок"]
    async fn real_streamer_is_verified_across_the_clip_at_two_frames_per_second() {
        let model =
            std::path::PathBuf::from(std::env::var_os("AVTO_FACE_MODEL").expect("AVTO_FACE_MODEL"));
        let video =
            std::path::PathBuf::from(std::env::var_os("AVTO_FACE_VIDEO").expect("AVTO_FACE_VIDEO"));
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let progress = std::sync::atomic::AtomicUsize::new(0);
        let check = scan_clip(
            &video,
            &model,
            0.0,
            10.0,
            1920,
            1080,
            Rect {
                x: 0.07,
                y: 0.04,
                width: 0.14,
                height: 0.3,
            },
            rx,
            |n| {
                progress.store(n, std::sync::atomic::Ordering::Relaxed);
            },
        )
        .await
        .unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&check).unwrap());
        assert_eq!(check.frames, 20);
        assert!(check.present());
        assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 20);
    }
    #[tokio::test]
    async fn cancelled_scan_does_not_load_a_model_or_spawn_decoder() {
        let (_tx, rx) = tokio::sync::watch::channel(true);
        let result = check_clip(
            Path::new("absent-video"),
            Path::new("absent-model"),
            0.0,
            10.0,
            1920,
            1080,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.2,
                height: 0.2,
            },
            rx,
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("остановлена"));
    }
}
