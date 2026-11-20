//! Детектор моментов эфира: чистая функция `step(state, params, sample)`.
//! Каждые несколько секунд приходит замер (сообщения чата, авторы, смайлы, звук, подсказки).
//! Отклонение от «обычного» за последние минуты считается робастно (медиана и MAD), поэтому
//! медленный рост онлайна момент не рождает, а резкий всплеск — да. Пик отслеживается до спада.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Один замер за интервал `Params::step_s`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// Секунды от начала записи (общая шкала с кольцом сегментов).
    pub t: f64,
    pub msgs: f64,
    pub authors: f64,
    /// Доля сообщений со смайлами/эмоутами, 0..1.
    pub emotes: f64,
    /// Громкость интервала, дБ (RMS).
    pub audio_db: f64,
    /// Сообщения с подсказками («клип», KEKW, «ахах»).
    pub hints: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Params {
    pub step_s: f64,
    pub window_s: f64,
    pub warmup_s: f64,
    pub z_on: f64,
    pub z_off: f64,
    /// Сколько замеров подряд выше порога нужно, чтобы считать это пиком.
    pub min_peak_samples: usize,
    pub pre_s: f64,
    pub post_s: f64,
    pub min_len_s: f64,
    pub max_len_s: f64,
    pub cooldown_s: f64,
    pub max_per_hour: usize,
    /// Есть ли чат. Без него решает только звук: порог достижим одним каналом.
    pub chat: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self { step_s: 5.0, window_s: 600.0, warmup_s: 120.0, z_on: 4.0, z_off: 1.5, min_peak_samples: 2, pre_s: 30.0, post_s: 10.0, min_len_s: 20.0, max_len_s: 90.0, cooldown_s: 60.0, max_per_hour: 6, chat: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub start: f64,
    pub end: f64,
    pub peak_t: f64,
    pub score: f64,
}

#[derive(Clone, Debug)]
struct Peak {
    start_t: f64,
    peak_t: f64,
    peak_z: f64,
    above: usize,
    calm: usize,
}

#[derive(Default, Debug)]
pub struct State {
    history: VecDeque<Sample>,
    peak: Option<Peak>,
    last_end: f64,
    fired: VecDeque<f64>,
    /// Последняя оценка отклонения (для ленты сигналов на экране).
    pub last_z: f64,
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Робастное отклонение значения от окна: (x − медиана) / max(1.4826·MAD, нижний предел).
fn robust_z(window: &[f64], x: f64, floor: f64) -> f64 {
    let mut w = window.to_vec();
    let m = median(&mut w);
    let mut dev: Vec<f64> = window.iter().map(|v| (v - m).abs()).collect();
    let mad = median(&mut dev);
    (x - m) / (1.4826 * mad).max(floor)
}

/// Взвешенная сумма положительных отклонений по каналам: падение громкости или чата моментом не считается.
pub fn deviation(history: &VecDeque<Sample>, s: &Sample, chat: bool) -> f64 {
    let col = |f: fn(&Sample) -> f64| history.iter().map(f).collect::<Vec<_>>();
    let z = |f: fn(&Sample) -> f64, x: f64, floor: f64| robust_z(&col(f), x, floor).clamp(0.0, 12.0);
    if !chat {
        return z(|s| s.audio_db, s.audio_db, 2.0);
    }
    0.35 * z(|s| s.msgs, s.msgs, 2.0) + 0.2 * z(|s| s.authors, s.authors, 1.5) + 0.1 * z(|s| s.emotes, s.emotes, 0.08) + 0.25 * z(|s| s.audio_db, s.audio_db, 2.0) + 0.1 * z(|s| s.hints, s.hints, 1.0)
}

/// Возвращает окно ролика, когда пик закончился. `end` может быть в будущем на `post_s`:
/// вызывающий ждёт, пока кольцо дорастёт до него.
pub fn step(st: &mut State, p: &Params, s: Sample) -> Option<Trigger> {
    let enough = st.history.len() as f64 * p.step_s >= p.warmup_s;
    // Разгон: пока истории мало, пиков не ищем. Иначе сравниваем с окном, где текущего замера ещё нет.
    let z = if enough { deviation(&st.history, &s, p.chat) } else { 0.0 };
    st.last_z = z;
    let mut out = None;

    match st.peak.take() {
        None => {
            if z >= p.z_on {
                st.peak = Some(Peak { start_t: s.t, peak_t: s.t, peak_z: z, above: 1, calm: 0 });
            }
        }
        Some(mut pk) => {
            if z >= p.z_on {
                pk.above += 1;
                pk.calm = 0;
                if z > pk.peak_z {
                    pk.peak_z = z;
                    pk.peak_t = s.t;
                }
            } else if z < p.z_off {
                pk.calm += 1;
            } else {
                pk.calm = 0; // между порогами: гистерезис, пик ещё жив
            }
            let too_long = s.t - pk.start_t >= p.max_len_s - p.pre_s - p.post_s;
            if pk.calm >= 2 || too_long {
                // Спад. Пик, который не продержался `min_peak_samples`, считаем случайным всплеском.
                if pk.above >= p.min_peak_samples {
                    let end_peak = s.t - pk.calm as f64 * p.step_s;
                    let mut start = pk.start_t.min(pk.peak_t) - p.pre_s;
                    let mut end = end_peak + p.post_s;
                    if end - start < p.min_len_s {
                        end = start + p.min_len_s;
                    }
                    if end - start > p.max_len_s {
                        start = end - p.max_len_s;
                    }
                    st.fired.retain(|t| s.t - t < 3600.0);
                    let allowed = s.t - st.last_end >= p.cooldown_s && st.fired.len() < p.max_per_hour;
                    if allowed {
                        st.last_end = end;
                        st.fired.push_back(s.t);
                        out = Some(Trigger { start: start.max(0.0), end, peak_t: pk.peak_t, score: pk.peak_z });
                    }
                }
            } else {
                st.peak = Some(pk);
            }
        }
    }

    // Окно «обычного» пополняется только вне пика: иначе всплеск сам себя нормализует.
    if st.peak.is_none() {
        st.history.push_back(s);
        let cap = (p.window_s / p.step_s).ceil() as usize;
        while st.history.len() > cap {
            st.history.pop_front();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Детерминированный шум без зависимостей.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f64) / (1u64 << 31) as f64
        }
    }

    fn calm(rng: &mut Lcg, t: f64) -> Sample {
        Sample { t, msgs: 8.0 + rng.next() * 4.0, authors: 5.0 + rng.next() * 3.0, emotes: 0.1 + rng.next() * 0.05, audio_db: -32.0 + rng.next() * 3.0, hints: rng.next().round() * 0.0 }
    }
    fn burst(rng: &mut Lcg, t: f64) -> Sample {
        Sample { t, msgs: 70.0 + rng.next() * 20.0, authors: 40.0 + rng.next() * 10.0, emotes: 0.6, audio_db: -20.0 + rng.next(), hints: 12.0 }
    }

    fn run(p: &Params, plan: &[(usize, bool)]) -> (Vec<Trigger>, Vec<f64>) {
        let mut st = State::default();
        let mut rng = Lcg(7);
        let mut t = 0.0;
        let mut out = vec![];
        let mut times = vec![];
        for &(n, loud) in plan {
            for _ in 0..n {
                let s = if loud { burst(&mut rng, t) } else { calm(&mut rng, t) };
                if let Some(tr) = step(&mut st, p, s) {
                    times.push(t);
                    out.push(tr);
                }
                t += p.step_s;
            }
        }
        (out, times)
    }

    #[test]
    fn calm_chat_never_fires() {
        let (out, _) = run(&Params::default(), &[(400, false)]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn burst_fires_once_with_window_around_peak() {
        let p = Params::default();
        // 150 спокойных замеров (750 с), 6 замеров всплеска (30 с), потом снова спокойно
        let (out, times) = run(&p, &[(150, false), (6, true), (60, false)]);
        assert_eq!(out.len(), 1, "{out:?}");
        let t = &out[0];
        let burst_start = 750.0;
        assert!(t.start <= burst_start - 25.0 && t.start >= burst_start - 35.0, "старт за ~30 с до пика: {t:?}");
        assert!(t.end >= burst_start + 25.0 + 10.0 - 0.1, "конец после спада + 10 с: {t:?}");
        assert!((p.min_len_s..=p.max_len_s).contains(&(t.end - t.start)), "{t:?}");
        assert!(t.score >= p.z_on);
        assert!(times[0] > burst_start + 30.0, "решение после спада, а не в середине пика");
    }

    #[test]
    fn single_spike_is_ignored() {
        let (out, _) = run(&Params::default(), &[(150, false), (1, true), (60, false)]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn warmup_prevents_early_triggers() {
        let (out, _) = run(&Params::default(), &[(5, false), (6, true), (20, false)]);
        assert!(out.is_empty());
    }

    #[test]
    fn cooldown_and_hourly_cap() {
        let p = Params { max_per_hour: 2, ..Params::default() };
        // три всплеска, разделённые 3 минутами спокойствия: третий упирается в потолок 2 в час
        let (out, _) = run(&p, &[(150, false), (6, true), (36, false), (6, true), (36, false), (6, true), (40, false)]);
        assert_eq!(out.len(), 2, "{out:?}");
        // два всплеска почти подряд: второй попадает в охлаждение
        let (out, _) = run(&Params::default(), &[(150, false), (6, true), (5, false), (6, true), (40, false)]);
        assert!(out.len() <= 1, "{out:?}");
    }

    #[test]
    fn long_peak_is_capped_to_max_len() {
        let p = Params::default();
        let (out, _) = run(&p, &[(150, false), (60, true), (40, false)]);
        assert!(!out.is_empty());
        assert!(out.iter().all(|t| t.end - t.start <= p.max_len_s + 1e-9), "{out:?}");
    }

    #[test]
    fn slow_growth_is_not_a_moment() {
        // онлайн растёт на 1 % за замер: окно успевает подстраиваться
        let mut st = State::default();
        let p = Params::default();
        let mut rng = Lcg(3);
        let mut fired = 0;
        for i in 0..600 {
            let k = 1.0 + 0.004 * i as f64;
            let s = Sample { t: i as f64 * p.step_s, msgs: (8.0 + rng.next() * 4.0) * k, authors: (5.0 + rng.next() * 3.0) * k, emotes: 0.1, audio_db: -32.0 + rng.next() * 3.0, hints: 0.0 };
            if step(&mut st, &p, s).is_some() {
                fired += 1;
            }
        }
        assert_eq!(fired, 0);
    }

    #[test]
    fn audio_only_mode_fires_on_loud_burst() {
        let p = Params { chat: false, ..Params::default() };
        let (with_chat, _) = run(&Params::default(), &[(150, false), (6, true), (60, false)]);
        let (audio_only, _) = run(&p, &[(150, false), (6, true), (60, false)]);
        assert_eq!((with_chat.len(), audio_only.len()), (1, 1));
        // без чата тихий эфир остаётся тихим
        assert!(run(&p, &[(300, false)]).0.is_empty());
    }

    #[test]
    fn drop_in_activity_is_not_a_peak() {
        let mut st = State::default();
        let p = Params::default();
        let mut rng = Lcg(5);
        for i in 0..150 {
            step(&mut st, &p, calm(&mut rng, i as f64 * 5.0));
        }
        let quiet = Sample { t: 750.0, msgs: 0.0, authors: 0.0, emotes: 0.0, audio_db: -60.0, hints: 0.0 };
        step(&mut st, &p, quiet);
        assert!(st.last_z < 0.5, "{}", st.last_z);
    }
}
