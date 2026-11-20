//! Наблюдательные подсказки. Никогда не меняет веса без явного применения.
use super::{
    features::{self, Features},
    *,
};
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub struct Outcome {
    pub draft: Option<i64>,
    pub account: i64,
    pub channel: Option<i64>,
    pub views: f64,
    pub features: Option<Features>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub signal: String,
    pub current: f64,
    pub proposed: f64,
    pub correlation: f64,
    pub sample_size: usize,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feedback {
    pub channel_id: i64,
    pub clips: usize,
    pub reason: String,
    pub suggestions: Vec<Suggestion>,
}
fn outcomes(db: &Connection) -> Result<Vec<Outcome>> {
    // Первый замер не ранее 48ч, не позднее 54ч. Поздние просмотры не выдаются за 48ч.
    let mut q=db.prepare("SELECT j.draft_id,COALESCE(j.target_account_id,j.account_id),mf.channel_id,mt.views,mf.features FROM publications j LEFT JOIN moment_features mf ON mf.draft_id=j.draft_id JOIN metrics mt ON mt.id=(SELECT mm.id FROM metrics mm WHERE mm.publication_id=j.id AND mm.views IS NOT NULL AND mm.observed_at BETWEEN j.sent_at+172800 AND j.sent_at+194400 ORDER BY mm.observed_at,mm.source DESC,mm.id DESC LIMIT 1) WHERE j.state='done' AND COALESCE(j.target_account_id,j.account_id) IS NOT NULL").map_err(err)?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(err)?;
    rows.map(|r| {
        let (draft, account, channel, views, raw) = r.map_err(err)?;
        Ok(Outcome {
            draft,
            account,
            channel,
            views,
            features: raw
                .map(|raw| serde_json::from_str(&raw).map_err(err))
                .transpose()?,
        })
    })
    .collect()
}
pub fn normalized(input: &[Outcome], channel: i64) -> BTreeMap<i64, (Features, f64)> {
    let mut accounts: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for o in input {
        accounts.entry(o.account).or_default().push(o.views);
    }
    let baselines: BTreeMap<_, _> = accounts
        .into_iter()
        .filter(|(_, v)| v.len() >= 5)
        .filter_map(|(a, v)| features::median(&v).filter(|n| *n > 0.).map(|m| (a, m)))
        .collect();
    let mut clips: BTreeMap<i64, (Features, Vec<f64>)> = BTreeMap::new();
    for o in input.iter().filter(|o| o.channel == Some(channel)) {
        if let (Some(m), Some(draft), Some(features)) =
            (baselines.get(&o.account), o.draft, o.features.as_ref())
        {
            clips
                .entry(draft)
                .or_insert_with(|| (features.clone(), vec![]))
                .1
                .push(o.views / m);
        }
    }
    clips
        .into_iter()
        .filter_map(|(id, (f, v))| features::median(&v).map(|n| (id, (f, n))))
        .collect()
}
pub fn propose(input: &[Outcome], channel: i64, weights: &BTreeMap<String, f64>) -> Feedback {
    let clips = normalized(input, channel);
    let count = clips.len();
    let mut suggestions = vec![];
    if count >= 50 {
        for (signal, current) in weights {
            let pairs: Vec<_> = clips
                .values()
                .filter_map(|(f, n)| f.signals.get(signal).map(|v| (*v, n.ln_1p())))
                .collect();
            if let Some(c) = features::correlation(&pairs).filter(|c| c.abs() >= 0.3) {
                let proposed = (current + if c > 0. { 0.1 } else { -0.1 }).clamp(0., 1.);
                if (proposed - current).abs() > 0.01 {
                    suggestions.push(Suggestion {
                        signal: signal.clone(),
                        current: *current,
                        proposed,
                        correlation: c,
                        sample_size: pairs.len(),
                    });
                }
            }
        }
    }
    let reason = if count < 50 {
        format!("Нужно 50 разных роликов с замером в 48–54 часа. Сейчас: {count}. Для нормировки нужно от 5 замеров на аккаунт и ненулевая медиана.")
    } else if suggestions.is_empty() {
        "Достаточно замеров, устойчивой связи сигналов с результатом пока нет.".into()
    } else {
        "Связь с просмотрами не доказывает причину. Это подсказки для следующей обработки; сохранённые профили заданий не меняются.".into()
    };
    Feedback {
        channel_id: channel,
        clips: count,
        reason,
        suggestions,
    }
}
fn weights(c: &crate::factory::profiles::Channel) -> BTreeMap<String, f64> {
    serde_json::from_value(serde_json::to_value(&c.analysis).unwrap_or_default()["weights"].clone())
        .unwrap_or_else(|_| {
            serde_json::from_value(
                serde_json::to_value(crate::analyze::score::Weights::default()).unwrap(),
            )
            .unwrap()
        })
}
pub(crate) fn read(db: &Connection, channel: i64) -> Result<Feedback> {
    let c = crate::factory::profiles::channels(db)?
        .into_iter()
        .find(|c| c.id == channel)
        .ok_or("Канал не найден")?;
    Ok(propose(&outcomes(db)?, channel, &weights(&c)))
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analytics_feedback(state: State<'_, AppState>, channel_id: i64) -> Result<Feedback> {
    read(&state.db.0.lock().unwrap(), channel_id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analytics_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    channel_id: i64,
    signal: String,
    current: f64,
) -> Result<()> {
    let db = state.db.0.lock().unwrap();
    let suggestion = read(&db, channel_id)?
        .suggestions
        .into_iter()
        .find(|s| s.signal == signal && (s.current - current).abs() < 1e-5)
        .ok_or("Подсказка устарела. Обнови данные перед применением")?;
    let mut c = crate::factory::profiles::channels(&db)?
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or("Канал не найден")?;
    let mut options = serde_json::to_value(&c.analysis).map_err(err)?;
    let mut w = weights(&c);
    w.insert(signal.clone(), suggestion.proposed);
    options["weights"] = serde_json::to_value(w).map_err(err)?;
    c.analysis = serde_json::from_value(options).map_err(err)?;
    crate::factory::profiles::save_channel(&db, c)?;
    db.execute(
        "INSERT INTO events(channel_id,kind,message)VALUES(?1,'settings',?2)",
        params![
            channel_id,
            format!(
                "По запросу пользователя вес {signal}: {:.2} → {:.2}",
                suggestion.current, suggestion.proposed
            )
        ],
    )
    .map_err(err)?;
    drop(db);
    let _ = app.emit("factory-changed", ());
    let _ = app.emit("analytics-changed", ());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn data(n: i64) -> Vec<Outcome> {
        (0..n)
            .flat_map(|i| {
                [
                    Outcome {
                        draft: Some(i),
                        account: 1,
                        channel: Some(1),
                        views: 100. + i as f64 * 10.,
                        features: Some(Features {
                            signals: BTreeMap::from([("chat".into(), i as f64)]),
                            llm_score: 0.,
                            signal_score: 0.,
                            length: 30.,
                            template: "reaction_pip".into(),
                        }),
                    },
                    Outcome {
                        draft: Some(i),
                        account: 2,
                        channel: Some(1),
                        views: (100. + i as f64 * 10.) * 100.,
                        features: Some(Features {
                            signals: BTreeMap::from([("chat".into(), i as f64)]),
                            llm_score: 0.,
                            signal_score: 0.,
                            length: 30.,
                            template: "reaction_pip".into(),
                        }),
                    },
                ]
            })
            .collect()
    }
    #[test]
    fn normalization_removes_account_scale_without_counting_a_clip_twice() {
        let d = data(50);
        let v = normalized(&d, 1);
        assert_eq!(v.len(), 50);
        let expected = 100. / 345.;
        assert!((v[&0].1 - expected).abs() < 1e-6);
        assert!(normalized(&d, 2).is_empty());
    }
    #[test]
    fn threshold_zero_baseline_and_explicit_proposal() {
        let w = BTreeMap::from([("chat".into(), 0.15)]);
        assert!(propose(&data(49), 1, &w).suggestions.is_empty());
        let p = propose(&data(50), 1, &w);
        assert_eq!(p.clips, 50);
        assert_eq!(p.suggestions.len(), 1);
        assert!((p.suggestions[0].proposed - 0.25).abs() < 1e-6);
        let mut zero = data(50);
        for o in &mut zero {
            o.views = 0.;
        }
        assert_eq!(propose(&zero, 1, &w).clips, 0);
    }
    #[test]
    fn older_and_imported_posts_contribute_to_account_median_without_becoming_featured_clips() {
        let mut all = data(49);
        let before = normalized(&all, 1)[&0].1;
        all.extend((0..30).map(|_| Outcome {
            draft: None,
            account: 1,
            channel: None,
            views: 0.,
            features: None,
        }));
        let after = normalized(&all, 1);
        assert_eq!(after.len(), 49);
        assert!(after[&0].1 > before);
        assert!(propose(&all, 1, &BTreeMap::from([("chat".into(), 0.15)]))
            .suggestions
            .is_empty());
    }
}
