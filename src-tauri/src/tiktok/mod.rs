//! Заливка готовых роликов в TikTok.
//!
//! Два пути: официальный API (`api`, `publish`, `oauth`) и запасной через браузер (`browser`).
//! Ключи и токены хранятся в связке ключей системы (`secrets`), а не в базе.

pub mod api;
pub mod browser;
pub mod oauth;
pub mod publish;
pub mod secrets;
