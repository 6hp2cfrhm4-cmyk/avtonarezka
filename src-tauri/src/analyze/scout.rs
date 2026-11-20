//! Дешёвый проход: ограниченные окна по всей временной шкале, без запуска Whisper.
use super::{Candidate, HOP};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
}
impl Window {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
    pub fn contains(&self, start: f64, end: f64) -> bool {
        start >= self.start - 0.001 && end <= self.end + 0.001
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoutReport {
    pub windows: Vec<Window>,
    pub duration: f64,
    pub selected_seconds: f64,
    pub speech_seconds: f64,
    pub full: bool,
}

/// Бюджет делится между временным разнообразием и глобальными пиками.
/// Несколько важных событий в одной области могут попасть в разные окна.
pub fn select(
    duration: f64,
    curve: &[f32],
    max_clip: f64,
    coverage: f64,
    full: bool,
) -> ScoutReport {
    if !duration.is_finite() || duration <= 0.0 {
        return ScoutReport {
            windows: vec![],
            duration: 0.0,
            selected_seconds: 0.0,
            speech_seconds: 0.0,
            full: true,
        };
    }
    let full = full || duration <= 600.0;
    let mut windows = Vec::new();
    if full {
        windows.push(Window {
            start: 0.0,
            end: duration,
        });
    } else {
        let coverage = if coverage.is_finite() {
            coverage.clamp(0.1, 0.75)
        } else {
            0.25
        };
        let max_clip = if max_clip.is_finite() {
            max_clip.clamp(5.0, 180.0)
        } else {
            60.0
        };
        let budget = duration * coverage;
        let width = (max_clip + 24.0).max(45.0).min(budget);
        let count = (budget / width).floor().max(1.0) as usize;
        let clean: Vec<f64> = curve
            .iter()
            .map(|v| {
                if v.is_finite() {
                    v.max(0.0) as f64
                } else {
                    0.0
                }
            })
            .collect();
        let mut prefix = vec![0.0; clean.len() + 1];
        for (i, v) in clean.iter().enumerate() {
            prefix[i + 1] = prefix[i] + v;
        }
        // Короткий центр оценивает событие, контекст по краям остаётся для Whisper.
        let core = (max_clip / 3.0).clamp(5.0, 20.0);
        let score = |center: f64| {
            let a = (((center - core / 2.0).max(0.0) / HOP).floor() as usize).min(clean.len());
            let b = (((center + core / 2.0) / HOP).ceil() as usize).clamp(a, clean.len());
            let mean = (prefix[b] - prefix[a]) / (b - a).max(1) as f64;
            let peak = clean[a..b].iter().copied().fold(0.0, f64::max);
            0.6 * mean + 0.4 * peak
        };
        let make = |center: f64| {
            let start = (center - width / 2.0).clamp(0.0, duration - width);
            Window {
                start,
                end: start + width,
            }
        };
        // Треть бюджета гарантирует представительство всей записи. Равенства
        // разрешаются в пользу центра области, а не её первого кадра.
        let spread = count.div_ceil(3);
        for bin in 0..spread {
            let lo = bin as f64 * duration / spread as f64;
            let hi = (bin + 1) as f64 * duration / spread as f64;
            let middle = (lo + hi) / 2.0;
            let mut best = middle;
            let mut value = score(middle);
            let mut at = lo;
            while at < hi {
                let v = score(at);
                if v > value + 1e-10
                    || ((v - value).abs() <= 1e-10 && (at - middle).abs() < (best - middle).abs())
                {
                    best = at;
                    value = v;
                }
                at += 5.0;
            }
            windows.push(make(best));
        }
        let mut ranked = vec![];
        let mut center = 0.0;
        while center <= duration {
            ranked.push((score(center), center));
            center += 5.0;
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.total_cmp(&b.1)));
        for (_, center) in ranked {
            if windows.len() >= count {
                break;
            }
            let candidate = make(center);
            if windows.iter().all(|w| {
                (w.end.min(candidate.end) - w.start.max(candidate.start)).max(0.0) <= width * 0.25
            }) {
                windows.push(candidate);
            }
        }
        windows.sort_by(|a, b| a.start.total_cmp(&b.start));
    }
    // Объединение только касающихся окон; непроанализированных дыр внутри окна нет.
    let mut merged: Vec<Window> = Vec::new();
    for w in windows {
        if let Some(last) = merged.last_mut().filter(|p| w.start <= p.end + 0.001) {
            last.end = last.end.max(w.end);
        } else {
            merged.push(w);
        }
    }
    let selected_seconds = merged.iter().map(Window::len).sum();
    ScoutReport {
        windows: merged,
        duration,
        selected_seconds,
        speech_seconds: 0.0,
        full,
    }
}

/// Не позволяем объединить фразы по разные стороны непроанализированного промежутка.
pub fn covered(candidates: Vec<Candidate>, windows: &[Window]) -> Vec<Candidate> {
    candidates
        .into_iter()
        .filter(|c| windows.iter().any(|w| w.contains(c.start, c.end)))
        .collect()
}

/// Сравнение двух top-N: доля моментов, которым нашлась пара с IoU >= 0.3.
pub fn top_overlap(a: &[Candidate], b: &[Candidate], n: usize) -> f64 {
    let a = &a[..a.len().min(n)];
    if a.is_empty() {
        return 0.0;
    }
    let mut used = vec![false; b.len().min(n)];
    let mut matched = 0;
    for x in a {
        if let Some(i) = b
            .iter()
            .take(n)
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .find_map(|(i, y)| {
                let inter = (x.end.min(y.end) - x.start.max(y.start)).max(0.0);
                (inter / (x.len() + y.len() - inter).max(0.001) >= 0.3).then_some(i)
            })
        {
            used[i] = true;
            matched += 1;
        }
    }
    matched as f64 / a.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hot_peak_is_inside_budget_and_windows_are_spread() {
        let duration = 10800.0;
        let mut curve = vec![0.0; (duration / HOP) as usize];
        for i in 2400..2440 {
            curve[i] = 1.0;
        }
        let r = select(duration, &curve, 60.0, 0.25, false);
        assert!(r.windows.iter().any(|w| w.contains(1200.0, 1220.0)));
        assert!(r.selected_seconds <= duration * 0.25 + 0.01);
        assert!(r.windows.iter().any(|w| w.start > 9000.0));
        assert!(r.windows.windows(2).all(|w| w[0].end <= w[1].start));
        assert_eq!(
            r.windows,
            select(duration, &curve, 60.0, 0.25, false).windows
        );
    }
    #[test]
    fn full_and_short_never_lose_coverage() {
        for (d, force) in [(89.0, false), (10800.0, true)] {
            let r = select(d, &[], 60.0, 0.25, force);
            assert!(r.full);
            assert_eq!(r.windows, vec![Window { start: 0.0, end: d }]);
        }
    }
    #[test]
    fn global_peaks_share_a_region_without_losing_temporal_spread() {
        let duration = 10800.0;
        let mut curve = vec![0.0; (duration / HOP) as usize];
        for at in [1200.0, 1300.0, 1400.0, 1500.0] {
            for i in (at / HOP) as usize..((at + 15.0) / HOP) as usize {
                curve[i] = 4.0;
            }
        }
        let r = select(duration, &curve, 60.0, 0.25, false);
        for at in [1200.0, 1300.0, 1400.0, 1500.0] {
            assert!(
                r.windows.iter().any(|w| w.contains(at, at + 15.0)),
                "Потерян глобальный пик {at}"
            );
        }
        assert!(r.windows.iter().any(|w| w.start > 9500.0));
        assert!(r.selected_seconds <= duration * 0.25);
        assert!(
            select(1000.0, &[f32::NAN; 20], f64::NAN, f64::NAN, false).selected_seconds <= 250.0
        );
    }
    #[test]
    fn no_candidate_can_bridge_a_hole_and_overlap_matching_is_unique() {
        let c = |a, b| Candidate {
            start: a,
            end: b,
            score: 1.0,
            climax: a + 1.0,
        };
        let x = covered(
            vec![c(0.0, 10.0), c(5.0, 35.0), c(30.0, 40.0)],
            &[
                Window {
                    start: 0.0,
                    end: 10.0,
                },
                Window {
                    start: 30.0,
                    end: 40.0,
                },
            ],
        );
        assert_eq!(x.len(), 2);
        assert_eq!(
            top_overlap(&[c(0.0, 10.0), c(0.0, 10.0)], &[c(1.0, 11.0)], 10),
            0.5
        );
    }
}
