//! Выбор видеокарты для Vulkan. В ноутбуках их две: встроенная и дискретная, и ggml по умолчанию
//! берёт устройство 0 — часто именно встроенную (у нас она в 3 раза медленнее).

/// Устройство из списка Vulkan: номер, название, видеопамять в байтах.
pub type Device = (i32, String, usize);

fn is_discrete(name: &str) -> bool {
    let n = name.to_lowercase();
    ["nvidia", "geforce", "rtx", "quadro", "radeon rx", "radeon pro", "arc "].iter().any(|k| n.contains(k))
}

/// Дискретная карта с наибольшей памятью, иначе первое устройство. `None` — Vulkan ничего не видит.
pub fn pick(devices: &[Device]) -> Option<i32> {
    devices
        .iter()
        .filter(|(_, name, _)| is_discrete(name))
        .max_by_key(|(_, _, vram)| *vram)
        .or_else(|| devices.first())
        .map(|(id, _, _)| *id)
}

/// Номер из `AVTO_GPU_DEVICE` (для отладки и настроек), иначе автоматический выбор.
#[cfg(feature = "gpu-probe")]
pub fn choose() -> Option<i32> {
    if let Some(d) = std::env::var("AVTO_GPU_DEVICE").ok().and_then(|v| v.parse().ok()) {
        return Some(d);
    }
    let devices: Vec<Device> = whisper_rs::vulkan::list_devices().into_iter().map(|d| (d.id, d.name, d.vram.total)).collect();
    pick(&devices)
}

/// Без Vulkan Whisper работает на процессоре.
#[cfg(not(feature = "gpu-probe"))]
pub fn choose() -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(id: i32, name: &str, gb: usize) -> Device {
        (id, name.into(), gb << 30)
    }

    #[test]
    fn laptop_picks_the_discrete_card_even_when_it_is_listed_second() {
        let v = [d(0, "AMD Radeon Graphics (RADV RENOIR)", 8), d(1, "NVIDIA GeForce RTX 3050 Laptop GPU", 4)];
        assert_eq!(pick(&v), Some(1));
    }

    #[test]
    fn biggest_discrete_card_wins() {
        let v = [d(0, "NVIDIA GeForce GTX 1050", 2), d(1, "AMD Radeon RX 6800", 16)];
        assert_eq!(pick(&v), Some(1));
    }

    #[test]
    fn only_integrated_graphics_is_still_better_than_cpu() {
        assert_eq!(pick(&[d(0, "Intel(R) UHD Graphics", 8)]), Some(0));
    }

    #[test]
    fn no_devices_means_cpu() {
        assert_eq!(pick(&[]), None);
    }
}
