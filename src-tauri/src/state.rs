use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::{Arc, Mutex}};

use tokio::sync::{watch, OnceCell, Semaphore};

use crate::{db::Db, media_server::MediaServer, tiktok::secrets::Secrets};

pub struct AppState {
    pub db: Db,
    pub media: MediaServer,
    pub data_dir: PathBuf,
    /// Запущенные анализы: `project_id` → сигнал отмены.
    pub analyses: Mutex<HashMap<i64, watch::Sender<bool>>>,
    /// Идущие и ожидающие рендеры: `draft_id` → сигнал отмены.
    pub renders: Mutex<HashMap<i64, watch::Sender<bool>>>,
    /// Одновременно рендерим два черновика, но только один финал.
    pub draft_slots: Arc<Semaphore>,
    pub final_slots: Arc<Semaphore>,
    /// Есть ли рабочий NVENC; проверяется один раз.
    pub nvenc: OnceCell<bool>,
    /// Ключи и токены TikTok: связка ключей системы или, если её нет, память.
    pub secrets: Secrets,
    /// Идущий вход в TikTok: сигнал отмены.
    pub login: Mutex<Option<watch::Sender<bool>>>,
    /// Ролики, которые прямо сейчас уходят в TikTok.
    pub posting: Mutex<HashSet<i64>>,
    /// Идущие и ожидающие загрузки с YouTube: id задачи → (id видео, сигнал отмены).
    pub downloads: Mutex<HashMap<String, (String, watch::Sender<bool>)>>,
    /// Качаем одновременно два ролика, остальные ждут своей очереди.
    pub youtube_slots: Arc<Semaphore>,
}

impl AppState {
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    pub fn fonts_dir(&self) -> PathBuf {
        self.data_dir.join("fonts")
    }
}
