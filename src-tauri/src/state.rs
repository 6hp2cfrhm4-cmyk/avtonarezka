use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::sync::{watch, OnceCell, Semaphore};

use crate::{db::Db, media_server::MediaServer, tiktok::secrets::Secrets};

pub struct AppState {
    pub source_use: tokio::sync::RwLock<()>,
    pub power: crate::operations::power::PowerState,
    pub lifecycle: crate::operations::lifecycle::Lifecycle,
    pub analytics_gate: tokio::sync::Mutex<()>,
    pub db: Db,
    pub media: MediaServer,
    pub data_dir: PathBuf,
    /// Запущенные анализы: `project_id` → сигнал отмены.
    pub analyses: Mutex<HashMap<i64, watch::Sender<bool>>>,
    pub camera_scan: Mutex<Option<(String, watch::Sender<bool>)>>,
    /// Идущие и ожидающие рендеры: `draft_id` → сигнал отмены.
    pub renders: Mutex<HashMap<i64, watch::Sender<bool>>>,
    /// Одновременно рендерим два черновика, но только один финал.
    pub draft_slots: Arc<crate::factory::resources::Lane>,
    pub final_slots: Arc<crate::factory::resources::Lane>,
    /// Есть ли рабочий NVENC; проверяется один раз.
    pub nvenc: OnceCell<bool>,
    /// Whisper и LLM (включая подписи) никогда не занимают видеопамять одновременно.
    pub ml_gate: Arc<Semaphore>,
    /// Есть ли CUDA в ffmpeg (декодирование NVDEC); проверяется один раз.
    pub cuda: OnceCell<bool>,
    /// Ключи и токены TikTok: связка ключей системы или, если её нет, память.
    pub secrets: Secrets,
    /// Идущий вход в TikTok: сигнал отмены.
    pub login: Mutex<Option<watch::Sender<bool>>>,
    pub platform_login: Mutex<Option<watch::Sender<bool>>>,
    /// Ролики, которые прямо сейчас уходят в TikTok.
    pub posting: Mutex<HashSet<i64>>,
    /// Идёт загрузка моделей при запуске или по кнопке.
    pub models_downloading: std::sync::atomic::AtomicBool,
    /// Браузер выкладывает по одному ролику за раз: очередь и ручной диалог ждут друг друга.
    pub browser_gate: tokio::sync::Mutex<()>,
    /// Идущая очередь автовыкладки: сигнал остановки. `Some` — очередь работает.
    pub publisher: Mutex<Option<watch::Sender<bool>>>,
    /// Идущее написание описаний: сигнал отмены.
    pub drafting: Mutex<Option<watch::Sender<bool>>>,
    /// Почему очередь остановилась в последний раз (капча, сбой): показывается в панели, пока человек не запустит её снова.
    pub queue_note: Mutex<Option<String>>,
    /// Идущие и ожидающие загрузки с YouTube: id задачи → (id видео, сигнал отмены).
    pub downloads: Mutex<HashMap<String, (String, watch::Sender<bool>)>>,
    /// Качаем одновременно два ролика, остальные ждут своей очереди.
    pub youtube_slots: Arc<Semaphore>,
    /// Пачка ссылок и файлов: раннер и то, что он сейчас скачивает или анализирует.
    pub batch: crate::batch::BatchState,
    pub monitor: crate::factory::monitor::MonitorState,
    pub output_prepares: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub source_prepares: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppState {
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    pub fn fonts_dir(&self) -> PathBuf {
        self.data_dir.join("fonts")
    }
}
