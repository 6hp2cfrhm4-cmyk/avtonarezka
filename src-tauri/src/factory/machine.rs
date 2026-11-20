//! Сохраняемые стадии и безопасное возобновление от последней устойчивой границы.
pub const STAGES: &[&str] = &[
    "discovered",
    "queued",
    "fetching",
    "scouting",
    "selecting",
    "awaiting_selection",
    "drafting",
    "qc",
    "awaiting_approval",
    "auto_approved",
    "rendering_final",
    "captioning",
    "ready_to_schedule",
    "scheduled",
    "publishing",
    "published",
    "measured",
    "completed",
    "failed",
];
/// Публикации никогда не повторяются автоматически после неизвестного исхода.
pub fn recovery(stage: &str) -> &str {
    match stage {
        "fetching" | "scouting" | "selecting" => "queued",
        "drafting" | "qc" => "drafting",
        "auto_approved" | "rendering_final" | "captioning" => "rendering_final",
        "publishing" => "needs_check",
        other => other,
    }
}
/// Допустимые переходы учитывают устойчивые контрольные точки и смешанную пачку превью/финалов.
pub fn transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if to == "failed" {
        return !["published", "measured", "completed"].contains(&from);
    }
    match from {
        "discovered" => to == "queued",
        "queued" => ["fetching", "scouting", "selecting", "drafting"].contains(&to),
        "fetching" => ["queued", "scouting", "selecting", "drafting"].contains(&to),
        "scouting" => ["queued", "selecting"].contains(&to),
        "selecting" => ["queued", "drafting", "awaiting_selection"].contains(&to),
        "awaiting_selection" => ["drafting", "qc", "awaiting_approval"].contains(&to),
        "drafting" => [
            "qc",
            "awaiting_approval",
            "rendering_final",
            "ready_to_schedule",
            "completed",
        ]
        .contains(&to),
        "qc" => [
            "drafting",
            "auto_approved",
            "awaiting_approval",
            "rendering_final",
            "completed",
        ]
        .contains(&to),
        "auto_approved" => ["qc", "awaiting_approval", "rendering_final"].contains(&to),
        "awaiting_approval" => [
            "qc",
            "auto_approved",
            "rendering_final",
            "ready_to_schedule",
            "completed",
        ]
        .contains(&to),
        "rendering_final" => [
            "qc",
            "awaiting_approval",
            "ready_to_schedule",
            "captioning",
            "completed",
        ]
        .contains(&to),
        "ready_to_schedule" => to == "captioning" || to == "scheduled",
        "captioning" => [
            "scheduled",
            "ready_to_schedule",
            "awaiting_approval",
            "rendering_final",
        ]
        .contains(&to),
        "scheduled" => to == "publishing",
        "publishing" => to == "published" || to == "needs_check",
        "needs_check" => ["published", "queued", "scheduled", "publishing"].contains(&to),
        "published" => to == "measured",
        "failed" => to == "queued",
        _ => false,
    }
}
pub fn selected(scores: &[(i64, f64, f64)], threshold: f64, cap: usize, fixed: bool) -> Vec<i64> {
    let mut rows = scores.to_vec();
    rows.retain(|(_, score, _)| fixed || (score.is_finite() && *score >= threshold));
    rows.sort_by(|a, b| {
        if fixed {
            a.2.total_cmp(&b.2)
        } else {
            b.1.total_cmp(&a.1).then_with(|| a.2.total_cmp(&b.2))
        }
    });
    rows.truncate(cap);
    rows.into_iter().map(|r| r.0).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_preserves_approval_and_never_blindly_republishes() {
        for stage in ["fetching", "scouting", "selecting"] {
            assert_eq!(recovery(stage), "queued");
        }
        for stage in ["awaiting_approval", "scheduled", "published", "measured"] {
            assert_eq!(recovery(stage), stage);
        }
        assert_eq!(recovery("publishing"), "needs_check");
        assert_eq!(recovery("qc"), "drafting");
        assert_eq!(
            recovery("captioning"),
            "rendering_final",
            "готовые финалы повторно доходят до подписей; сохранённые варианты переиспользуются"
        );
    }
    #[test]
    fn transitions_reject_skipped_approval_and_regression_from_published() {
        for pair in [
            ("queued", "fetching"),
            ("scouting", "selecting"),
            ("selecting", "drafting"),
            ("drafting", "qc"),
            ("qc", "awaiting_approval"),
            ("qc", "auto_approved"),
            ("auto_approved", "rendering_final"),
            ("rendering_final", "ready_to_schedule"),
            ("captioning", "scheduled"),
            ("captioning", "rendering_final"),
            ("scheduled", "publishing"),
            ("publishing", "needs_check"),
        ] {
            assert!(transition(pair.0, pair.1), "{pair:?}");
        }
        assert!(!transition("queued", "published"));
        assert!(!transition("awaiting_approval", "scheduled"));
        assert!(!transition("published", "queued"));
        assert!(!transition("measured", "failed"));
    }
    #[test]
    fn automatic_selection_is_bounded_deterministic_and_honors_threshold() {
        let scores = vec![
            (1, 0.2, 0.0),
            (2, 0.9, 30.0),
            (3, 0.9, 60.0),
            (4, 0.7, 90.0),
        ];
        assert_eq!(selected(&scores, 0.7, 2, false), vec![2, 3]);
        assert_eq!(selected(&scores, 0.99, 5, false), Vec::<i64>::new());
        assert_eq!(
            selected(&scores, 0.9, 2, true),
            vec![1, 2],
            "сплошная нарезка использует время, а не выдуманную оценку"
        );
    }
}
