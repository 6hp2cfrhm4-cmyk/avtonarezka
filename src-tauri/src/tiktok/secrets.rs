//! Хранилище секретов. Linux: связка ключей системы через `secret-tool` (libsecret / gnome-keyring);
//! значение передаётся через stdin, в командной строке его нет — иначе оно было бы видно в списке процессов.
//! Windows: Диспетчер учётных данных (`CredWriteW` и др.), запись привязана к учётной записи пользователя.
//! Если ничего этого нет, секреты живут только в памяти и пропадают при закрытии приложения.

use std::{collections::HashMap, process::Stdio, sync::Mutex, time::Duration};

use anyhow::{bail, Context};
use tokio::{io::AsyncWriteExt, process::Command};

const APP: &str = "avtonarezka";
/// Связка ключей может ждать, пока пользователь её разблокирует.
const TIMEOUT: Duration = Duration::from_secs(60);

pub enum Secrets {
    #[cfg_attr(windows, allow(dead_code))]
    Keyring,
    #[cfg(windows)]
    WinCred,
    Memory(Mutex<HashMap<String, String>>),
}

impl Secrets {
    /// Хранилище системы, если оно есть (на Windows всегда; на Linux — при наличии `secret-tool`), иначе память.
    pub fn detect() -> Self {
        #[cfg(windows)]
        return Secrets::WinCred;
        #[cfg(not(windows))]
        Self::detect_secret_tool()
    }

    #[cfg(not(windows))]
    fn detect_secret_tool() -> Self {
        let found = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("secret-tool").is_file()));
        if found { Secrets::Keyring } else { Secrets::memory() }
    }

    pub fn memory() -> Self {
        Secrets::Memory(Mutex::default())
    }

    pub fn backend(&self) -> &'static str {
        match self {
            Secrets::Keyring => "keyring",
            #[cfg(windows)]
            Secrets::WinCred => "keyring",
            Secrets::Memory(_) => "memory",
        }
    }

    pub async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        match self {
            Secrets::Memory(m) => Ok(m.lock().unwrap().get(key).cloned()),
            #[cfg(windows)]
            Secrets::WinCred => wincred::read(key),
            Secrets::Keyring => {
                let out = run(&["lookup", "application", APP, "key", key], None).await?;
                // код 1 и пустой вывод — секрета нет; остальное — сбой связки ключей
                if out.status.success() {
                    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()).filter(|s| !s.is_empty()))
                } else if out.status.code() == Some(1) && out.stderr.is_empty() {
                    Ok(None)
                } else {
                    bail!("связка ключей: {}", String::from_utf8_lossy(&out.stderr).trim())
                }
            }
        }
    }

    pub async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        match self {
            Secrets::Memory(m) => {
                m.lock().unwrap().insert(key.to_string(), value.to_string());
                Ok(())
            }
            #[cfg(windows)]
            Secrets::WinCred => wincred::write(key, value),
            Secrets::Keyring => {
                let label = format!("Автонарезка: {key}");
                let out = run(&["store", &format!("--label={label}"), "application", APP, "key", key], Some(value)).await?;
                if !out.status.success() {
                    bail!("не удалось сохранить в связку ключей: {}", String::from_utf8_lossy(&out.stderr).trim());
                }
                Ok(())
            }
        }
    }

    pub async fn delete(&self, key: &str) -> anyhow::Result<()> {
        match self {
            Secrets::Memory(m) => {
                m.lock().unwrap().remove(key);
                Ok(())
            }
            #[cfg(windows)]
            Secrets::WinCred => wincred::delete(key),
            Secrets::Keyring => {
                // `clear` для отсутствующего секрета не считаем ошибкой
                run(&["clear", "application", APP, "key", key], None).await.map(|_| ())
            }
        }
    }
}

#[cfg_attr(windows, allow(dead_code))]
async fn run(args: &[&str], stdin: Option<&str>) -> anyhow::Result<std::process::Output> {
    let mut child = Command::new("secret-tool")
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("не удалось запустить secret-tool")?;
    if let (Some(value), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(value.as_bytes()).await?;
        drop(pipe);
    }
    tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("связка ключей не ответила за минуту (возможно, она заблокирована)"))?
        .context("secret-tool завершился с ошибкой")
}

/// Диспетчер учётных данных Windows. Запись общая для всех процессов пользователя; значение хранится как UTF-16,
/// поэтому в окне «Диспетчер учётных данных» оно читается, а не показывается как мусор.
#[cfg(windows)]
mod wincred {
    use anyhow::{bail, Result};
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_NOT_FOUND},
        Security::Credentials::{CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC},
    };

    /// Предел Windows на размер значения одной записи (CRED_MAX_CREDENTIAL_BLOB_SIZE).
    const MAX_BLOB: usize = 2560;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target(key: &str) -> Vec<u16> {
        wide(&format!("{}/{key}", super::APP))
    }

    fn fail(what: &str, code: u32) -> anyhow::Error {
        anyhow::anyhow!("диспетчер учётных данных Windows ({what}): ошибка {code}")
    }

    pub fn read(key: &str) -> Result<Option<String>> {
        let name = target(key);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: имя заканчивается нулём; при успехе `cred` указывает на запись, которую освобождаем CredFree
        unsafe {
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                let code = GetLastError();
                return if code == ERROR_NOT_FOUND { Ok(None) } else { Err(fail("чтение", code)) };
            }
            let c = &*cred;
            let units: Vec<u16> = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect()
            };
            CredFree(cred as *const _);
            Ok(Some(String::from_utf16_lossy(&units)).filter(|s| !s.is_empty()))
        }
    }

    pub fn write(key: &str, value: &str) -> Result<()> {
        let mut blob: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
        if blob.len() > MAX_BLOB {
            bail!("значение слишком длинное для Диспетчера учётных данных Windows: {} знаков, а можно не больше {}", blob.len() / 2, MAX_BLOB / 2);
        }
        let mut name = target(key);
        let mut user = wide(super::APP);
        let mut comment = wide("Автонарезка");
        let mut cred = CREDENTIALW::default();
        cred.Type = CRED_TYPE_GENERIC;
        cred.TargetName = name.as_mut_ptr();
        cred.UserName = user.as_mut_ptr();
        cred.Comment = comment.as_mut_ptr();
        cred.CredentialBlobSize = blob.len() as u32;
        cred.CredentialBlob = blob.as_mut_ptr();
        cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
        // SAFETY: все указатели в `cred` живут до конца вызова; CredWriteW копирует данные
        let ok = unsafe { CredWriteW(&cred, 0) };
        if ok == 0 {
            return Err(fail("запись", unsafe { GetLastError() }));
        }
        Ok(())
    }

    pub fn delete(key: &str) -> Result<()> {
        let name = target(key);
        // SAFETY: имя заканчивается нулём
        if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let code = unsafe { GetLastError() };
            // удалять нечего — не ошибка
            if code != ERROR_NOT_FOUND {
                return Err(fail("удаление", code));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn roundtrip_overwrite_and_delete() {
            let key = format!("test_{}", std::process::id());
            assert_eq!(read(&key).unwrap(), None);
            write(&key, "секрет-1").unwrap();
            write(&key, "секрет-2 ✓").unwrap();
            assert_eq!(read(&key).unwrap().as_deref(), Some("секрет-2 ✓"));
            delete(&key).unwrap();
            assert_eq!(read(&key).unwrap(), None);
            delete(&key).unwrap();
        }

        #[test]
        fn too_long_value_is_refused() {
            assert!(write("test_long", &"я".repeat(1281)).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_backend_stores_overwrites_and_deletes() {
        let s = Secrets::memory();
        assert_eq!(s.get("k").await.unwrap(), None);
        s.set("k", "один").await.unwrap();
        s.set("k", "два").await.unwrap();
        assert_eq!(s.get("k").await.unwrap().as_deref(), Some("два"));
        s.delete("k").await.unwrap();
        assert_eq!(s.get("k").await.unwrap(), None);
        s.delete("k").await.unwrap(); // повторное удаление — не ошибка
    }
}
