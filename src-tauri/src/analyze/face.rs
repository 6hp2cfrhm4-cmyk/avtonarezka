//! Геометрия неподвижной камеры и локальный YuNet (отдельная добровольная загрузка).
//! Наличие рамки само по себе не означает, что лицо найдено или проверено.
pub mod detector;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Rect {
    pub fn expanded(self, factor: f64) -> Self {
        let width = (self.width * factor.max(1.0)).min(1.0);
        let height = (self.height * factor.max(1.0)).min(1.0);
        Self {
            x: (self.x + (self.width - width) / 2.0).clamp(0.0, 1.0 - width),
            y: (self.y + (self.height - height) / 2.0).clamp(0.0, 1.0 - height),
            width,
            height,
        }
    }
    pub fn validate(self) -> Result<Self, String> {
        if ![self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            || self.x < 0.0
            || self.y < 0.0
            || self.width < 0.01
            || self.height < 0.01
            || self.x + self.width > 1.0 + 1e-9
            || self.y + self.height > 1.0 + 1e-9
        {
            return Err("Рамка камеры должна находиться внутри кадра; размеры не меньше 1%".into());
        }
        Ok(self)
    }
    pub fn iou(self, other: Self) -> f64 {
        let w = ((self.x + self.width).min(other.x + other.width) - self.x.max(other.x)).max(0.0);
        let h = ((self.y + self.height).min(other.y + other.height) - self.y.max(other.y)).max(0.0);
        let intersection = w * h;
        intersection
            / (self.width * self.height + other.width * other.height - intersection).max(1e-12)
    }
    /// Чётные координаты для crop в yuv420p, без выхода за исходный кадр.
    pub fn pixels(self, w: u32, h: u32) -> Result<[u32; 4], String> {
        self.validate()?;
        if w < 2 || h < 2 {
            return Err("Кадр слишком мал для камеры".into());
        }
        let x = ((self.x * w as f64).floor() as u32 & !1).min((w - 2) & !1);
        let y = ((self.y * h as f64).floor() as u32 & !1).min((h - 2) & !1);
        let cw = ((self.width * w as f64).round() as u32 & !1)
            .max(2)
            .min((w - x) & !1);
        let ch = ((self.height * h as f64).round() as u32 & !1)
            .max(2)
            .min((h - y) & !1);
        Ok([x, y, cw, ch])
    }
}

/// Медиана устойчивой группы: перемещающиеся лица/реклама не сдвигают камеру стримера.
/// Группы строятся по пересечению; редкие детекции не считаются надёжным профилем.
pub fn median_camera(samples: &[Option<Rect>], min_coverage: f64) -> Option<Rect> {
    if samples.is_empty() {
        return None;
    }
    let valid: Vec<Rect> = samples
        .iter()
        .filter_map(|r| r.and_then(|r| r.validate().ok()))
        .collect();
    // Не квадратично по 21600 кадрам трёхчасового VOD: не больше 64 опорных рамок.
    let stride = valid.len().div_ceil(64).max(1);
    let anchor = valid
        .iter()
        .step_by(stride)
        .max_by_key(|a| valid.iter().filter(|b| a.iou(**b) >= 0.35).count())?;
    let cluster: Vec<Rect> = valid
        .iter()
        .copied()
        .filter(|b| anchor.iou(*b) >= 0.35)
        .collect();
    if cluster.len() < 3
        || cluster.len() as f64 / (samples.len() as f64) < min_coverage.clamp(0.0, 1.0)
    {
        return None;
    }
    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(f64::total_cmp);
        let n = v.len();
        if n % 2 == 0 {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        } else {
            v[n / 2]
        }
    }
    Rect {
        x: median(cluster.iter().map(|r| r.x).collect()),
        y: median(cluster.iter().map(|r| r.y).collect()),
        width: median(cluster.iter().map(|r| r.width).collect()),
        height: median(cluster.iter().map(|r| r.height).collect()),
    }
    .validate()
    .ok()
}

/// Стабилизация выбранного лица: dead-zone, EMA, три подтверждения перемещения.
/// Пропуски сохраняют рамку до 2 секунд, затем возвращают None для Inbox.
#[derive(Default)]
pub struct Stabilizer {
    current: Option<Rect>,
    pending: Option<(Rect, u8)>,
    missing: u8,
}
impl Stabilizer {
    pub fn update(&mut self, detection: Option<Rect>) -> Option<Rect> {
        let next = detection.and_then(|r| r.validate().ok());
        let Some(next) = next else {
            self.missing = self.missing.saturating_add(1);
            self.pending = None;
            if self.missing > 4 {
                self.current = None;
            }
            return self.current;
        };
        self.missing = 0;
        let Some(current) = self.current else {
            self.current = Some(next);
            return self.current;
        };
        if current.iou(next) < 0.35 {
            let count = self
                .pending
                .filter(|(r, _)| r.iou(next) >= 0.6)
                .map_or(1, |(_, n)| n + 1);
            self.pending = Some((next, count));
            if count >= 3 {
                self.current = Some(next);
                self.pending = None;
            }
        } else {
            self.pending = None;
            let close = [
                (current.x, next.x),
                (current.y, next.y),
                (current.width, next.width),
                (current.height, next.height),
            ]
            .iter()
            .all(|(a, b)| (a - b).abs() < 0.003);
            if !close {
                let mix = |a: f64, b: f64| a * 0.8 + b * 0.2;
                self.current = Some(Rect {
                    x: mix(current.x, next.x),
                    y: mix(current.y, next.y),
                    width: mix(current.width, next.width),
                    height: mix(current.height, next.height),
                });
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64) -> Rect {
        Rect {
            x,
            y: 0.1,
            width: 0.2,
            height: 0.25,
        }
    }
    #[test]
    fn rejects_outside_nan_and_tiny_boxes() {
        for r in [
            rect(-0.1),
            rect(0.9),
            rect(f64::NAN),
            Rect {
                height: 0.001,
                ..rect(0.1)
            },
        ] {
            assert!(r.validate().is_err());
        }
        assert_eq!(
            Rect {
                x: 0.9,
                y: 0.9,
                width: 0.1,
                height: 0.1
            }
            .pixels(1919, 1079)
            .unwrap(),
            [1726, 970, 192, 108]
        );
    }
    #[test]
    fn median_rejects_interludes_and_missing_faces() {
        let s = [
            Some(rect(0.1)),
            Some(rect(0.105)),
            None,
            Some(rect(0.098)),
            Some(rect(0.6)),
            Some(rect(0.1)),
        ];
        let r = median_camera(&s, 0.6).unwrap();
        assert!((r.x - 0.1).abs() < 1e-8);
        assert!(median_camera(&s, 0.8).is_none());
        assert!(median_camera(&[None; 10], 0.1).is_none());
    }
    #[test]
    fn hysteresis_jitter_sudden_move_and_loss() {
        let mut s = Stabilizer::default();
        s.update(Some(rect(0.1)));
        assert_eq!(s.update(Some(rect(0.101))), Some(rect(0.1)));
        assert_eq!(s.update(Some(rect(0.6))), Some(rect(0.1)));
        s.update(Some(rect(0.6)));
        assert_eq!(s.update(Some(rect(0.6))), Some(rect(0.6)));
        for _ in 0..4 {
            assert!(s.update(None).is_some());
        }
        assert!(s.update(None).is_none());
        assert_eq!(s.update(Some(rect(0.2))), Some(rect(0.2)));
    }
    #[test]
    fn pixel_crop_stays_inside_even_at_edges() {
        for w in [2, 3, 17, 1920] {
            for h in [2, 3, 19, 1080] {
                let [x, y, cw, ch] = Rect {
                    x: 0.99,
                    y: 0.99,
                    width: 0.01,
                    height: 0.01,
                }
                .pixels(w, h)
                .unwrap();
                assert!(x + cw <= w && y + ch <= h && cw >= 2 && ch >= 2);
                assert_eq!((x | y | cw | ch) & 1, 0);
            }
        }
    }
    #[test]
    fn expanded_camera_keeps_face_center_and_stays_inside_frame() {
        let r = rect(0.1).expanded(1.8);
        assert!((r.width - 0.36).abs() < 1e-9);
        assert!(r.validate().is_ok());
        let r = Rect {
            x: 0.9,
            y: 0.9,
            width: 0.1,
            height: 0.1,
        }
        .expanded(1.8);
        assert!(r.validate().is_ok());
        assert_eq!(r.x + r.width, 1.0);
    }
}
