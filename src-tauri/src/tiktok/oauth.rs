//! Вход в TikTok: браузер пользователя → адрес на нашем локальном сервере → код.
//! Сервер слушает только 127.0.0.1, принимает один запрос с верным `state` и сразу выключается.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail, Context};
use axum::{extract::{Query, State}, response::Html, routing::get, Router};
use tokio::{net::TcpListener, sync::{oneshot, watch}};

type Slot = Arc<Mutex<Option<oneshot::Sender<Result<String, String>>>>>;

#[derive(Clone)]
struct Shared {
    state: Arc<str>,
    slot: Slot,
}

fn page(title: &str, text: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>Автонарезка</title><body style=\"font-family:system-ui;background:#0b0b13;color:#ecebf5;display:grid;place-items:center;height:100vh;margin:0\"><div style=\"text-align:center\"><h1 style=\"margin:0 0 8px\">{title}</h1><p style=\"opacity:.6\">{text}</p></div>"
    ))
}

async fn callback(State(sh): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Html<String> {
    // чужие и устаревшие запросы (favicon, повторное открытие) игнорируем: вход они не прерывают
    if q.get("state").map(String::as_str) != Some(&*sh.state) {
        return page("Это не тот адрес", "Вернись в Автонарезку и начни вход заново.");
    }
    let result = if let Some(err) = q.get("error") {
        let why = q.get("error_description").filter(|d| !d.is_empty()).unwrap_or(err);
        Err(if err == "access_denied" { "Доступ не разрешён".to_string() } else { format!("TikTok отказал: {why}") })
    } else {
        match q.get("code").filter(|c| !c.is_empty()) {
            Some(code) => Ok(code.clone()),
            None => Err("TikTok не прислал код".to_string()),
        }
    };
    let ok = result.is_ok();
    if let Some(tx) = sh.slot.lock().unwrap().take() {
        let _ = tx.send(result);
    }
    if ok {
        page("Готово", "Можно закрыть эту вкладку и вернуться в Автонарезку.")
    } else {
        page("Вход не удался", "Вернись в Автонарезку: там написана причина.")
    }
}

/// Ждёт код на уже занятом порту. Ошибка, если время вышло, вход отменили или TikTok отказал.
pub async fn wait_for_code(listener: TcpListener, expected_state: &str, timeout: Duration, mut cancel: watch::Receiver<bool>) -> anyhow::Result<String> {
    let (tx, rx) = oneshot::channel();
    let shared = Shared { state: expected_state.into(), slot: Arc::new(Mutex::new(Some(tx))) };
    let app = Router::new().route("/callback/", get(callback)).route("/callback", get(callback)).with_state(shared);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let result = tokio::select! {
        r = rx => r.map_err(|_| anyhow!("приём ответа остановлен")).and_then(|r| r.map_err(|e| anyhow!(e))),
        _ = tokio::time::sleep(timeout) => Err(anyhow!("Время на вход вышло. Нажми «Подключить» ещё раз.")),
        _ = async { while cancel.changed().await.is_ok() { if *cancel.borrow() { return; } } std::future::pending::<()>().await } => Err(anyhow!("Вход отменён")),
    };
    server.abort();
    result
}

/// Занимает порт, пока не открыта страница входа: иначе ответ мог бы прийти до запуска сервера.
pub async fn bind(port: u16) -> anyhow::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("порт {port} занят. Выбери другой в настройках TikTok и добавь такой же адрес в кабинете TikTok"))
}

/// Открывает адрес в браузере по умолчанию. Только http(s): системные открывалки умеют и другие схемы.
pub fn open_browser(url: &str) -> anyhow::Result<()> {
    let u = reqwest::Url::parse(url).context("неверный адрес")?;
    if !matches!(u.scheme(), "http" | "https") {
        bail!("можно открывать только http и https");
    }
    crate::platform::open_url(url).with_context(|| format!("не удалось открыть браузер{}", crate::platform::OPENER_HINT))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn start(state: &str, timeout: Duration) -> (u16, tokio::task::JoinHandle<anyhow::Result<String>>, watch::Sender<bool>) {
        let l = bind(0).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = watch::channel(false);
        let st = state.to_string();
        (port, tokio::spawn(async move { wait_for_code(l, &st, timeout, rx).await }), tx)
    }

    async fn get(port: u16, query: &str) -> String {
        reqwest::get(format!("http://127.0.0.1:{port}/callback/?{query}")).await.unwrap().text().await.unwrap()
    }

    #[tokio::test]
    async fn correct_state_yields_the_code_and_a_friendly_page() {
        let (port, task, _tx) = start("s1", Duration::from_secs(10)).await;
        assert!(get(port, "code=abc&state=s1&scopes=video.upload").await.contains("Готово"));
        assert_eq!(task.await.unwrap().unwrap(), "abc");
    }

    #[tokio::test]
    async fn wrong_or_missing_state_is_ignored_and_login_keeps_waiting() {
        let (port, task, _tx) = start("good", Duration::from_secs(10)).await;
        assert!(get(port, "code=evil&state=bad").await.contains("не тот адрес"));
        assert!(get(port, "code=evil").await.contains("не тот адрес"));
        assert!(!task.is_finished(), "чужой запрос не должен завершать вход");
        get(port, "code=real&state=good").await;
        assert_eq!(task.await.unwrap().unwrap(), "real");
    }

    #[tokio::test]
    async fn user_denial_is_reported_in_plain_language() {
        let (port, task, _tx) = start("s", Duration::from_secs(10)).await;
        get(port, "error=access_denied&state=s").await;
        assert_eq!(task.await.unwrap().unwrap_err().to_string(), "Доступ не разрешён");
    }

    #[tokio::test]
    async fn timeout_and_cancel_end_the_wait() {
        let (_, task, _tx) = start("s", Duration::from_millis(150)).await;
        assert!(task.await.unwrap().unwrap_err().to_string().contains("Время на вход вышло"));

        let (_, task, tx) = start("s", Duration::from_secs(30)).await;
        tx.send(true).unwrap();
        assert!(task.await.unwrap().unwrap_err().to_string().contains("отменён"));
    }

    #[tokio::test]
    async fn port_is_released_after_login_and_busy_port_is_explained() {
        let l = bind(0).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let e = bind(port).await.unwrap_err().to_string();
        assert!(e.contains("занят"), "{e}");
        drop(l);
        assert!(bind(port).await.is_ok());
    }

    #[test]
    fn only_web_addresses_can_be_opened() {
        assert!(open_browser("file:///etc/passwd").is_err());
        assert!(open_browser("javascript:alert(1)").is_err());
        assert!(open_browser("не адрес").is_err());
    }
}
