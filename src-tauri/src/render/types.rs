use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum TopFit {
    /// Горизонтальный исходник (16:9 и подобные) вписывается целиком, остальные заполняют область.
    #[default]
    Auto,
    /// Заполнить область с обрезкой краёв.
    Fill,
    /// Вписать целиком, пустое место — размытая копия кадра.
    Contain,
}

/// Начиная с какого отношения ширины к высоте исходник считается горизонтальным и показывается целиком.
/// Область под верх уже 16:9 (1080×960 — это 1,125), поэтому 1,2 отделяет «широкие» кадры от вертикальных и квадратных.
pub const WIDE_ASPECT: f64 = 1.2;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum BannerAnim {
    None,
    Fade,
    SlideUp,
    #[default]
    Pop,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct BannerStyle {
    pub enabled: bool,
    /// Цвет фона баннера, RRGGBB.
    pub key_color: String,
    pub similarity: f64,
    pub blend: f64,
    /// Убрать зелёный ореол по краям.
    pub despill: bool,
    /// Ширина баннера в долях ширины кадра.
    pub width_pct: f64,
    /// Смещение от центра линии раздела в долях ширины/высоты кадра.
    pub offset_x: f64,
    pub offset_y: f64,
    pub anim: BannerAnim,
    /// Ограничить показ баннера, секунд.
    pub max_seconds: Option<f64>,
}

impl Default for BannerStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            key_color: "00ff00".into(),
            similarity: 0.18,
            blend: 0.08,
            despill: true,
            width_pct: 0.72,
            offset_x: 0.0,
            offset_y: 0.0,
            anim: BannerAnim::Pop,
            max_seconds: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct SubtitleStyle {
    pub enabled: bool,
    pub font: String,
    /// Размер в пикселях при высоте кадра 1920.
    pub size: u32,
    pub color: String,
    pub highlight: String,
    pub outline_color: String,
    pub outline: f64,
    pub uppercase: bool,
    pub words_per_line: usize,
    /// Увеличение подсвеченного слова, %.
    pub highlight_scale: u32,
    /// Положение по вертикали в долях высоты; `None` — на линии раздела.
    pub y_pct: Option<f64>,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            font: "Montserrat ExtraBold".into(),
            size: 84,
            color: "ffffff".into(),
            highlight: "b6ff3c".into(),
            outline_color: "000000".into(),
            outline: 7.0,
            uppercase: true,
            words_per_line: 3,
            highlight_scale: 112,
            y_pct: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct PartLabelStyle {
    pub enabled: bool,
    /// `{n}` заменяется номером части.
    pub template: String,
    pub font: String,
    pub size: u32,
    pub color: String,
    pub outline_color: String,
    pub outline: f64,
    pub y_pct: f64,
}

impl Default for PartLabelStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            template: "Часть {n}".into(),
            font: "Montserrat Black".into(),
            size: 76,
            color: "ffffff".into(),
            outline_color: "000000".into(),
            outline: 6.0,
            y_pct: 0.11,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioMix {
    /// Звук исходного видео.
    pub main: f64,
    pub banner: f64,
    pub gameplay: f64,
    pub music: f64,
    /// Выровнять громкость до −14 LUFS (только в финале).
    pub loudnorm: bool,
}

impl Default for AudioMix {
    fn default() -> Self {
        Self { main: 1.0, banner: 1.0, gameplay: 0.0, music: 0.18, loudnorm: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Style {
    /// Доля высоты кадра под верхнее видео.
    pub split: f64,
    pub top_fit: TopFit,
    /// Какую долю высоты исходника срезать снизу до вписывания в кадр. Прячет субтитры, вшитые в само видео:
    /// при баннере верх замирает, и вшитые субтитры остались бы на замороженном кадре.
    /// По умолчанию 0: кадр виден целиком.
    pub top_crop_bottom: f64,
    /// Замирает ли геймплей, пока играет баннер.
    pub gameplay_freeze: bool,
    pub banner: BannerStyle,
    pub subtitles: SubtitleStyle,
    pub part_label: PartLabelStyle,
    pub audio: AudioMix,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            split: 0.5,
            top_fit: TopFit::Auto,
            top_crop_bottom: 0.0,
            gameplay_freeze: false,
            banner: BannerStyle::default(),
            subtitles: SubtitleStyle::default(),
            part_label: PartLabelStyle::default(),
            audio: AudioMix::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Profile {
    /// 540×960, быстро — для проверки.
    Draft,
    /// 1080×1920, для публикации.
    Final,
}

impl Profile {
    pub fn size(self) -> (u32, u32) {
        match self {
            Profile::Draft => (540, 960),
            Profile::Final => (1080, 1920),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Encoder {
    Nvenc,
    X264,
}

/// Кто декодирует видеовходы: процессор или NVDEC на видеокарте NVIDIA.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decode {
    #[default]
    Cpu,
    Nvidia,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MediaRef {
    pub path: PathBuf,
    pub duration: f64,
    pub has_audio: bool,
}

/// Слово с таймкодами в секундах исходного видео.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RenderJob {
    /// Кто декодирует видео. Кадры всё равно возвращаются в память: фильтры наложения пока на процессоре.
    #[serde(default)]
    pub decode: Decode,
    pub source: MediaRef,
    /// Ширина исходника, делённая на высоту; 0 — неизвестно.
    #[serde(default)]
    pub source_aspect: f64,
    pub start: f64,
    pub end: f64,
    pub banner: Option<MediaRef>,
    /// Секунда внутри клипа (от `start`), на которой верх замирает и появляется баннер.
    pub banner_at: Option<f64>,
    pub gameplay: MediaRef,
    pub gameplay_offset: f64,
    pub music: Option<MediaRef>,
    pub music_offset: f64,
    pub words: Vec<Word>,
    pub part: Option<u32>,
    pub style: Style,
    pub profile: Profile,
    pub encoder: Encoder,
    pub out: PathBuf,
}

impl RenderJob {
    pub fn clip_len(&self) -> f64 {
        (self.end - self.start).max(0.1)
    }

    /// Как верх ложится в свою область. `Auto` решает по форме кадра, который останется после среза снизу.
    pub fn top_fit(&self) -> TopFit {
        match self.style.top_fit {
            TopFit::Auto => {
                let kept = 1.0 - self.style.top_crop_bottom.clamp(0.0, 0.4);
                if self.source_aspect / kept >= WIDE_ASPECT { TopFit::Contain } else { TopFit::Fill }
            }
            fixed => fixed,
        }
    }

    /// Параметры заморозки `(tb, D)`, если баннер реально будет показан.
    pub fn freeze(&self) -> Option<(f64, f64)> {
        let banner = self.banner.as_ref()?;
        if !self.style.banner.enabled {
            return None;
        }
        let at = self.banner_at?;
        let len = self.clip_len();
        let mut d = banner.duration;
        if let Some(max) = self.style.banner.max_seconds {
            d = d.min(max);
        }
        if d < 0.2 || len < 1.0 {
            return None;
        }
        Some((at.clamp(0.2, len - 0.2), d))
    }

    /// Длина готового ролика: клип плюс время, пока играет баннер.
    pub fn out_len(&self) -> f64 {
        self.clip_len() + self.freeze().map_or(0.0, |(_, d)| d)
    }
}
