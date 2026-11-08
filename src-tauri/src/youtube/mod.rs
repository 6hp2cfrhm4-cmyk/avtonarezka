//! Скачивание видео с YouTube через yt-dlp.
//!
//! Логика портирована из утилиты `ytdl` (папка «youtube video downoalder»): те же правила ссылок, та же настройка
//! yt-dlp (Deno/Node для задач YouTube, склейка ffmpeg без перекодирования, докачка `.part`), тот же подсчёт
//! прогресса по потокам. Здесь ничего нет про Tauri: так всё проверяется тестами без окна.

pub mod engine;
pub mod progress;
pub mod select;
pub mod url;
