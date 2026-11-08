use super::{Candidate, Sentence, Signals, HOP};

#[derive(Clone, Debug)]
pub struct Weights {
    pub loudness: f32,
    pub motion: f32,
    pub events: f32,
    pub speech: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self { loudness: 0.35, motion: 0.15, events: 0.35, speech: 0.15 }
    }
}

#[derive(Clone, Debug)]
pub struct Params {
    pub min_len: f64,
    pub max_len: f64,
    pub top_k: usize,
    /// Какую долю короткого из двух кандидатов можно делить с уже выбранным.
    pub max_overlap: f64,
    /// Запас до первой и после последней фразы, секунд.
    pub pad_start: f64,
    pub pad_end: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self { min_len: 20.0, max_len: 60.0, top_k: 30, max_overlap: 0.3, pad_start: 0.15, pad_end: 0.35 }
    }
}

/// Кривая интереса 0..1 по сетке `HOP`. Недостающие сигналы не штрафуют: веса пересчитываются.
pub fn interest_curve(sig: &Signals, w: &Weights) -> Vec<f32> {
    let n = [Some(sig.loudness.len()), Some(sig.motion.len()), sig.events.as_ref().map(Vec::len), sig.speech.as_ref().map(Vec::len)]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);
    let parts: Vec<(f32, &[f32])> = [
        (w.loudness, Some(sig.loudness.as_slice())),
        (w.motion, Some(sig.motion.as_slice())),
        (w.events, sig.events.as_deref()),
        (w.speech, sig.speech.as_deref()),
    ]
    .into_iter()
    .filter_map(|(wt, v)| v.filter(|v| !v.is_empty() && wt > 0.0).map(|v| (wt, v)))
    .collect();
    let total: f32 = parts.iter().map(|(wt, _)| wt).sum();
    if total <= 0.0 {
        return vec![0.0; n];
    }
    let raw: Vec<f32> = (0..n)
        .map(|i| parts.iter().map(|(wt, v)| wt * v.get(i).copied().unwrap_or(0.0).clamp(0.0, 4.0) / 4.0).sum::<f32>() / total)
        .collect();
    smooth(&raw, 1)
}

/// Скользящее среднее по окну 2·r+1.
fn smooth(v: &[f32], r: usize) -> Vec<f32> {
    (0..v.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(r), (i + r + 1).min(v.len()));
            v[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect()
}

fn slice(curve: &[f32], a: f64, b: f64) -> &[f32] {
    let lo = ((a / HOP).floor().max(0.0) as usize).min(curve.len());
    let hi = ((b / HOP).ceil().max(0.0) as usize).clamp(lo, curve.len());
    &curve[lo..hi]
}

/// Лучшая секунда для баннера: пик кривой в середине клипа (35–85% длины).
pub fn pick_climax(curve: &[f32], start: f64, end: f64) -> f64 {
    let len = end - start;
    let (lo, hi) = (start + 0.35 * len, start + 0.85 * len);
    let sm = smooth(curve, 1);
    let first = (lo / HOP).floor().max(0.0) as usize;
    let best = (first..(hi / HOP).ceil() as usize)
        .filter(|&i| i < sm.len())
        .max_by(|&a, &b| sm[a].total_cmp(&sm[b]));
    match best {
        Some(i) => ((i as f64 + 0.5) * HOP).clamp(lo, hi),
        None => start + 0.6 * len,
    }
}

fn rate(curve: &[f32], start: f64, end: f64) -> f64 {
    let s = slice(curve, start, end);
    if s.is_empty() {
        return 0.0;
    }
    let mean = s.iter().sum::<f32>() / s.len() as f32;
    let max = s.iter().copied().fold(0.0, f32::max);
    (0.6 * mean + 0.4 * max) as f64
}

/// Окна из подряд идущих фраз длиной `min_len..=max_len`; границы всегда на границах фраз.
pub fn candidates(sentences: &[Sentence], curve: &[f32], p: &Params) -> Vec<Candidate> {
    let mut sents: Vec<&Sentence> = sentences.iter().collect();
    sents.sort_by(|a, b| a.start.total_cmp(&b.start));

    let mut out = Vec::new();
    for i in 0..sents.len() {
        for j in i..sents.len() {
            let len = sents[j].end - sents[i].start;
            if len > p.max_len {
                break;
            }
            if len >= p.min_len {
                let start = (sents[i].start - p.pad_start).max(0.0);
                let end = sents[j].end + p.pad_end;
                out.push(Candidate { start, end, score: rate(curve, start, end), climax: pick_climax(curve, start, end) });
            }
        }
    }
    nms(out, p.max_overlap, p.top_k)
}

/// Ближайшая к `t` тихая точка в пределах `snap` секунд, иначе сама `t`.
fn snap_to(t: f64, quiet: &[f64], snap: f64) -> f64 {
    quiet
        .iter()
        .copied()
        .filter(|q| (q - t).abs() <= snap)
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
        .unwrap_or(t)
}

/// Кандидаты для видео без речи (геймплей, музыка): скользящие окна, границы — в паузах.
pub fn window_candidates(duration: f64, curve: &[f32], quiet: &[f64], p: &Params) -> Vec<Candidate> {
    let target = (p.min_len + p.max_len) / 2.0;
    let mut out = Vec::new();
    let mut t = 0.0;
    while t + p.min_len <= duration {
        let start = snap_to(t, quiet, 2.5).max(0.0);
        let end = snap_to((t + target).min(duration), quiet, 2.5).min(duration);
        if end - start >= p.min_len && end - start <= p.max_len {
            out.push(Candidate { start, end, score: rate(curve, start, end), climax: pick_climax(curve, start, end) });
        }
        t += 5.0;
    }
    nms(out, p.max_overlap, p.top_k)
}

/// Оставляет лучшие кандидаты, которые почти не пересекаются.
pub fn nms(mut c: Vec<Candidate>, max_overlap: f64, top_k: usize) -> Vec<Candidate> {
    c.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Candidate> = Vec::new();
    for cand in c {
        if kept.len() >= top_k {
            break;
        }
        let clash = kept.iter().any(|k| {
            let inter = (cand.end.min(k.end) - cand.start.max(k.start)).max(0.0);
            inter / cand.len().min(k.len()) > max_overlap
        });
        if !clash {
            kept.push(cand);
        }
    }
    kept
}

/// Режим «по N секунд»: подряд идущие части, у которых границы прилипают к ближайшей паузе
/// или концу фразы. Короткого хвоста не остаётся.
pub fn fixed_grid(duration: f64, target: f64, min_tail: f64, boundaries: &[f64], snap: f64) -> Vec<(f64, f64)> {
    let mut parts = Vec::new();
    let mut start = 0.0;
    while duration - start > target + min_tail {
        let ideal = start + target;
        let end = boundaries
            .iter()
            .copied()
            .filter(|b| (b - ideal).abs() <= snap && *b > start + target * 0.5)
            .min_by(|a, b| (a - ideal).abs().total_cmp(&(b - ideal).abs()))
            .unwrap_or(ideal);
        parts.push((start, end));
        start = end;
    }
    parts.push((start, duration));
    parts
}

/// Место, где можно разрезать видео, и насколько оно для этого подходит.
struct Cut {
    t: f64,
    quality: f64,
}

/// Режим «вплотную»: всё видео подряд, без пропусков, перекрытий и запаса по краям. Граница ставится
/// в паузе между фразами, на смене сцены или в тихом месте; длина части лежит в `min_len..=max_len`.
/// Видео короче `min_len` остаётся одной частью, а остаток меньше двух минимальных частей делится пополам.
pub fn tight_split(duration: f64, sentences: &[Sentence], cuts: &[f64], quiet: &[f64], curve: &[f32], p: &Params) -> Vec<Candidate> {
    let mut sents: Vec<&Sentence> = sentences.iter().collect();
    sents.sort_by(|a, b| a.start.total_cmp(&b.start));
    let inside = |t: f64| {
        let i = sents.partition_point(|s| s.start < t);
        i > 0 && sents[i - 1].end > t
    };

    let mut points: Vec<Cut> = sents
        .windows(2)
        .map(|w| {
            let gap = (w[1].start - w[0].end).max(0.0);
            Cut { t: w[0].end + gap / 2.0, quality: 1.0 + gap.min(1.5) / 3.0 }
        })
        .collect();
    points.extend(cuts.iter().map(|&t| Cut { t, quality: if inside(t) { 0.15 } else { 0.55 } }));
    // тишина внутри фразы не повод её резать: берём только тихие места вне речи
    points.extend(quiet.iter().filter(|&&t| !inside(t)).map(|&t| Cut { t, quality: if sents.is_empty() { 0.4 } else { 0.3 } }));
    points.sort_by(|a, b| a.t.total_cmp(&b.t));

    let target = (p.min_len + p.max_len) / 2.0;
    let span = (p.max_len - p.min_len).max(1.0);
    // лучшая граница в [lo, hi]: качество минус штраф за удаление от желаемой точки `want`
    let best_in = |lo: f64, hi: f64, want: f64| {
        let (a, b) = (points.partition_point(|c| c.t < lo), points.partition_point(|c| c.t <= hi));
        let fit = |c: &Cut| c.quality - 0.8 * (c.t - want).abs() / span;
        points[a..b].iter().max_by(|x, y| fit(x).total_cmp(&fit(y))).map_or(want.clamp(lo, hi), |c| c.t)
    };

    let mut bounds = vec![0.0];
    let mut start = 0.0;
    while duration - start > p.max_len {
        let (lo, hi) = (start + p.min_len, (start + p.max_len).min(duration - p.min_len));
        let end = if hi >= lo {
            best_in(lo, hi, start + target)
        } else {
            let rest = duration - start;
            best_in(start + rest / 4.0, start + rest * 0.75, start + rest / 2.0)
        };
        bounds.push(end);
        start = end;
    }
    bounds.push(duration);

    bounds
        .windows(2)
        .map(|w| Candidate { start: w[0], end: w[1], score: rate(curve, w[0], w[1]), climax: pick_climax(curve, w[0], w[1]) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentences(n: usize, each: f64) -> Vec<Sentence> {
        (0..n).map(|i| Sentence { start: i as f64 * each, end: (i + 1) as f64 * each - 0.2, text: format!("фраза {i}") }).collect()
    }

    /// Кривая, у которой пик в секундах `at`, остальное фон.
    fn curve_with_peak(secs: f64, at: f64) -> Vec<f32> {
        (0..(secs / HOP) as usize).map(|i| if ((i as f64 + 0.5) * HOP - at).abs() < 2.0 { 1.0 } else { 0.05 }).collect()
    }

    #[test]
    fn curve_is_normalised_and_peaks_where_the_signal_does() {
        let mut z = vec![0.0f32; 100];
        z[40] = 6.0;
        let sig = Signals { loudness: z, motion: vec![0.0; 100], ..Default::default() };
        let c = interest_curve(&sig, &Weights::default());
        assert!(c.iter().all(|v| (0.0..=1.0).contains(v)));
        let peak = c.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert!((39..=41).contains(&peak), "{peak}");
    }

    #[test]
    fn missing_signals_do_not_drag_the_score_down() {
        let loud = vec![4.0f32; 10];
        let only = Signals { loudness: loud.clone(), motion: vec![], ..Default::default() };
        let all = Signals { loudness: loud, motion: vec![4.0; 10], events: Some(vec![4.0; 10]), speech: Some(vec![4.0; 10]), ..Default::default() };
        let (a, b) = (interest_curve(&only, &Weights::default()), interest_curve(&all, &Weights::default()));
        assert!((a[5] - b[5]).abs() < 1e-5, "{} vs {}", a[5], b[5]);
    }

    #[test]
    fn best_candidate_covers_the_peak_and_respects_length() {
        let s = sentences(60, 5.0); // 300 секунд
        let c = candidates(&s, &curve_with_peak(300.0, 150.0), &Params::default());
        assert!(!c.is_empty());
        let top = &c[0];
        assert!(top.start <= 150.0 && 150.0 <= top.end, "{top:?}");
        assert!(c.iter().all(|k| k.len() >= 20.0 && k.len() <= 61.0));
    }

    #[test]
    fn candidate_edges_sit_on_sentence_edges() {
        let s = sentences(60, 5.0);
        let p = Params::default();
        for k in candidates(&s, &curve_with_peak(300.0, 150.0), &p) {
            let on_start = s.iter().any(|x| (x.start - p.pad_start - k.start).abs() < 1e-9 || k.start == 0.0);
            let on_end = s.iter().any(|x| (x.end + p.pad_end - k.end).abs() < 1e-9);
            assert!(on_start && on_end, "{k:?}");
        }
    }

    #[test]
    fn selected_candidates_barely_overlap_and_top_k_is_respected() {
        let s = sentences(120, 5.0);
        let p = Params { top_k: 5, ..Default::default() };
        let c = candidates(&s, &curve_with_peak(600.0, 300.0), &p);
        assert_eq!(c.len(), 5);
        for (i, a) in c.iter().enumerate() {
            for b in &c[i + 1..] {
                let inter = (a.end.min(b.end) - a.start.max(b.start)).max(0.0);
                assert!(inter / a.len().min(b.len()) <= 0.3 + 1e-9, "{a:?} {b:?}");
            }
        }
        assert!(c.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn climax_stays_in_the_middle_of_the_clip() {
        // пик в самом начале клипа не должен утащить баннер к началу
        let c = curve_with_peak(100.0, 41.0);
        let t = pick_climax(&c, 40.0, 80.0);
        assert!(t >= 40.0 + 0.35 * 40.0 && t <= 40.0 + 0.85 * 40.0, "{t}");
        // пик в середине находится точно
        let c = curve_with_peak(100.0, 60.0);
        assert!((pick_climax(&c, 40.0, 80.0) - 60.0).abs() <= 2.0);
    }

    #[test]
    fn fixed_grid_covers_everything_without_a_tiny_tail() {
        let parts = fixed_grid(200.0, 60.0, 15.0, &[], 3.0);
        assert_eq!(parts.first().unwrap().0, 0.0);
        assert_eq!(parts.last().unwrap().1, 200.0);
        assert!(parts.windows(2).all(|w| w[0].1 == w[1].0));
        assert!(parts.iter().all(|(a, b)| b - a >= 15.0));
        assert_eq!(parts.len(), 4); // 60 + 60 + 60 + 20
        let short = fixed_grid(130.0, 60.0, 15.0, &[], 3.0);
        assert_eq!(short.len(), 2); // хвост 10 с приклеился к предыдущей части
    }

    #[test]
    fn windows_without_speech_find_the_peak_and_respect_length() {
        let curve = curve_with_peak(300.0, 150.0);
        let quiet: Vec<f64> = (0..60).map(|i| i as f64 * 5.0 + 0.25).collect();
        let c = window_candidates(300.0, &curve, &quiet, &Params::default());
        assert!(!c.is_empty());
        assert!(c[0].start <= 150.0 && 150.0 <= c[0].end, "{:?}", c[0]);
        assert!(c.iter().all(|k| k.len() >= 20.0 && k.len() <= 60.0 && k.end <= 300.0));
        // границы прилипли к паузам
        assert!(quiet.iter().any(|q| (q - c[0].start).abs() < 1e-9));
    }

    #[test]
    fn fixed_grid_snaps_to_nearby_boundaries() {
        let parts = fixed_grid(200.0, 60.0, 15.0, &[58.0, 61.5, 118.5], 3.0);
        assert_eq!(parts[0].1, 61.5); // ближе к 60, чем 58
        assert_eq!(parts[1].1, 118.5 + 0.0); // 61.5+60 = 121.5 → граница 118.5 в пределах 3 с
    }

    /// Части идут подряд от нуля до конца видео, ни одна не длиннее `max_len`.
    fn assert_tight(parts: &[Candidate], duration: f64, p: &Params) {
        assert!(!parts.is_empty());
        assert_eq!(parts.first().unwrap().start, 0.0);
        assert_eq!(parts.last().unwrap().end, duration);
        assert!(parts.windows(2).all(|w| w[0].end == w[1].start), "зазор или перекрытие: {parts:?}");
        assert!(parts.iter().all(|k| k.len() > 0.0 && k.len() <= p.max_len + 1e-9), "{parts:?}");
        assert!(parts.iter().all(|k| k.climax >= k.start && k.climax <= k.end), "{parts:?}");
    }

    #[test]
    fn tight_split_covers_the_whole_video_and_cuts_in_pauses() {
        let s = sentences(120, 5.0); // 600 секунд, пауза 0.2 с после каждой фразы
        let p = Params::default();
        let parts = tight_split(600.0, &s, &[], &[], &curve_with_peak(600.0, 300.0), &p);
        assert_tight(&parts, 600.0, &p);
        assert!(parts.iter().all(|k| k.len() >= p.min_len), "{parts:?}");
        for k in &parts[..parts.len() - 1] {
            // граница — середина паузы между фразами, ни одна фраза не разрезана
            assert!(s.iter().any(|x| (x.end + 0.1 - k.end).abs() < 1e-9), "{k:?}");
        }
    }

    #[test]
    fn tight_split_prefers_a_long_pause_near_the_target() {
        // 40 с — обычная пауза 0.2 с, 45 с — пауза 1.5 с: длинная лучше, хотя и дальше от цели
        let s = vec![
            Sentence { start: 0.0, end: 39.9, text: "а".into() },
            Sentence { start: 40.1, end: 44.0, text: "б".into() },
            Sentence { start: 45.5, end: 100.0, text: "в".into() },
        ];
        let parts = tight_split(100.0, &s, &[], &[], &[], &Params::default());
        assert!((parts[0].end - 44.75).abs() < 1e-9, "{parts:?}");
    }

    #[test]
    fn tight_split_avoids_scene_cuts_inside_a_phrase() {
        let s = vec![
            Sentence { start: 0.0, end: 38.0, text: "а".into() },
            Sentence { start: 40.0, end: 100.0, text: "б".into() },
        ];
        // смена сцены у самой цели (40,5 с), но на ней идёт фраза «б»; пауза 38–40 с лучше
        let parts = tight_split(100.0, &s, &[40.5], &[], &[], &Params::default());
        assert!((parts[0].end - 39.0).abs() < 1e-9, "{parts:?}");
    }

    #[test]
    fn tight_split_without_speech_uses_scene_cuts_and_quiet_points() {
        let p = Params::default();
        let parts = tight_split(200.0, &[], &[41.0], &[], &[], &p);
        assert_eq!(parts[0].end, 41.0); // смена сцены рядом с целью
        let quiet: Vec<f64> = (0..400).map(|i| i as f64 * 0.5 + 0.25).collect();
        let parts = tight_split(200.0, &[], &[], &quiet, &[], &p);
        assert_tight(&parts, 200.0, &p);
        assert!(parts[..parts.len() - 1].iter().all(|k| quiet.contains(&k.end)), "{parts:?}");
    }

    #[test]
    fn tight_split_with_no_signals_cuts_by_the_target_length() {
        let p = Params::default();
        let parts = tight_split(200.0, &[], &[], &[], &[], &p);
        assert_tight(&parts, 200.0, &p);
        assert_eq!(parts.len(), 5); // 40 + 40 + 40 + 40 + 40
        assert!(parts.iter().all(|k| (k.len() - 40.0).abs() < 1e-9));
    }

    #[test]
    fn tight_split_keeps_a_short_video_whole_and_halves_an_awkward_rest() {
        let p = Params::default();
        let short = tight_split(15.0, &[], &[], &[], &[], &p);
        assert_eq!((short.len(), short[0].start, short[0].end), (1, 0.0, 15.0));

        // остаток 30 с при max_len 25 и min_len 20: двух частей по 20 не получится, делим пополам
        let p = Params { min_len: 20.0, max_len: 25.0, ..Default::default() };
        let halves = tight_split(30.0, &[], &[], &[], &[], &p);
        assert_tight(&halves, 30.0, &p);
        assert_eq!(halves.len(), 2);
        assert!((halves[0].end - 15.0).abs() < 1e-9);
    }

    #[test]
    fn tight_split_holds_its_invariants_for_any_length() {
        let p = Params::default();
        for tenths in (1..6000).step_by(7) {
            let d = tenths as f64 / 10.0;
            let quiet: Vec<f64> = (0..(d / 0.5) as usize).filter(|i| i % 9 == 0).map(|i| i as f64 * 0.5 + 0.25).collect();
            let parts = tight_split(d, &sentences((d / 5.0) as usize, 5.0), &[d / 3.0], &quiet, &[], &p);
            assert_tight(&parts, d, &p);
            // короткий хвост не остаётся: кроме видео короче min_len, все части не короче min_len
            assert!(d < p.min_len || parts.iter().all(|k| k.len() >= p.min_len - 1e-9), "{d}: {parts:?}");
        }
    }
}
