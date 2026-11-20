//! Трея и автозапуска окна в демоне нет: остаются только пустые заглушки под общий `AppState`.
use crate::host::AppHandle;

#[derive(Default)]
pub struct Lifecycle;

pub fn restore(_app: &AppHandle) {}
