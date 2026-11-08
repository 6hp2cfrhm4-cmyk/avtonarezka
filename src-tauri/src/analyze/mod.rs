//! Поиск интересных моментов: сигналы звука и картинки → кривая интереса → кандидаты.

pub mod audio;
pub mod events;
pub mod gpu;
pub mod llm;
pub mod pipeline;
pub mod speech;
pub mod score;
pub mod video;

use serde::{Deserialize, Serialize};

/// Шаг временной сетки всех кривых, секунд.
pub const HOP: f64 = 0.5;

/// Кривые по времени. Каждая — оценка в z-единицах на отрезок `HOP`, все одной длины.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Signals {
    pub loudness: Vec<f32>,
    pub motion: Vec<f32>,
    /// Смех, крики, аплодисменты и т. п. (YAMNet), если модель была запущена.
    pub events: Option<Vec<f32>>,
    /// Слов в секунду (Whisper), если распознавание было.
    pub speech: Option<Vec<f32>>,
    /// Моменты смены сцен, секунды.
    pub cuts: Vec<f64>,
}

/// Законченная фраза с таймкодами в секундах исходного видео.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sentence {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Candidate {
    pub start: f64,
    pub end: f64,
    /// Итоговая оценка 0..1 (пока только сигналы).
    pub score: f64,
    /// Лучшая секунда для баннера, абсолютное время в исходнике.
    pub climax: f64,
}

impl Candidate {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}
