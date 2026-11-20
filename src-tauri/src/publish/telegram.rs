//! Bot API: потоковый multipart, без неявного повтора потенциально принятой публикации.
use super::*;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::time::Duration;

pub struct Telegram {
    http: reqwest::Client,
    endpoint: String,
    token: String,
    chat: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCheck {
    pub bot_username: String,
    pub chat_id: String,
    pub chat_title: String,
    pub checked_at: i64,
}
impl Telegram {
    pub fn new(token: String, chat: String) -> Result<Self, Failure> {
        Self::with_endpoint(token, chat, "https://api.telegram.org".into())
    }
    /// Поддельный сервер допускается только на loopback. Токен никогда не отправляется на сторонний host.
    pub fn with_endpoint(token: String, chat: String, endpoint: String) -> Result<Self, Failure> {
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'_' || b == b'-')
            || !token.contains(':')
        {
            return Err(Failure::new(
                FailureKind::Auth,
                "Некорректный токен бота Telegram",
            ));
        }
        let chat = chat.trim().to_string();
        if chat.is_empty() || chat.contains(char::is_whitespace) {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Укажи @имя канала или числовой chat_id",
            ));
        }
        let url = reqwest::Url::parse(&endpoint)
            .map_err(|_| Failure::new(FailureKind::Rejected, "Некорректный адрес Bot API"))?;
        let production = url.scheme() == "https"
            && url.host_str() == Some("api.telegram.org")
            && url.port().is_none();
        let local = url.scheme() == "http"
            && [Some("127.0.0.1"), Some("[::1]"), Some("localhost")].contains(&url.host_str());
        if (!production && !local)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Bot API разрешён только на официальном адресе или тестовом loopback",
            ));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(1800))
            .build()
            .map_err(|_| {
                Failure::new(FailureKind::Rejected, "Не удалось создать клиент Telegram")
            })?;
        Ok(Self {
            http,
            endpoint: endpoint.trim_end_matches('/').into(),
            token,
            chat,
        })
    }
    /// Только чтение: ни одного метода отправки или изменения прав.
    async fn read(&self, method: &str, fields: &[(&str, String)]) -> Result<Value, Failure> {
        let invalid = || {
            Failure::new(
                FailureKind::Rejected,
                "Не удалось прочитать ответ проверки Telegram",
            )
        };
        let mut response = self
            .http
            .post(format!("{}/bot{}/{method}", self.endpoint, self.token))
            .timeout(Duration::from_secs(20))
            .form(fields)
            .send()
            .await
            .map_err(|_| {
                Failure::new(
                    FailureKind::Rejected,
                    "Telegram не ответил. Проверь сеть и повтори проверку доступа",
                )
            })?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            return Err(Failure::new(
                FailureKind::Auth,
                "Telegram отклонил доступ: проверь токен и права бота в канале",
            ));
        }
        if response.status().is_redirection() {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Telegram вернул перенаправление. Проверка остановлена",
            ));
        }
        if status >= 500 {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Сбой сервера Telegram. Повтори проверку позже",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len() + chunk.len() > 65_536 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if body["error_code"] == 401 || body["error_code"] == 403 {
            return Err(Failure::new(
                FailureKind::Auth,
                "Telegram отклонил доступ: проверь токен и права бота в канале",
            ));
        }
        if status == 429 || body["error_code"] == 429 {
            let seconds = body["parameters"]["retry_after"]
                .as_u64()
                .unwrap_or(60)
                .clamp(1, 86400);
            return Err(Failure::new(
                FailureKind::RetryAfter(seconds),
                format!("Telegram ограничил частоту проверок. Повтори через {seconds} с"),
            ));
        }
        if status >= 500 {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Сбой сервера Telegram. Повтори проверку позже",
            ));
        }
        if !(200..300).contains(&status) || body["ok"] != true {
            return Err(Failure::new(FailureKind::Rejected,"Не удалось проверить канал Telegram. Проверь chat_id и добавь бота администратором"));
        }
        Ok(body["result"].clone())
    }
    pub async fn check_connection(&self) -> Result<ConnectionCheck, Failure> {
        let valid_name = self.chat.strip_prefix('@').is_some_and(|s| {
            s.len() >= 5
                && s.len() <= 32
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        if !valid_name && !self.chat.parse::<i64>().is_ok_and(|id| id != 0) {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Укажи @имя канала или числовой chat_id",
            ));
        }
        let bot = self.read("getMe", &[]).await?;
        let bot_id = bot["id"].as_i64().filter(|id| *id > 0);
        let username = bot["username"].as_str().filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        let (Some(bot_id), Some(username), true) = (bot_id, username, bot["is_bot"] == true) else {
            return Err(Failure::new(
                FailureKind::Auth,
                "Telegram не подтвердил личность бота",
            ));
        };
        let chat = self
            .read("getChat", &[("chat_id", self.chat.clone())])
            .await?;
        let id = chat["id"]
            .as_i64()
            .filter(|id| *id < 0)
            .ok_or_else(|| Failure::new(FailureKind::Rejected, "Telegram не вернул ID канала"))?;
        if chat["type"] != "channel" {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Нужен канал Telegram. Группы и личные чаты этой проверкой не поддерживаются",
            ));
        }
        let matches = if let Some(wanted) = self.chat.strip_prefix('@') {
            chat["username"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(wanted))
        } else {
            self.chat.parse::<i64>().ok() == Some(id)
        };
        if !matches {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Telegram вернул другой канал. Проверь его ID или имя",
            ));
        }
        let member = self
            .read(
                "getChatMember",
                &[("chat_id", id.to_string()), ("user_id", bot_id.to_string())],
            )
            .await?;
        if member["user"]["id"].as_i64() != Some(bot_id) || member["user"]["is_bot"] != true {
            return Err(Failure::new(
                FailureKind::Auth,
                "Telegram не подтвердил права этого бота",
            ));
        }
        if member["status"] != "administrator" || member["can_post_messages"] != true {
            return Err(Failure::new(
                FailureKind::Auth,
                "Бот должен быть администратором канала с правом публикации сообщений",
            ));
        }
        Ok(ConnectionCheck {
            bot_username: username.into(),
            chat_id: id.to_string(),
            chat_title: chat["title"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(255)
                .collect(),
            checked_at: crate::tiktok::api::now_unix(),
        })
    }
    async fn send(
        &self,
        upload: &Upload,
        on: &mut (dyn FnMut(f64, String) + Send),
    ) -> Result<Receipt, Failure> {
        validate(upload, TELEGRAM)?;
        if upload
            .scheduled_at
            .is_some_and(|at| at > crate::tiktok::api::now_unix())
        {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Telegram отправляет в назначенный момент через календарь приложения",
            ));
        }
        let size = tokio::fs::metadata(&upload.path)
            .await
            .map_err(|_| Failure::new(FailureKind::Rejected, "Не найден готовый файл ролика"))?
            .len();
        if size == 0 || size > TELEGRAM.file_max.unwrap() {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Bot API принимает ролики размером до 50 МБ; нужен вариант меньшего размера",
            ));
        }
        let part = reqwest::multipart::Part::file(&upload.path)
            .await
            .map_err(|_| {
                Failure::new(
                    FailureKind::Rejected,
                    "Не удалось открыть ролик для отправки",
                )
            })?
            .file_name("video.mp4")
            .mime_str("video/mp4")
            .map_err(|_| Failure::new(FailureKind::Rejected, "Некорректный формат файла"))?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat.clone())
            .text("caption", upload.caption.clone())
            .text("supports_streaming", "true")
            .part("video", part);
        on(0.01, "Отправляю ролик в Telegram".into());
        let response = self
            .http
            .post(format!("{}/bot{}/sendVideo", self.endpoint, self.token))
            .multipart(form)
            .send()
            .await
            // reqwest::Error содержит URL с токеном. Никогда не включать его в сообщение/журнал.
            .map_err(|_| {
                Failure::new(
                    FailureKind::NeedsCheck,
                    "Связь с Telegram прервалась. Проверь канал перед повторной отправкой",
                )
            })?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().await.map_err(|_| {
            Failure::new(
                FailureKind::NeedsCheck,
                "Не удалось прочитать ответ Telegram. Проверь канал перед повтором",
            )
        })?;
        if status == 401 || status == 403 || body["error_code"] == 401 || body["error_code"] == 403
        {
            return Err(Failure::new(
                FailureKind::Auth,
                "Telegram отклонил доступ: проверь токен и права бота в канале",
            ));
        }
        if status == 429 || body["error_code"] == 429 {
            return Err(Failure::new(
                FailureKind::RetryAfter(
                    body["parameters"]["retry_after"]
                        .as_u64()
                        .unwrap_or(60)
                        .clamp(1, 86400),
                ),
                "Telegram ограничил частоту отправок",
            ));
        }
        if status >= 500 {
            return Err(Failure::new(
                FailureKind::NeedsCheck,
                "Сбой сервера Telegram. Проверь, появился ли ролик, прежде чем повторять",
            ));
        }
        if !(200..300).contains(&status) || body["ok"] != true {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Telegram не принял ролик. Проверь chat_id, формат файла и разрешения бота",
            ));
        }
        let result = &body["result"];
        let message_id = result["message_id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::NeedsCheck,
                    "Telegram не вернул номер сообщения. Проверь канал",
                )
            })?;
        let chat_id = result["chat"]["id"].as_i64().ok_or_else(|| {
            Failure::new(
                FailureKind::NeedsCheck,
                "Telegram не вернул номер канала. Проверь канал",
            )
        })?;
        let username = result["chat"]["username"]
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
        let url = username.map(|name| format!("https://t.me/{name}/{message_id}"));
        on(1.0, "Опубликовано в Telegram".into());
        Ok(Receipt {
            external_id: format!("{chat_id}:{message_id}"),
            url,
            published: true,
        })
    }
}
impl Platform for Telegram {
    fn caps(&self) -> Caps {
        TELEGRAM
    }
    fn publish<'a>(
        &'a self,
        upload: &'a Upload,
        on: &'a mut (dyn FnMut(f64, String) + Send),
    ) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(self.send(upload, on))
    }
}
