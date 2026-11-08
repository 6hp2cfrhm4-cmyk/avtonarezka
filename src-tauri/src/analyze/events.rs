//! Звуковые события (YAMNet на `tract`): смех, крики, толпа, удары.
//! Это то, что отличает «забавный момент» от ровной речи.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use anyhow::{bail, Context};
use tract_onnx::prelude::*;

use super::HOP;

const SR: usize = 16_000;
const WIN: usize = 15_600; // 0.975 с — окно YAMNet
const STEP: usize = 7_680; // 0.48 с — шаг кадров
const FRAMES_PER_CHUNK: usize = 64;
const CHUNK_LEN: usize = WIN + STEP * (FRAMES_PER_CHUNK - 1);
const N_CLASSES: usize = 521;

/// Группы событий и их вес в интересе. Названия — из `yamnet_class_map.csv`.
const GROUPS: &[(f32, &[&str])] = &[
    (1.0, &["Laughter", "Giggle", "Chuckle, chortle", "Belly laugh", "Snicker", "Baby laughter"]),
    (0.9, &["Screaming", "Shout", "Yell", "Whoop", "Children shouting", "Bellow"]),
    (0.8, &["Cheering", "Applause", "Clapping", "Crowd"]),
    (0.6, &["Explosion", "Gunshot, gunfire", "Machine gun", "Boom", "Smash, crash"]),
    (0.5, &["Gasp", "Groan", "Whistling"]),
];

type Run = Box<dyn Fn(&[f32]) -> anyhow::Result<Vec<f32>> + Send + Sync>;

pub struct EventModel {
    run: Run,
    groups: Vec<(f32, Vec<usize>)>,
}

/// Названия классов по индексу. Третье поле может быть в кавычках и содержать запятые.
pub fn parse_classes(csv: &str) -> Vec<String> {
    csv.lines()
        .skip(1)
        .filter_map(|l| l.trim_end_matches('\r').splitn(3, ',').nth(2))
        .map(|n| n.trim_matches('"').to_string())
        .collect()
}

impl EventModel {
    pub fn load(onnx: &Path, classes_csv: &Path) -> anyhow::Result<Self> {
        let classes = parse_classes(&std::fs::read_to_string(classes_csv).context("нет карты классов YAMNet")?);
        if classes.len() != N_CLASSES {
            bail!("в карте классов {} строк, ждали {N_CLASSES}", classes.len());
        }
        let groups = GROUPS
            .iter()
            .map(|(w, names)| {
                let idx = names
                    .iter()
                    .map(|n| classes.iter().position(|c| c == n).with_context(|| format!("в YAMNet нет класса «{n}»")))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok((*w, idx))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let plan = tract_onnx::onnx()
            .model_for_path(onnx)
            .context("не удалось прочитать модель YAMNet")?
            .with_input_fact(0, f32::fact([CHUNK_LEN]).into())?
            .into_optimized()?
            .into_runnable()?;
        let run: Run = Box::new(move |wav| {
            let input = tract_ndarray::Array1::from_vec(wav.to_vec()).into_tensor();
            let out = plan.run(tvec!(input.into()))?;
            let scores = out[0].to_array_view::<f32>()?;
            Ok(scores.iter().copied().collect())
        });
        Ok(Self { run, groups })
    }

    /// Балл события 0..1 для одного кадра из 521 вероятности.
    fn frame_score(&self, scores: &[f32]) -> f32 {
        self.groups
            .iter()
            .map(|(w, idx)| w * idx.iter().map(|&i| scores[i]).fold(0.0, f32::max))
            .fold(0.0, f32::max)
    }

    /// Кривая событий на сетке `HOP` в единицах 0..4 (как z-оценки остальных сигналов).
    pub fn scan(&self, pcm: &[i16], threads: usize, progress: impl Fn(f64) + Sync) -> anyhow::Result<Vec<f32>> {
        let n_frames = if pcm.len() < WIN { 1 } else { (pcm.len() - WIN) / STEP + 1 };
        let n_chunks = n_frames.div_ceil(FRAMES_PER_CHUNK);
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let results: Mutex<Vec<(usize, Vec<f32>)>> = Mutex::new(Vec::new());
        let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);

        std::thread::scope(|s| {
            for _ in 0..threads.clamp(1, n_chunks) {
                s.spawn(|| loop {
                    let c = next.fetch_add(1, Ordering::Relaxed);
                    if c >= n_chunks || failure.lock().unwrap().is_some() {
                        return;
                    }
                    let from = c * FRAMES_PER_CHUNK * STEP;
                    let mut wav = vec![0.0f32; CHUNK_LEN];
                    for (dst, &src) in wav.iter_mut().zip(pcm.iter().skip(from)) {
                        *dst = src as f32 / 32768.0;
                    }
                    match (self.run)(&wav) {
                        Ok(flat) => {
                            let valid = FRAMES_PER_CHUNK.min(n_frames - c * FRAMES_PER_CHUNK);
                            let scores = flat.chunks_exact(N_CLASSES).take(valid).map(|f| self.frame_score(f)).collect();
                            results.lock().unwrap().push((c, scores));
                            progress((done.fetch_add(1, Ordering::Relaxed) + 1) as f64 / n_chunks as f64);
                        }
                        Err(e) => {
                            *failure.lock().unwrap() = Some(e);
                            return;
                        }
                    }
                });
            }
        });
        if let Some(e) = failure.into_inner().unwrap() {
            return Err(e);
        }
        let mut chunks = results.into_inner().unwrap();
        chunks.sort_by_key(|(c, _)| *c);
        let frames: Vec<f32> = chunks.into_iter().flat_map(|(_, v)| v).collect();
        Ok(frames_to_hops(&frames, (pcm.len() as f64 / SR as f64 / HOP).ceil() as usize))
    }
}

/// Кадр `i` покрывает [i·0.48, i·0.48 + 0.975] с; для каждого отрезка берём ближайший по центру.
pub fn frames_to_hops(frames: &[f32], n_hops: usize) -> Vec<f32> {
    if frames.is_empty() {
        return vec![0.0; n_hops];
    }
    let (step, centre0) = (STEP as f64 / SR as f64, WIN as f64 / SR as f64 / 2.0);
    (0..n_hops)
        .map(|h| {
            let t = (h as f64 + 0.5) * HOP;
            let f = (((t - centre0) / step).round().max(0.0) as usize).min(frames.len() - 1);
            4.0 * frames[f]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_names_with_commas_stay_whole() {
        let csv = "index,mid,display_name\r\n0,/m/09x0r,Speech\r\n1,/m/0ytgt,\"Child speech, kid speaking\"\r\n";
        assert_eq!(parse_classes(csv), vec!["Speech", "Child speech, kid speaking"]);
    }

    #[test]
    fn frames_map_onto_the_half_second_grid() {
        // кадр 0 центрован на 0.4875 с, кадр 10 — на 5.2875 с
        let mut frames = vec![0.0; 20];
        frames[10] = 1.0;
        let hops = frames_to_hops(&frames, 20);
        let peak = hops.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert!((10..=11).contains(&peak), "{peak}"); // 5.25 и 5.75 с — ближайшие к 5.29
        assert!((hops[peak] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn no_frames_means_a_flat_curve() {
        assert_eq!(frames_to_hops(&[], 3), vec![0.0; 3]);
    }

    #[test]
    fn chunk_geometry_matches_yamnet() {
        assert_eq!(CHUNK_LEN, WIN + STEP * 63);
        // следующий чанк начинается там, где кончились кадры предыдущего
        assert_eq!(FRAMES_PER_CHUNK * STEP, 491_520);
    }
}
