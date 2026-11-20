//! Изменяемый лимит рендера: снижение не обрывает активные задачи и не создаёт второй семафор.
use crate::host::{AppHandle, Emitter, State};
use crate::{drafts, state::AppState};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub struct Lane {
    counts: Mutex<(usize, usize)>,
    changed: Notify,
}
pub struct Permit(Arc<Lane>);
impl Lane {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            counts: Mutex::new((0, limit.max(1))),
            changed: Notify::new(),
        })
    }
    pub fn set_limit(&self, limit: usize) {
        self.counts.lock().unwrap().1 = limit.max(1);
        self.changed.notify_waiters();
    }
    pub async fn acquire_owned(self: Arc<Self>) -> Permit {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut c = self.counts.lock().unwrap();
                if c.0 < c.1 {
                    c.0 += 1;
                    return Permit(self.clone());
                }
            }
            notified.await;
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.counts.lock().unwrap().0 -= 1;
        self.0.changed.notify_waiters();
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub draft_concurrency: usize,
    pub final_concurrency: usize,
    pub resume_on_start: bool,
    pub inhibit_sleep: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            draft_concurrency: 2,
            final_concurrency: 1,
            resume_on_start: true,
            inhibit_sleep: true,
        }
    }
}
pub(crate) fn load(db: &rusqlite::Connection) -> Settings {
    let mut s: Settings = drafts::kv_get(db, "factory_settings")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    s.draft_concurrency = s.draft_concurrency.clamp(1, 4);
    s.final_concurrency = s.final_concurrency.clamp(1, 2);
    s
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn factory_settings(state: State<'_, AppState>) -> Settings {
    load(&state.db.0.lock().unwrap())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn factory_settings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    if !(1..=4).contains(&settings.draft_concurrency)
        || !(1..=2).contains(&settings.final_concurrency)
    {
        return Err("Черновики: 1–4; финалы: 1–2".into());
    }
    drafts::kv_set(
        &state.db.0.lock().unwrap(),
        "factory_settings",
        &serde_json::to_string(&settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    state.draft_slots.set_limit(settings.draft_concurrency);
    state.final_slots.set_limit(settings.final_concurrency);
    let _ = app.emit("factory-changed", ());
    Ok(settings)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lowering_limit_waits_for_existing_work_and_raising_wakes_waiters() {
        let lane = Lane::new(2);
        let a = lane.clone().acquire_owned().await;
        let b = lane.clone().acquire_owned().await;
        lane.set_limit(1);
        let mut waiting = tokio::spawn(lane.clone().acquire_owned());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(a);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        lane.set_limit(2);
        let c = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        drop((b, c));
        assert_eq!(lane.counts.lock().unwrap().0, 0);
    }
}
