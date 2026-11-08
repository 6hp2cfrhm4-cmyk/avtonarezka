//! Сборка вертикального ролика: верх — момент из исходника, низ — геймплей,
//! на линии раздела баннер с хромакеем, пока баннер играет, верх замирает.

pub mod ass;
pub mod ffmpeg;
pub mod filter;
pub mod types;

pub use ffmpeg::render;
pub use types::*;
