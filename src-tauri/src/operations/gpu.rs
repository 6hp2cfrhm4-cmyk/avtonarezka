//! Общий бюджет GPU: тяжёлая модель исключает NVENC/NVDEC, рендер в это время идёт на CPU.
use std::{
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

fn gate() -> Arc<RwLock<()>> {
    static GATE: OnceLock<Arc<RwLock<()>>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(RwLock::new(()))).clone()
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total_mib: u64,
    pub free_mib: u64,
}

fn parse_memory(text: &str) -> Option<Memory> {
    // Vulkan/CUDA могут по-разному нумеровать устройства. Без UUID-сопоставления
    // берём наименьший свободный объём, чтобы не обещать память другой карты.
    text.lines()
        .map(|line| {
            let (total, free) = line.split_once(',')?;
            let m = Memory {
                total_mib: total.trim().parse().ok()?,
                free_mib: free.trim().parse().ok()?,
            };
            (m.total_mib > 0 && m.free_mib <= m.total_mib).then_some(m)
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min_by_key(|m| m.free_mib)
}

/// Не устанавливает утилит; отсутствующий/неработающий probe оставляет штатный fallback.
pub async fn memory() -> Option<Memory> {
    let tool = crate::platform::find_tool("nvidia-smi")?;
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        crate::platform::command_at(tool)
            .args([
                "--query-gpu=memory.total,memory.free",
                "--format=csv,noheader,nounits",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_memory(std::str::from_utf8(&output.stdout).ok()?)
}

pub fn fits(memory: Option<Memory>, required_mib: u64) -> bool {
    memory.is_none_or(|m| m.free_mib >= required_mib)
}

/// Модель + контекст/рабочие буферы; это нижняя граница, ошибки драйвера всё равно обрабатываются.
pub fn model_budget(model: &Path) -> u64 {
    std::fs::metadata(model)
        .map_or(0, |m| m.len().div_ceil(1024 * 1024))
        .saturating_add(768)
}

pub async fn model() -> OwnedRwLockWriteGuard<()> {
    gate().write_owned().await
}
pub fn speech() -> Option<OwnedRwLockWriteGuard<()>> {
    // Whisper вызывается на отдельном OS-потоке. Если рендер уже занят, речь может
    // продолжить на CPU без ожидания и без взаимной блокировки с отменой анализа.
    gate().try_write_owned().ok()
}
pub async fn render() -> Option<OwnedRwLockReadGuard<()>> {
    let permit = gate().try_read_owned().ok()?;
    if fits(memory().await, 768) {
        Some(permit)
    } else {
        tracing::warn!("GPU: свободной видеопамяти для NVENC недостаточно, рендер на CPU");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_probe_rejects_invalid_and_uses_conservative_device() {
        assert_eq!(
            parse_memory("4096, 3811\n8192, 1500\n").unwrap().free_mib,
            1500
        );
        for text in ["", "N/A, N/A", "0, 0", "4096, 5000", "4096, 2000\ninvalid"] {
            assert!(parse_memory(text).is_none(), "{text}");
        }
        assert!(fits(None, 768));
        assert!(fits(
            Some(Memory {
                total_mib: 4096,
                free_mib: 768
            }),
            768
        ));
        assert!(!fits(
            Some(Memory {
                total_mib: 4096,
                free_mib: 767
            }),
            768
        ));
    }
    #[tokio::test]
    async fn waiting_model_excludes_new_gpu_renders_until_shutdown() {
        let gate = Arc::new(RwLock::new(()));
        let render = gate.clone().read_owned().await;
        let model = gate.clone().write_owned();
        tokio::pin!(model);
        assert!(futures_util::poll!(&mut model).is_pending());
        assert!(gate.clone().try_read_owned().is_err());
        drop(render);
        let model = model.await;
        assert!(gate.clone().try_read_owned().is_err());
        assert!(gate.clone().try_write_owned().is_err());
        drop(model);
        assert!(gate.try_read_owned().is_ok());
    }
}
