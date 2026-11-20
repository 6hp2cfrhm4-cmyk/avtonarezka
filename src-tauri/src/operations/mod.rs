//! Эксплуатация фабрики: временные системные ресурсы и сохранённое намерение пользователя.
pub mod diagnostics;
pub mod disk;
pub mod gpu;
#[cfg(feature = "desktop")]
pub mod lifecycle;
#[cfg(not(feature = "desktop"))]
#[path = "lifecycle_stub.rs"]
pub mod lifecycle;
pub mod power;
