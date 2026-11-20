//! Уведомления о требуемом действии. Без токенов, текста чата и автоматического повтора отправки.
use crate::host::{AppHandle, Manager, State};
use crate::{
    drafts::{kv_get, kv_set},
    state::AppState,
};
use rusqlite::Connection;
fn reserve(db: &Connection, key: &str, now: i64) -> bool {
    if kv_get(db, "desktop_notifications").as_deref() == Some("false") {
        return false;
    }
    let key = format!(
        "notification_last:{}",
        hex::encode(sha2::Sha256::digest(key.as_bytes()))
    );
    let last = kv_get(db, &key)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last < 900 {
        return false;
    }
    kv_set(db, &key, &now.to_string()).is_ok()
}
use sha2::Digest;
pub(crate) fn attention(app: &AppHandle, key: String, title: &'static str, body: &'static str) {
    let state = app.state::<AppState>();
    if !reserve(
        &state.db.0.lock().unwrap(),
        &key,
        chrono::Utc::now().timestamp(),
    ) {
        return;
    }
    let app = app.clone();
    crate::host::spawn(async move {
        if let Err(e) = deliver(title, body).await {
            tracing::warn!("Уведомление рабочего стола: {e}");
            let _ = app;
        }
    });
}
async fn deliver(title: &str, body: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        #[cfg(debug_assertions)]
        let executable = std::env::var_os("AVTO_TEST_NOTIFY")
            .map(std::path::PathBuf::from)
            .or_else(|| crate::platform::find_tool("notify-send"));
        #[cfg(not(debug_assertions))]
        let executable = crate::platform::find_tool("notify-send");
        let executable = executable
            .ok_or("Не найдена установленная notify-send. Событие остаётся в журнале приложения")?;
        let mut c = crate::platform::command_at(executable);
        c.args([
            "--app-name",
            "Автонарезка",
            "--icon",
            "dialog-warning",
            "--expire-time",
            "15000",
            "--",
            title,
            body,
        ]);
        c.kill_on_drop(true);
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), c.status())
            .await
            .map_err(|_| "Служба уведомлений не ответила")?
            .map_err(|_| "Не удалось вызвать службу уведомлений")?;
        if !result.success() {
            return Err("Служба уведомлений отклонила сообщение".into());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // Статический PowerShell-код; данные передаются через окружение, не подставляются в код.
        let script = r#"$ErrorActionPreference='Stop';[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]|Out-Null;[Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom,ContentType=WindowsRuntime]|Out-Null;$x=New-Object Windows.Data.Xml.Dom.XmlDocument;$x.LoadXml('<toast><visual><binding template="ToastGeneric"><text></text><text></text></binding></visual></toast>');$t=$x.GetElementsByTagName('text');$t.Item(0).InnerText=$env:AVTO_NOTIFICATION_TITLE;$t.Item(1).InnerText=$env:AVTO_NOTIFICATION_BODY;$n=[Windows.UI.Notifications.ToastNotification]::new($x);[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('app.avtonarezka.desktop').Show($n)"#;
        let mut c = crate::platform::command("powershell");
        c.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("AVTO_NOTIFICATION_TITLE", title)
        .env("AVTO_NOTIFICATION_BODY", body)
        .kill_on_drop(true);
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), c.status())
            .await
            .map_err(|_| "Служба уведомлений не ответила")?
            .map_err(|_| "Не удалось открыть службу уведомлений")?;
        if status.success() {
            Ok(())
        } else {
            Err("Служба уведомлений отклонила сообщение".into())
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (title, body);
        Err("Уведомления этой ОС не подключены".into())
    }
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn notifications_enabled(state: State<'_, AppState>) -> bool {
    kv_get(&state.db.0.lock().unwrap(), "desktop_notifications").as_deref() != Some("false")
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn notifications_set(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    kv_set(
        &state.db.0.lock().unwrap(),
        "desktop_notifications",
        if enabled { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_throttle_preference_and_independent_incidents() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        assert!(reserve(&c, "quota:1", 2000));
        assert!(!reserve(&c, "quota:1", 2001));
        assert!(reserve(&c, "captcha:1", 2001));
        assert!(reserve(&c, "quota:1", 2901));
        kv_set(&c, "desktop_notifications", "false").unwrap();
        assert!(!reserve(&c, "failure:2", 3000));
    }
}
