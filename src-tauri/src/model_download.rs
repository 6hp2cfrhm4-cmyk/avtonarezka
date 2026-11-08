//! Модели для анализа качаются сами: при запуске программы, если каких-то файлов на диске нет.
//! Размер и SHA256 каждого файла заданы здесь. Загрузка идёт в `.part`-файл и докачивается, если прервалась.
//! Файл получает своё имя, только когда его контрольная сумма совпала.

use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use anyhow::{bail, Context};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::state::AppState;

struct Model {
    /// Путь внутри папки моделей.
    rel: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

static MODELS: [Model; 4] = [
    Model {
        rel: "whisper-large-v3-turbo-q5_0/ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    Model {
        rel: "yamnet-onnx/yamnet.onnx",
        url: "https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet.onnx",
        size: 16_093_603,
        sha256: "1510041dce24a2e9e84ec546807ac408ae496da6d1ed41bc3ccba649623f8e19",
    },
    Model {
        rel: "yamnet-onnx/yamnet_class_map.csv",
        url: "https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet_class_map.csv",
        size: 14_096,
        sha256: "cdf24d193e196d9e95912a2667051ae203e92a2ba09449218ccb40ef787c6df2",
    },
    Model {
        rel: "qwen3-4b-q4_k_m/Qwen3-4B-Q4_K_M.gguf",
        url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf",
        size: 2_497_280_256,
        sha256: "7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5",
    },
];

/// Модели, которых нет на диске или которые лежат не целиком (по размеру).
fn missing(dir: &Path) -> Vec<&'static Model> {
    MODELS.iter().filter(|m| std::fs::metadata(dir.join(m.rel)).map_or(true, |md| md.len() != m.size)).collect()
}

/// Все модели на месте.
pub fn ready(dir: &Path) -> bool {
    missing(dir).is_empty()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    name: &'a str,
    received: u64,
    total: u64,
}

#[derive(Serialize, Clone)]
struct Failed {
    message: String,
}

/// Запускает загрузку недостающих моделей в фоне. Возвращает `false`, если всё уже есть или загрузка уже идёт.
/// Ход загрузки уходит событиями `models-progress`, итог — `models-done` или `models-error`.
pub fn start(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    if ready(&state.models_dir()) || state.models_downloading.swap(true, Ordering::SeqCst) {
        return false;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = run(&app).await;
        app.state::<AppState>().models_downloading.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => {
                let _ = app.emit("models-done", ());
            }
            Err(e) => {
                let _ = app.emit("models-error", Failed { message: format!("{e:#}") });
            }
        }
    });
    true
}

async fn run(app: &AppHandle) -> anyhow::Result<()> {
    let dir = app.state::<AppState>().models_dir();
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build()?;
    for m in missing(&dir) {
        download(app, &client, &dir, m).await.with_context(|| format!("не скачалась модель {}", m.rel))?;
    }
    Ok(())
}

async fn download(app: &AppHandle, client: &reqwest::Client, dir: &Path, m: &Model) -> anyhow::Result<()> {
    let target = dir.join(m.rel);
    let part = PathBuf::from(format!("{}.part", target.display()));
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut have = tokio::fs::metadata(&part).await.map_or(0, |md| md.len());
    if have > m.size {
        have = 0;
    }
    if have < m.size {
        let mut req = client.get(m.url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut resp = req.send().await?.error_for_status()?;
        // сервер может проигнорировать докачку и прислать файл целиком: тогда пишем с начала
        let resumed = have > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !resumed {
            have = 0;
        }
        let mut file = if resumed {
            tokio::fs::OpenOptions::new().append(true).open(&part).await?
        } else {
            tokio::fs::File::create(&part).await?
        };

        let mut received = have;
        let mut reported = have;
        loop {
            let next = tokio::time::timeout(Duration::from_secs(60), resp.chunk())
                .await
                .context("соединение молчит больше минуты")??;
            let Some(bytes) = next else { break };
            file.write_all(&bytes).await?;
            received += bytes.len() as u64;
            // событие не чаще чем раз в 4 МБ, иначе интерфейс захлебнётся
            if received - reported >= 4 << 20 || received == m.size {
                reported = received;
                let _ = app.emit("models-progress", Progress { name: m.rel, received, total: m.size });
            }
        }
        file.flush().await?;
    }

    // хэш считается на 2,5 ГБ несколько секунд: в отдельном потоке, чтобы не держать runtime
    let check = part.clone();
    let expected = m.sha256;
    let matches = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let mut file = std::fs::File::open(&check)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = std::io::Read::read(&mut file, &mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()) == expected)
    })
    .await??;
    if !matches {
        tokio::fs::remove_file(&part).await.ok();
        bail!("контрольная сумма не совпала, файл скачается заново при следующей попытке");
    }
    tokio::fs::rename(&part, &target).await?;
    Ok(())
}

/// Команда из интерфейса: докачать недостающее. `false` — ничего качать не надо или уже качается.
#[tauri::command]
pub fn models_download(app: AppHandle) -> bool {
    start(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_short_file_counts_as_not_downloaded() {
        let dir = std::env::temp_dir().join(format!("avto-models-{}", std::process::id()));
        assert_eq!(missing(&dir).len(), MODELS.len(), "пустая папка — всё качать");
        let yamnet = dir.join("yamnet-onnx");
        std::fs::create_dir_all(&yamnet).unwrap();
        std::fs::write(yamnet.join("yamnet_class_map.csv"), [0u8; 10]).unwrap();
        // короткий файл не в счёт: докачка по размеру, а не «файл есть»
        assert_eq!(missing(&dir).len(), MODELS.len());
        assert!(!ready(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }
}
