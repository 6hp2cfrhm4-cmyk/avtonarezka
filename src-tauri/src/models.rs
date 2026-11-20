//! Где лежат локальные модели. Скачиваются в `<данные приложения>/models/<имя>/<файл>`.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ModelPaths {
    pub whisper: PathBuf,
    pub yamnet: PathBuf,
    pub yamnet_classes: PathBuf,
    pub llm: PathBuf,
}

impl ModelPaths {
    pub fn in_dir(models: &Path) -> Self {
        Self::with_whisper_override(models, std::env::var_os("AVTO_WHISPER_MODEL"))
    }

    /// `AVTO_WHISPER_MODEL` подменяет модель распознавания речи: на сервере без видеокарты стоит модель поменьше.
    pub fn with_whisper_override(models: &Path, over: Option<std::ffi::OsString>) -> Self {
        Self {
            whisper: over.map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| models.join("whisper-large-v3-turbo-q5_0/ggml-large-v3-turbo-q5_0.bin")),
            yamnet: models.join("yamnet-onnx/yamnet.onnx"),
            yamnet_classes: models.join("yamnet-onnx/yamnet_class_map.csv"),
            llm: models.join("qwen3-4b-q4_k_m/Qwen3-4B-Q4_K_M.gguf"),
        }
    }

    /// Папка моделей по умолчанию (та же, что использует приложение).
    pub fn default_dir() -> Option<PathBuf> {
        crate::platform::default_models_dir()
    }

    /// Названия моделей, которых нет на диске.
    pub fn missing(&self) -> Vec<&'static str> {
        [
            ("Whisper", &self.whisper),
            ("YAMNet", &self.yamnet),
            ("YAMNet (классы)", &self.yamnet_classes),
            ("LLM", &self.llm),
        ]
        .into_iter()
        .filter(|(_, p)| !p.is_file())
        .map(|(n, _)| n)
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_path_can_be_overridden_for_the_server() {
        let dir = Path::new("/m");
        assert!(ModelPaths::with_whisper_override(dir, None).whisper.ends_with("ggml-large-v3-turbo-q5_0.bin"));
        assert_eq!(ModelPaths::with_whisper_override(dir, Some("/srv/small.bin".into())).whisper, Path::new("/srv/small.bin"));
        assert!(ModelPaths::with_whisper_override(dir, Some("".into())).whisper.starts_with("/m"), "пустое значение игнорируется");
    }
}
