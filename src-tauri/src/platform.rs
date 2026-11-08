//! Отличия ОС в одном месте: где лежат утилиты, как открыть файл или ссылку, где домашняя папка.
//! Остальной код про ОС не знает. На Linux всё работает как раньше; на Windows добавляются папка `tools`
//! рядом с программой (туда установщик кладёт ffmpeg, yt-dlp и deno), запуск без консольного окна
//! и системные способы открыть файл.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Расширение исполняемых файлов.
const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// Подсказка к ошибке «не удалось открыть» (на Windows системный обработчик есть всегда).
pub const OPENER_HINT: &str = if cfg!(windows) { "" } else { " (установлен ли xdg-open?)" };

/// Папка `tools` рядом с исполняемым файлом, если она есть.
pub fn tools_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::current_exe().ok()?.parent()?.join("tools");
        dir.is_dir().then_some(dir)
    })
    .as_deref()
}

/// Утилита по имени: сначала из `tools` (вшитая), затем из PATH.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{EXE}");
    if let Some(p) = tools_dir().map(|d| d.join(&file)).filter(|p| p.is_file()) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(&file)).find(|p| p.is_file())
}

/// PATH с `dir` в начале: дочерний процесс (yt-dlp) находит там же ffmpeg и deno.
fn path_with(dir: &Path, old: Option<OsString>) -> Option<OsString> {
    let old = old.unwrap_or_default();
    std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&old))).ok()
}

/// Общая настройка дочерних процессов: вшитые утилиты в PATH, на Windows без окна консоли.
fn tune(cmd: &mut std::process::Command) {
    if let Some(path) = tools_dir().and_then(|d| path_with(d, std::env::var_os("PATH"))) {
        cmd.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

/// Запуск программы по полному пути.
pub fn command_at(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    tune(cmd.as_std_mut());
    cmd
}

/// Запуск утилиты по имени (`ffmpeg`, `ffprobe`, …): вшитая из `tools`, иначе из PATH.
pub fn command(name: &str) -> tokio::process::Command {
    command_at(find_tool(name).unwrap_or_else(|| PathBuf::from(name)))
}

/// Домашняя папка пользователя.
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

/// Папка моделей по умолчанию (там же, где её создаёт приложение).
pub fn default_models_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("app.avtonarezka.desktop").join("models"))
    }
    #[cfg(not(windows))]
    {
        home_dir().map(|h| h.join(".local/share/app.avtonarezka.desktop/models"))
    }
}

/// Запускает и не ждёт завершения; процесс дожидается в фоне, чтобы не копить зомби.
fn spawn_detached(mut cmd: std::process::Command) -> std::io::Result<()> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Открывает файл или папку в программе по умолчанию.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("explorer.exe");
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path);
    spawn_detached(cmd)
}

/// Открывает адрес в браузере по умолчанию. Вызывающий сам проверяет схему (только http и https).
pub fn open_url(url: &str) -> std::io::Result<()> {
    // `rundll32 url.dll,FileProtocolHandler`, а не `cmd /c start`: в адресе OAuth есть `&`, который cmd понял бы как команду
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", url]).creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(windows))]
    let cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    spawn_detached(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_dir_goes_first_in_path() {
        let old = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let joined = path_with(Path::new("/opt/app/tools"), Some(old)).unwrap();
        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parts, [PathBuf::from("/opt/app/tools"), PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    }

    #[test]
    fn empty_path_still_gets_the_tools_dir() {
        let joined = path_with(Path::new("/opt/app/tools"), None).unwrap();
        assert_eq!(std::env::split_paths(&joined).next(), Some(PathBuf::from("/opt/app/tools")));
    }

    #[test]
    fn missing_tool_is_not_found() {
        assert_eq!(find_tool("avto-no-such-tool-xyz"), None);
    }
}
