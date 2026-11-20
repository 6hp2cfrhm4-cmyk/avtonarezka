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
    /// Зашифрованный файл (XChaCha20-Poly1305) для сервера без связки ключей и D-Bus.
    File(filestore::FileStore),
}

impl Secrets {
    /// Хранилище системы, если оно есть (на Windows всегда; на Linux — при наличии `secret-tool`), иначе память.
    pub fn detect() -> Self {
        #[cfg(debug_assertions)]
        if std::env::var_os("AVTO_TEST_SECRET_MEMORY").is_some() {
            return Self::memory();
        }
        #[cfg(windows)]
        return Secrets::WinCred;
        #[cfg(not(windows))]
        Self::detect_secret_tool()
    }

    #[cfg(not(windows))]
    fn detect_secret_tool() -> Self {
        let found = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("secret-tool").is_file()));
        if found {
            Secrets::Keyring
        } else {
            Secrets::memory()
        }
    }

    /// Для демона: зашифрованный файл в `data_dir`, если задан ключ (`AVTO_SECRETS_KEY_FILE` или
    /// systemd-credential `secrets-key`), иначе память с предупреждением.
    pub fn detect_headless(data_dir: &std::path::Path) -> Self {
        match filestore::FileStore::from_env(data_dir.join("secrets.enc")) {
            Ok(Some(store)) => Secrets::File(store),
            Ok(None) => {
                tracing::warn!("ключ шифрования секретов не задан (AVTO_SECRETS_KEY_FILE): секреты живут только в памяти");
                Self::memory()
            }
            Err(e) => {
                tracing::error!("хранилище секретов недоступно: {e:#}; секреты только в памяти");
                Self::memory()
            }
        }
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
            Secrets::File(_) => "file",
        }
    }

    /// Синхронный доступ для серверных операций (демон и `avtoctl`): только файловое хранилище.
    pub fn get_sync(&self, key: &str) -> anyhow::Result<Option<String>> {
        match self {
            Secrets::File(f) => f.get(key),
            _ => bail!("хранилище секретов сервера не настроено (нет ключа шифрования)"),
        }
    }
    pub fn set_sync(&self, key: &str, value: &str) -> anyhow::Result<()> {
        match self {
            Secrets::File(f) => f.set(key, value),
            _ => bail!("хранилище секретов сервера не настроено (нет ключа шифрования)"),
        }
    }
    pub fn delete_sync(&self, key: &str) -> anyhow::Result<()> {
        match self {
            Secrets::File(f) => f.delete(key),
            _ => bail!("хранилище секретов сервера не настроено (нет ключа шифрования)"),
        }
    }

    pub async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        match self {
            Secrets::Memory(m) => Ok(m.lock().unwrap().get(key).cloned()),
            Secrets::File(f) => f.get(key),
            #[cfg(windows)]
            Secrets::WinCred => wincred::read(key),
            Secrets::Keyring => {
                let out = run(&["lookup", "application", APP, "key", key], None).await?;
                // код 1 и пустой вывод — секрета нет; остальное — сбой связки ключей
                if out.status.success() {
                    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())
                        .filter(|s| !s.is_empty()))
                } else if out.status.code() == Some(1) && out.stderr.is_empty() {
                    Ok(None)
                } else {
                    bail!(
                        "связка ключей: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )
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
            Secrets::File(f) => f.set(key, value),
            #[cfg(windows)]
            Secrets::WinCred => wincred::write(key, value),
            Secrets::Keyring => {
                let label = format!("Автонарезка: {key}");
                let out = run(
                    &[
                        "store",
                        &format!("--label={label}"),
                        "application",
                        APP,
                        "key",
                        key,
                    ],
                    Some(value),
                )
                .await?;
                if !out.status.success() {
                    bail!(
                        "не удалось сохранить в связку ключей: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
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
            Secrets::File(f) => f.delete(key),
            #[cfg(windows)]
            Secrets::WinCred => wincred::delete(key),
            Secrets::Keyring => {
                // `clear` для отсутствующего секрета не считаем ошибкой
                run(&["clear", "application", APP, "key", key], None)
                    .await
                    .map(|_| ())
            }
        }
    }
}

#[cfg_attr(windows, allow(dead_code))]
async fn run(args: &[&str], stdin: Option<&str>) -> anyhow::Result<std::process::Output> {
    let mut child = Command::new("secret-tool")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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
        .map_err(|_| {
            anyhow::anyhow!("связка ключей не ответила за минуту (возможно, она заблокирована)")
        })?
        .context("secret-tool завершился с ошибкой")
}

/// Файловое хранилище: все секреты одним JSON, зашифрованным целиком, запись атомарная (временный файл + rename), права 0600.
mod filestore {
    use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

    use anyhow::{bail, Context, Result};
    use chacha20poly1305::{
        aead::{Aead, KeyInit},
        Key, XChaCha20Poly1305, XNonce,
    };
    use rand::RngCore;

    const MAGIC: &[u8; 4] = b"AVS1";
    const NONCE: usize = 24;

    pub struct FileStore {
        path: PathBuf,
        key: [u8; 32],
        lock: Mutex<()>,
    }

    impl FileStore {
        pub fn new(path: PathBuf, key: [u8; 32]) -> Self {
            Self {
                path,
                key,
                lock: Mutex::new(()),
            }
        }

        /// Ключ: 64 hex-символа в файле `AVTO_SECRETS_KEY_FILE` либо в `$CREDENTIALS_DIRECTORY/secrets-key`.
        pub fn from_env(path: PathBuf) -> Result<Option<Self>> {
            let key_file = std::env::var_os("AVTO_SECRETS_KEY_FILE")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("CREDENTIALS_DIRECTORY")
                        .map(|d| PathBuf::from(d).join("secrets-key"))
                })
                // серверная установка: ключ лежит в /etc/avtonarezka и читается группой avtonarezka (его читает и avtoctl)
                .or_else(|| Some(PathBuf::from("/etc/avtonarezka/secrets.key")))
                .filter(|p| p.is_file());
            let Some(key_file) = key_file else {
                return Ok(None);
            };
            let text = std::fs::read_to_string(&key_file)
                .with_context(|| format!("не читается ключ {}", key_file.display()))?;
            let bytes =
                hex::decode(text.trim()).context("ключ секретов должен быть 64 hex-символа")?;
            let key: [u8; 32] = bytes.try_into().map_err(|_| {
                anyhow::anyhow!("ключ секретов должен быть 32 байта (64 hex-символа)")
            })?;
            Ok(Some(Self::new(path, key)))
        }

        fn cipher(&self) -> XChaCha20Poly1305 {
            XChaCha20Poly1305::new(Key::from_slice(&self.key))
        }

        fn load(&self) -> Result<BTreeMap<String, String>> {
            let data = match std::fs::read(&self.path) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
                Err(e) => return Err(e).context("не читается файл секретов"),
            };
            if data.len() < MAGIC.len() + NONCE || &data[..MAGIC.len()] != MAGIC {
                bail!("файл секретов повреждён");
            }
            let (nonce, body) = data[MAGIC.len()..].split_at(NONCE);
            let plain = self
                .cipher()
                .decrypt(XNonce::from_slice(nonce), body)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "файл секретов не расшифровывается (неверный ключ или повреждён)"
                    )
                })?;
            serde_json::from_slice(&plain).context("файл секретов: неверное содержимое")
        }

        fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
            let mut nonce = [0u8; NONCE];
            rand::rng().fill_bytes(&mut nonce);
            let body = self
                .cipher()
                .encrypt(
                    XNonce::from_slice(&nonce),
                    serde_json::to_vec(map)?.as_slice(),
                )
                .map_err(|_| anyhow::anyhow!("не удалось зашифровать секреты"))?;
            let mut out = Vec::with_capacity(MAGIC.len() + NONCE + body.len());
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&nonce);
            out.extend_from_slice(&body);
            let tmp = self.path.with_extension("enc.tmp");
            write_private(&tmp, &out)?;
            std::fs::rename(&tmp, &self.path).context("не удалось заменить файл секретов")
        }

        pub fn get(&self, key: &str) -> Result<Option<String>> {
            let _g = self.lock.lock().unwrap();
            Ok(self.load()?.get(key).cloned())
        }

        pub fn set(&self, key: &str, value: &str) -> Result<()> {
            let _g = self.lock.lock().unwrap();
            let mut map = self.load()?;
            map.insert(key.to_string(), value.to_string());
            self.save(&map)
        }

        pub fn delete(&self, key: &str) -> Result<()> {
            let _g = self.lock.lock().unwrap();
            let mut map = self.load()?;
            if map.remove(key).is_some() {
                self.save(&map)?;
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)?;
        f.sync_all()?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
        std::fs::write(path, data)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn store(dir: &std::path::Path, key: u8) -> FileStore {
            FileStore::new(dir.join("secrets.enc"), [key; 32])
        }

        #[test]
        fn roundtrip_persists_and_is_not_plaintext() {
            let dir = tempdir();
            let s = store(&dir, 7);
            assert_eq!(s.get("a").unwrap(), None);
            s.set("a", "секрет-1").unwrap();
            s.set("b", "2").unwrap();
            // новый экземпляр читает то же
            let again = store(&dir, 7);
            assert_eq!(again.get("a").unwrap().as_deref(), Some("секрет-1"));
            let raw = std::fs::read(dir.join("secrets.enc")).unwrap();
            assert!(!String::from_utf8_lossy(&raw).contains("секрет"));
            again.delete("a").unwrap();
            assert_eq!(store(&dir, 7).get("a").unwrap(), None);
            assert_eq!(store(&dir, 7).get("b").unwrap().as_deref(), Some("2"));
        }

        #[test]
        fn wrong_key_and_corruption_are_errors_not_silent_loss() {
            let dir = tempdir();
            store(&dir, 1).set("a", "x").unwrap();
            assert!(store(&dir, 2).get("a").is_err());
            let p = dir.join("secrets.enc");
            let mut raw = std::fs::read(&p).unwrap();
            *raw.last_mut().unwrap() ^= 1;
            std::fs::write(&p, raw).unwrap();
            assert!(store(&dir, 1).get("a").is_err());
            std::fs::write(&p, b"junk").unwrap();
            assert!(store(&dir, 1).set("a", "y").is_err());
        }

        #[cfg(unix)]
        #[test]
        fn file_is_private() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempdir();
            store(&dir, 3).set("a", "x").unwrap();
            let mode = std::fs::metadata(dir.join("secrets.enc"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }

        fn tempdir() -> PathBuf {
            let d = std::env::temp_dir().join(format!(
                "avs-{}-{}",
                std::process::id(),
                rand::random::<u32>()
            ));
            std::fs::create_dir_all(&d).unwrap();
            d
        }
    }
}

/// Диспетчер учётных данных Windows. Запись общая для всех процессов пользователя; значение хранится как UTF-16,
/// поэтому в окне «Диспетчер учётных данных» оно читается, а не показывается как мусор.
#[cfg(windows)]
mod wincred {
    use anyhow::{bail, Result};
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_NOT_FOUND},
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
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
                return if code == ERROR_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(fail("чтение", code))
                };
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
