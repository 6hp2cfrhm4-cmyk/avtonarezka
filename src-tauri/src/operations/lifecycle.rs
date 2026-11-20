//! Трей и автозапуск только по явному действию. Закрытие окна по-прежнему завершает приложение.
use crate::{drafts, state::AppState};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, State,
};

#[derive(Default)]
pub struct Lifecycle {
    gate: Mutex<()>,
    error: Mutex<Option<String>>,
    active: AtomicBool,
}
const TRAY: &str = "factory-tray";
const MARKER: &str = "avtonarezka-factory-autostart-v1";

fn show(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("Главное окно недоступно")?;
    window.show().map_err(|e| e.to_string())?;
    window.unminimize().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())
}
fn create_tray(app: &AppHandle) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id(TRAY) {
        return tray.set_visible(true).map_err(|e| e.to_string());
    }
    // libappindicator-sys паникует при отсутствии .so. Проверяем до вызова GTK,
    // чтобы необязательная опция возвращала понятную ошибку без падения приложения.
    #[cfg(target_os = "linux")]
    if ![
        "libayatana-appindicator3.so.1",
        "libappindicator3.so.1",
        "libayatana-appindicator3.so",
        "libappindicator3.so",
    ]
    .iter()
    .any(|name| unsafe { libloading::Library::new(name) }.is_ok())
    {
        return Err("Для трея нужна установленная библиотека AppIndicator (libayatana-appindicator3 или libappindicator-gtk3)".into());
    }
    let open = MenuItem::with_id(
        app,
        "factory-open",
        "Открыть Автонарезку",
        true,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(
        app,
        "factory-quit",
        "Завершить приложение",
        true,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let menu = Menu::with_items(app, &[&open, &quit]).map_err(|e| e.to_string())?;
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("Нет значка приложения для трея")?;
    TrayIconBuilder::with_id(TRAY)
        .icon(icon)
        .tooltip("Автонарезка · фабрика работает, пока приложение запущено")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "factory-open" => {
                if let Err(e) = show(app) {
                    tracing::warn!("Не удалось открыть окно из трея: {e}");
                }
            }
            "factory-quit" => app.exit(0),
            _ => {}
        })
        .build(app)
        .map_err(|e| format!("Трей недоступен: {e}"))?;
    Ok(())
}
pub fn restore(app: &AppHandle) {
    let state = app.state::<AppState>();
    if drafts::kv_get(&state.db.0.lock().unwrap(), "tray_enabled").as_deref() == Some("1") {
        match create_tray(app) {
            Ok(()) => state.lifecycle.active.store(true, Ordering::Release),
            Err(e) => {
                tracing::warn!("{e}");
                *state.lifecycle.error.lock().unwrap() = Some(e);
            }
        }
    }
}
fn autostart_path() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| crate::platform::home_dir().map(|p| p.join(".config")))?;
        Some(config.join("autostart/avtonarezka-factory.desktop"))
    }
    #[cfg(windows)]
    {
        Some(
            PathBuf::from(std::env::var_os("APPDATA")?)
                .join("Microsoft/Windows/Start Menu/Programs/Startup/avtonarezka-factory.cmd"),
        )
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}
fn autostart_text(exe: &Path, windows: bool) -> Result<String, String> {
    let path = exe
        .to_str()
        .ok_or("Путь приложения не поддерживает UTF-8")?;
    if !exe.is_absolute() || path.contains(['\n', '\r', '\0', '"']) {
        return Err("Неподдерживаемый путь автозапуска".into());
    }
    if windows {
        Ok(format!("@rem {MARKER}\r\n@echo off\r\nsetlocal DisableDelayedExpansion\r\nchcp 65001>nul\r\nstart \"\" \"{}\"\r\n",path.replace('%',"%%")))
    } else {
        // Два уровня escaping: Desktop Entry и Exec, без shell. env оставляет путь
        // аргументом: GIO проверяет первый executable до развёртки %% и иначе
        // отклоняет корректную запись, когда сам путь содержит процент.
        let quoted = path
            .replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%");
        let escaped = quoted.replace('\\', "\\\\");
        Ok(format!("# {MARKER}\n[Desktop Entry]\nType=Application\nName=Автонарезка\nExec=/usr/bin/env -- \"{escaped}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"))
    }
}
fn owned(path: &Path) -> Result<bool, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("Путь автозапуска занят ссылкой или каталогом".into());
    }
    if meta.len() > 16384 {
        return Err("Путь автозапуска занят другим файлом".into());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if !text.starts_with(&format!("# {MARKER}\n"))
        && !text.starts_with(&format!("@rem {MARKER}\r\n"))
    {
        return Err("Файл автозапуска создан другой программой; измени его вручную".into());
    }
    Ok(true)
}
fn set_autostart(path: &Path, exe: &Path, enabled: bool) -> Result<(), String> {
    let existed = owned(path)?;
    if !enabled {
        if existed {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let content = autostart_text(exe, cfg!(windows))?;
    let parent = path.parent().ok_or("Нет каталога автозапуска")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // Файл создаётся один раз; повторный opt-in не меняет уже настроенный запуск.
    if existed {
        return Ok(());
    }
    use std::io::Write;
    let part = parent.join(format!(
        ".avtonarezka-autostart-{}.tmp",
        rand::random::<u64>()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&part)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // link создаёт только отсутствующий target; конкурентный чужой файл не заменяется.
        std::fs::hard_link(&part, path)
    })();
    let _ = std::fs::remove_file(part);
    result.map_err(|e| e.to_string())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    tray_enabled: bool,
    tray_active: bool,
    tray_error: Option<String>,
    autostart_available: bool,
    autostart_enabled: bool,
    autostart_error: Option<String>,
    window_visible: bool,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn lifecycle_status(app: AppHandle, state: State<'_, AppState>) -> Status {
    status(&app, &state)
}
fn status(app: &AppHandle, state: &AppState) -> Status {
    let path = autostart_path();
    let autostart = path.as_deref().map(owned).unwrap_or(Ok(false));
    Status {
        tray_enabled: drafts::kv_get(&state.db.0.lock().unwrap(), "tray_enabled").as_deref()
            == Some("1"),
        tray_active: state.lifecycle.active.load(Ordering::Acquire)
            && app.tray_by_id(TRAY).is_some(),
        tray_error: state.lifecycle.error.lock().unwrap().clone(),
        autostart_available: path.is_some(),
        autostart_enabled: autostart.as_ref().copied().unwrap_or(false),
        autostart_error: autostart.err(),
        window_visible: app
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false),
    }
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn lifecycle_tray_set(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<Status, String> {
    let _gate = state.lifecycle.gate.lock().unwrap();
    if enabled {
        create_tray(&app)?;
    } else {
        show(&app)?;
        // AppIndicator сохраняет экспорт D-Bus после удаления значка. Повторное создание
        // с тем же id теряет меню; сохраняем объект и переключаем его Active/Passive.
        if let Some(tray) = app.tray_by_id(TRAY) {
            tray.set_visible(false).map_err(|e| e.to_string())?;
        }
    }
    state.lifecycle.active.store(enabled, Ordering::Release);
    drafts::kv_set(
        &state.db.0.lock().unwrap(),
        "tray_enabled",
        if enabled { "1" } else { "0" },
    )
    .map_err(|e| e.to_string())?;
    *state.lifecycle.error.lock().unwrap() = None;
    Ok(status(&app, &state))
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn lifecycle_autostart_set(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<Status, String> {
    let _gate = state.lifecycle.gate.lock().unwrap();
    let path = autostart_path().ok_or("Автозапуск недоступен в этой ОС")?;
    set_autostart(
        &path,
        &std::env::current_exe().map_err(|e| e.to_string())?,
        enabled,
    )?;
    Ok(status(&app, &state))
}
#[cfg(target_os = "linux")]
fn tray_host_available() -> bool {
    use glib::variant::ToVariant;
    use webkit2gtk::{gio, glib};
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return false;
    };
    let args = (
        "org.kde.StatusNotifierWatcher",
        "IsStatusNotifierHostRegistered",
    )
        .to_variant();
    bus.call_sync(
        Some("org.kde.StatusNotifierWatcher"),
        "/StatusNotifierWatcher",
        "org.freedesktop.DBus.Properties",
        "Get",
        Some(&args),
        Some(glib::VariantTy::new("(v)").unwrap()),
        gio::DBusCallFlags::NO_AUTO_START,
        750,
        None::<&gio::Cancellable>,
    )
    .ok()
    .and_then(|v| v.child_value(0).as_variant())
    .and_then(|v| v.get::<bool>())
    .unwrap_or(false)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn lifecycle_hide(app: AppHandle) -> Result<(), String> {
    if !app
        .state::<AppState>()
        .lifecycle
        .active
        .load(Ordering::Acquire)
        || app.tray_by_id(TRAY).is_none()
    {
        return Err("Сначала включи значок в трее".into());
    }
    #[cfg(target_os = "linux")]
    if !tray_host_available() {
        return Err("Панель рабочего стола не подтвердила поддержку трея. Окно остаётся открытым; включи поддержку значков в панели и повтори".into());
    }
    app.get_webview_window("main")
        .ok_or("Нет окна")?
        .hide()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn desktop_entry_and_windows_startup_do_not_invoke_a_shell_with_unescaped_path() {
        let text =
            autostart_text(Path::new("/tmp/Мой путь/$cash`tick`%literal\\app"), false).unwrap();
        assert!(text.contains(
            "Exec=/usr/bin/env -- \"/tmp/Мой путь/\\\\$cash\\\\`tick\\\\`%%literal\\\\\\\\app\""
        ));
        let windows = autostart_text(Path::new("/test/%x% & name.exe"), true).unwrap();
        assert!(windows.contains("DisableDelayedExpansion"));
        assert!(windows.contains("\"/test/%%x%% & name.exe\""));
        for path in ["relative", "/tmp/a\nb", "/tmp/\"oops"] {
            assert!(autostart_text(Path::new(path), false).is_err());
        }
    }
    #[test]
    fn only_explicit_opt_in_creates_owned_file_and_foreign_file_is_preserved() {
        let dir = std::env::temp_dir().join(format!(
            "avto-autostart-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = dir.join("autostart/app.desktop");
        assert!(!owned(&path).unwrap());
        let exe = dir.join("Автонарезка/app");
        set_autostart(&path, &exe, false).unwrap();
        assert!(!dir.exists());
        set_autostart(&path, &exe, true).unwrap();
        assert!(owned(&path).unwrap());
        let original = std::fs::read(&path).unwrap();
        set_autostart(&path, &dir.join("different/app"), true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        set_autostart(&path, &exe, false).unwrap();
        assert!(!path.exists());
        std::fs::write(&path, b"foreign app").unwrap();
        assert!(set_autostart(&path, &exe, true).is_err());
        assert!(set_autostart(&path, &exe, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign app");
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&exe, &path).unwrap();
            assert!(owned(&path).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
