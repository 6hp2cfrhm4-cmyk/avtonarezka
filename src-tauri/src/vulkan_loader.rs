//! Загрузчик Vulkan (vulkan-1.dll) на Windows.
//!
//! Whisper и llama.cpp собраны с Vulkan, и vulkan-1.dll им нужна даже на машине без видеокарты. Обычно её кладёт драйвер
//! в System32, но на чистой Windows или в виртуалке её нет, и обычная (статическая) зависимость не даёт программе
//! запуститься. Поэтому в сборке для Windows vulkan-1.dll подключена с отложенной загрузкой (`/DELAYLOAD`, см. build.rs),
//! а `prepare` до первого обращения к видеокарте решает, откуда её брать: сначала системная (драйвер видеокарты
//! поставил её сам, и она точно подходит к драйверу), иначе запасная из установки (`vk\vulkan-1.dll`). Запасной
//! загрузчик без драйвера видит ноль видеоускорителей, и анализ идёт на процессоре.
//!
//! Файл подключают и приложение (`mod`), и llm-worker (`#[path]`), поэтому он зависит только от std и windows-sys.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Source {
    /// vulkan-1.dll из System32 (её поставил драйвер видеокарты); на других ОС Vulkan ищет система сама
    System,
    /// Запасной загрузчик из установки программы
    Bundled,
    /// Нет ни системного, ни запасного: первое обращение к Vulkan завершит программу
    Missing,
}

#[cfg(windows)]
pub fn prepare() -> Source {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryExW, LoadLibraryW, LOAD_LIBRARY_SEARCH_SYSTEM32};

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();

    // Модуль остаётся загруженным: когда отложенная загрузка потом попросит «vulkan-1.dll», Windows найдёт его по имени
    // среди уже загруженных и не будет искать заново.
    let system = wide("vulkan-1.dll".as_ref());
    if !unsafe { LoadLibraryExW(system.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) }.is_null() {
        return Source::System;
    }
    let bundled = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("vk").join("vulkan-1.dll")));
    if let Some(path) = bundled {
        if path.is_file() && !unsafe { LoadLibraryW(wide(path.as_os_str()).as_ptr()) }.is_null() {
            return Source::Bundled;
        }
    }
    Source::Missing
}

#[cfg(not(windows))]
pub fn prepare() -> Source {
    Source::System
}
