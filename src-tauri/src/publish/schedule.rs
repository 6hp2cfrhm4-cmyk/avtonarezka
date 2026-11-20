//! Слоты используют те же окна и суточные лимиты, что прежний next_action.
//! Учёт будущих броней не даёт автозаполнению занять один слот дважды.
use crate::publish_queue::{next_action, Action, Plan, QueuedJob, Turn};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Policy {
    pub pause_min: i64,
    pub daily_limit: i64,
    pub window_start: i64,
    pub window_end: i64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            pause_min: 45,
            daily_limit: 5,
            window_start: 0,
            window_end: 1440,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=1440).contains(&self.pause_min)
            || !(1..=100).contains(&self.daily_limit)
            || !(0..=1440).contains(&self.window_start)
            || !(0..=1440).contains(&self.window_end)
        {
            return Err("Проверь паузу, суточный лимит и окно публикаций".into());
        }
        Ok(())
    }
}
pub fn next_slot(policy: &Policy, from: i64, offset: i64, reserved: &[i64]) -> Result<i64, String> {
    policy.validate()?;
    let mut at = from;
    let deadline = from.saturating_add(366 * 86400);
    // Каждый проход либо перескакивает сутки/окно, либо одну существующую бронь.
    for _ in 0..reserved.len().saturating_add(740) {
        if at > deadline {
            return Err("Нет свободного слота в ближайший год".into());
        }
        let day = at - (at + offset).rem_euclid(86400);
        let sent = reserved
            .iter()
            .copied()
            .filter(|t| *t >= day && *t < day + 86400)
            .collect();
        let plan = Plan {
            now: at,
            offset,
            window_start: policy.window_start,
            window_end: policy.window_end,
            daily_limit: policy.daily_limit,
            accounts: vec![Turn { id: 1, sent }],
            last: None,
        };
        match next_action(
            &plan,
            &[QueuedJob {
                id: 1,
                not_before: at,
                wanted: Some(1),
            }],
        ) {
            Action::OutsideWindow(next) | Action::LimitReached(next) | Action::WaitUntil(next) => {
                at = next.max(at + 1);
                continue;
            }
            Action::Send { .. } => {}
            _ => return Err("Не удалось подобрать слот".into()),
        }
        let gap = policy.pause_min * 60;
        if let Some(conflict) = reserved
            .iter()
            .copied()
            .filter(|t| at.abs_diff(*t) < gap as u64)
            .max()
        {
            at = conflict.saturating_add(gap);
            continue;
        }
        return Ok(at);
    }
    Err("Не удалось подобрать свободный слот".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn midnight_windows_limits_and_reserved_future_slots() {
        let p = Policy {
            pause_min: 30,
            daily_limit: 2,
            window_start: 22 * 60,
            window_end: 6 * 60,
        };
        assert_eq!(next_slot(&p, 12 * 3600, 0, &[]).unwrap(), 22 * 3600);
        assert_eq!(
            next_slot(&p, 23 * 3600, 0, &[23 * 3600, 23 * 3600 + 1800]).unwrap(),
            86400
        );
        assert_eq!(
            next_slot(&p, 5 * 3600 + 50 * 60, 0, &[5 * 3600 + 40 * 60]).unwrap(),
            22 * 3600
        );
        assert_eq!(
            next_slot(&Policy::default(), 1000, 0, &[2000]).unwrap(),
            4700
        );
        assert_eq!(next_slot(&p, 19 * 3600, 3 * 3600, &[]).unwrap(), 19 * 3600);
    }
    #[test]
    fn all_day_boundary_and_bad_policy() {
        let p = Policy {
            window_start: 0,
            window_end: 0,
            ..Default::default()
        };
        assert_eq!(next_slot(&p, 42, 0, &[]).unwrap(), 42);
        assert!(next_slot(
            &Policy {
                daily_limit: 0,
                ..p
            },
            1,
            0,
            &[]
        )
        .is_err());
    }
}
