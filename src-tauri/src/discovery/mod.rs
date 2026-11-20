//! Автопоиск трендов. Здесь — чистая оценка кандидатов (`trend_score`) и правило автопринятия;
//! провайдеры (Twitch Helix, Kick, YouTube, Google Trends) подключаются поверх и отдают `Observation`.
pub mod providers;
pub mod runtime;
pub mod store;

use serde::{Deserialize, Serialize};

/// Один замер популярности источника (стрима, видео, клипа).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub platform: String,
    pub url: String,
    pub title: String,
    pub category: String,
    pub language: String,
    /// Зрители сейчас (для эфира) или просмотры (для записи).
    pub metric: f64,
    /// Тот же показатель `window_min` минут назад; 0 — неизвестно.
    pub previous: f64,
    pub window_min: f64,
    pub duration_min: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Taste {
    pub languages: Vec<String>,
    pub categories: Vec<String>,
    pub topics: Vec<String>,
    pub known_channels: Vec<String>,
    pub already_processed: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Scored {
    pub score: f64,
    pub why: String,
}

/// Оценка 0..1: рост за окно важнее абсолютного числа, затем совпадение с вашими темами и категориями.
pub fn trend_score(o: &Observation, t: &Taste) -> Scored {
    if t.already_processed {
        return Scored { score: 0.0, why: "уже обработано".into() };
    }
    if !t.languages.is_empty() && !t.languages.iter().any(|l| l.eq_ignore_ascii_case(&o.language)) {
        return Scored { score: 0.0, why: format!("язык {} не из списка", o.language) };
    }
    let growth = if o.previous > 0.0 { (o.metric - o.previous) / o.previous } else { 0.0 };
    let growth_part = (growth / 3.0).clamp(0.0, 1.0); // +300 % и больше — максимум
    let size_part = ((o.metric.max(1.0)).log10() / 5.0).clamp(0.0, 1.0); // 100 000 — максимум
    let cat = t.categories.iter().any(|c| c.eq_ignore_ascii_case(&o.category));
    let title = o.title.to_lowercase();
    let topic = t.topics.iter().any(|x| !x.is_empty() && title.contains(&x.to_lowercase()));
    let known = t.known_channels.iter().any(|c| o.url.contains(c.as_str()));
    let mut score = 0.45 * growth_part + 0.2 * size_part + if cat { 0.15 } else { 0.0 } + if topic { 0.1 } else { 0.0 } + if known { 0.1 } else { 0.0 };
    if let Some(d) = o.duration_min {
        if !(0.5..=240.0).contains(&d) {
            score *= 0.5;
        }
    }
    let mut why = Vec::new();
    if o.previous > 0.0 {
        why.push(format!("{:+.0} % за {:.0} мин", growth * 100.0, o.window_min));
    }
    if cat {
        why.push(format!("категория {}", o.category));
    }
    if topic {
        why.push("совпала тема".into());
    }
    if known {
        why.push("канал из вашего списка".into());
    }
    Scored { score: score.clamp(0.0, 1.0), why: why.join(", ") }
}

/// Автопринятие: оценка выше порога и канал разрешён (белый список или owned/agreed); иначе только в Inbox.
pub fn auto_accept(score: f64, threshold: f64, whitelisted: bool, permission: &str, today: u32, daily_limit: u32) -> bool {
    score >= threshold && today < daily_limit && (whitelisted || matches!(permission, "owned" | "agreed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(metric: f64, previous: f64) -> Observation {
        Observation { platform: "twitch".into(), url: "https://twitch.tv/x".into(), title: "Финал турнира".into(), category: "Dota 2".into(), language: "ru".into(), metric, previous, window_min: 30.0, duration_min: None }
    }
    fn taste() -> Taste {
        Taste { languages: vec!["ru".into()], categories: vec!["dota 2".into()], topics: vec!["турнир".into()], known_channels: vec![], already_processed: false }
    }

    #[test]
    fn growth_beats_raw_size() {
        let hot = trend_score(&obs(3000.0, 600.0), &taste());
        let big = trend_score(&obs(50000.0, 50000.0), &taste());
        assert!(hot.score > big.score, "{hot:?} {big:?}");
        assert!(hot.why.contains("+400 %") && hot.why.contains("совпала тема"));
    }

    #[test]
    fn filters_language_and_processed() {
        let mut o = obs(1000.0, 100.0);
        o.language = "en".into();
        assert_eq!(trend_score(&o, &taste()).score, 0.0);
        let mut t = taste();
        t.already_processed = true;
        assert_eq!(trend_score(&obs(1000.0, 100.0), &t).score, 0.0);
    }

    #[test]
    fn odd_duration_is_penalised() {
        let mut o = obs(3000.0, 600.0);
        let normal = trend_score(&o, &taste()).score;
        o.duration_min = Some(600.0);
        assert!(trend_score(&o, &taste()).score < normal);
    }

    #[test]
    fn auto_accept_needs_rights_and_budget() {
        assert!(auto_accept(0.8, 0.7, true, "unspecified", 0, 5));
        assert!(auto_accept(0.8, 0.7, false, "owned", 0, 5));
        assert!(!auto_accept(0.8, 0.7, false, "unspecified", 0, 5));
        assert!(!auto_accept(0.6, 0.7, true, "owned", 0, 5));
        assert!(!auto_accept(0.8, 0.7, true, "owned", 5, 5));
    }
}
