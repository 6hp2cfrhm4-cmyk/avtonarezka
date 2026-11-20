//! Снимок признаков исходного момента. Правки профиля не переписывают историю.
use crate::{
    analyze::{Signals, HOP},
    factory::profiles::Snapshot,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Features {
    pub signals: BTreeMap<String, f64>,
    pub llm_score: f64,
    pub signal_score: f64,
    pub length: f64,
    pub template: String,
}
fn mean(values: &[f32], start: f64, end: f64) -> Option<f64> {
    let a = ((start / HOP).floor().max(0.) as usize).min(values.len());
    let b = ((end / HOP).ceil().max(0.) as usize).min(values.len());
    let values = values.get(a..b)?;
    let mut n = 0;
    let sum = values
        .iter()
        .filter(|v| v.is_finite())
        .map(|v| {
            n += 1;
            *v as f64
        })
        .sum::<f64>();
    (n > 0).then(|| sum / n as f64)
}
pub(crate) fn capture(db: &Connection, draft: i64) -> Result<(), String> {
    let (project,start,end,llm,signal):(i64,f64,f64,f64,f64)=db.query_row("SELECT m.project_id,m.start,m.\"end\",m.llm_score,m.signal_score FROM drafts d JOIN moments m ON m.id=d.moment_id WHERE d.id=?1",[draft],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|e|e.to_string())?;
    let raw: Option<String> = db
        .query_row(
            "SELECT profile_json FROM factory_items WHERE project_id=?1 ORDER BY id DESC LIMIT 1",
            [project],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let profile = raw.and_then(|s| serde_json::from_str::<Snapshot>(&s).ok());
    let signals = db
        .query_row::<String, _, _>(
            "SELECT signals_json FROM analysis WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )
        .ok()
        .and_then(|s| serde_json::from_str::<Signals>(&s).ok());
    let mut values = BTreeMap::new();
    if let Some(s) = signals {
        for (name, series) in [
            ("loudness", Some(s.loudness.as_slice())),
            ("motion", Some(s.motion.as_slice())),
            ("events", s.events.as_deref()),
            ("speech", s.speech.as_deref()),
            ("chat", s.chat.as_deref()),
            ("viewer_clips", s.viewer_clips.as_deref()),
            ("camera", s.camera.as_deref()),
        ] {
            if let Some(v) = series.and_then(|s| mean(s, start, end)) {
                values.insert(name.into(), v);
            }
        }
    }
    let features = Features {
        signals: values,
        llm_score: llm,
        signal_score: signal,
        length: end - start,
        template: profile
            .as_ref()
            .map(|p| p.template.layout.clone())
            .unwrap_or_else(|| "gameplay_stack".into()),
    };
    db.execute("INSERT OR IGNORE INTO moment_features(draft_id,channel_id,template_id,features)VALUES(?1,?2,?3,?4)",params![draft,profile.as_ref().map(|p|p.channel.id).filter(|n|*n>0),profile.as_ref().map(|p|p.template.id).filter(|n|*n>0),serde_json::to_string(&features).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
    Ok(())
}

pub fn median(values: &[f64]) -> Option<f64> {
    let mut v: Vec<_> = values.iter().copied().filter(|n| n.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        None
    } else if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        Some((v[n / 2 - 1] + v[n / 2]) / 2.)
    }
}
pub fn correlation(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 20 {
        return None;
    }
    let n = pairs.len() as f64;
    let x = pairs.iter().map(|p| p.0).sum::<f64>() / n;
    let y = pairs.iter().map(|p| p.1).sum::<f64>() / n;
    let numerator = pairs.iter().map(|p| (p.0 - x) * (p.1 - y)).sum::<f64>();
    let denominator = (pairs.iter().map(|p| (p.0 - x).powi(2)).sum::<f64>()
        * pairs.iter().map(|p| (p.1 - y).powi(2)).sum::<f64>())
    .sqrt();
    (denominator > 1e-9)
        .then(|| numerator / denominator)
        .filter(|n| n.is_finite())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stats_ignore_missing_and_preserve_zero() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[0., 2., 4., 6.]), Some(3.));
        assert_eq!(median(&[f64::NAN, 0.]), Some(0.));
        assert_eq!(correlation(&vec![(1., 1.); 50]), None);
        assert!(
            (correlation(
                &(0..50)
                    .map(|n| (n as f64, n as f64 * 2.))
                    .collect::<Vec<_>>()
            )
            .unwrap()
                - 1.)
                .abs()
                < 1e-6
        );
    }
    #[test]
    fn clip_signals_use_absolute_bounds() {
        assert_eq!(mean(&[0., 0., 2., 4., 9.], 1., 2.), Some(3.));
        assert_eq!(mean(&[], 1., 2.), None);
    }
}
