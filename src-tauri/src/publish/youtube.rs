//! Resumable YouTube upload. Не создаём новую сессию после неоднозначного PUT:
//! сначала узнаём позицию той же сессии. После аварии общая очередь → needs_check.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{io::SeekFrom, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub mod accounts;
pub const SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly";
const CHUNK: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub privacy: String,
    pub made_for_kids: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            privacy: "private".into(),
            made_for_kids: false,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !["private", "unlisted", "public"].contains(&self.privacy.as_str()) {
            return Err("Выбери видимость YouTube".into());
        }
        Ok(())
    }
}
pub struct YouTube {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    access: String,
    title: String,
    settings: Settings,
}
fn failure(kind: FailureKind, message: &str) -> Failure {
    Failure::new(kind, message)
}
fn local(url: &reqwest::Url) -> bool {
    url.scheme() == "http"
        && [Some("127.0.0.1"), Some("[::1]"), Some("localhost")].contains(&url.host_str())
}
pub(crate) fn endpoint(value: &str, host: &str) -> Result<reqwest::Url, Failure> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| failure(FailureKind::Rejected, "Неверный адрес API"))?;
    if (!local(&url)
        && !(url.scheme() == "https" && url.host_str() == Some(host) && url.port().is_none()))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(failure(
            FailureKind::Rejected,
            "API разрешён только на официальном адресе или тестовом loopback",
        ));
    }
    Ok(url)
}
pub(crate) fn http() -> Result<reqwest::Client, Failure> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|_| failure(FailureKind::Rejected, "Не удалось создать клиент YouTube"))
}
impl YouTube {
    pub fn new(access: String, title: String, settings: Settings) -> Result<Self, Failure> {
        Self::with_endpoint(access, title, settings, "https://www.googleapis.com/")
    }
    pub fn with_endpoint(
        access: String,
        title: String,
        settings: Settings,
        url: &str,
    ) -> Result<Self, Failure> {
        settings
            .validate()
            .map_err(|e| Failure::new(FailureKind::Rejected, e))?;
        let title = title.trim().to_string();
        if title.is_empty()
            || title.encode_utf16().count() > YOUTUBE.title_max
            || title.contains(['<', '>'])
        {
            return Err(failure(
                FailureKind::Rejected,
                "Заголовок YouTube: 1–100 знаков, без угловых скобок",
            ));
        }
        if access.is_empty() || access.contains(char::is_whitespace) {
            return Err(failure(FailureKind::Auth, "Подключи аккаунт YouTube"));
        }
        Ok(Self {
            http: http()?,
            endpoint: endpoint(url, "www.googleapis.com")?,
            access,
            title,
            settings,
        })
    }
    fn session_url(&self, raw: &str) -> Result<reqwest::Url, Failure> {
        let url = reqwest::Url::parse(raw).map_err(|_| {
            failure(
                FailureKind::NeedsCheck,
                "YouTube не вернул адрес сессии загрузки",
            )
        })?;
        let trusted = if local(&self.endpoint) {
            url.origin() == self.endpoint.origin()
        } else {
            url.scheme() == "https"
                && [Some("www.googleapis.com"), Some("youtube.googleapis.com")]
                    .contains(&url.host_str())
                && url.port().is_none()
        };
        if !trusted
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(failure(
                FailureKind::NeedsCheck,
                "YouTube вернул неподдерживаемый адрес сессии",
            ));
        }
        Ok(url)
    }
    async fn send(
        &self,
        upload: &Upload,
        on: &mut (dyn FnMut(f64, String) + Send),
    ) -> Result<Receipt, Failure> {
        validate(upload, YOUTUBE)?;
        if upload.caption.contains(['<', '>']) {
            return Err(failure(
                FailureKind::Rejected,
                "Описание YouTube не принимает угловые скобки",
            ));
        }
        let mut file = tokio::fs::File::open(&upload.path)
            .await
            .map_err(|_| failure(FailureKind::Rejected, "Файл ролика не найден"))?;
        let total = file
            .metadata()
            .await
            .map_err(|_| failure(FailureKind::Rejected, "Не удалось прочитать размер ролика"))?
            .len();
        if total == 0 || total > YOUTUBE.file_max.unwrap() {
            return Err(failure(
                FailureKind::Rejected,
                "Размер файла выходит за лимит YouTube",
            ));
        }
        let mut status = json!({"privacyStatus":self.settings.privacy,"selfDeclaredMadeForKids":self.settings.made_for_kids});
        if let Some(at) = upload.scheduled_at {
            if at <= crate::tiktok::api::now_unix() || self.settings.privacy != "private" {
                return Err(failure(
                    FailureKind::Rejected,
                    "Для publishAt нужна будущая дата и видимость «Приватное»",
                ));
            }
            status["publishAt"] = json!(chrono::DateTime::from_timestamp(at, 0)
                .ok_or_else(|| failure(FailureKind::Rejected, "Неверная дата публикации"))?
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
        let metadata = json!({"snippet":{"title":self.title,"description":upload.caption,"categoryId":"22"},"status":status});
        on(0.0, "Создаю сессию YouTube".into());
        let response=self.http.post(self.endpoint.join("upload/youtube/v3/videos?uploadType=resumable&part=snippet,status&notifySubscribers=false").unwrap())
            .bearer_auth(&self.access).header("X-Upload-Content-Type","video/mp4").header("X-Upload-Content-Length",total).json(&metadata).send().await
            .map_err(|_|failure(FailureKind::NeedsCheck,"Не получен ответ на создание загрузки YouTube. Проверь канал"))?;
        if !response.status().is_success() {
            return Err(api_error(response, false).await);
        }
        let session = self.session_url(
            response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .unwrap_or(""),
        )?;
        let (mut pos, mut failures) = (0u64, 0u8);
        loop {
            let response = if pos < total {
                file.seek(SeekFrom::Start(pos)).await.map_err(|_| {
                    failure(
                        FailureKind::NeedsCheck,
                        "Не удалось прочитать ролик во время загрузки",
                    )
                })?;
                let mut bytes = vec![0u8; CHUNK.min((total - pos) as usize)];
                file.read_exact(&mut bytes).await.map_err(|_| {
                    failure(
                        FailureKind::NeedsCheck,
                        "Файл ролика изменился во время загрузки",
                    )
                })?;
                self.http
                    .put(session.clone())
                    .bearer_auth(&self.access)
                    .header("Content-Type", "video/mp4")
                    .header(
                        "Content-Range",
                        format!("bytes {pos}-{}/{total}", pos + bytes.len() as u64 - 1),
                    )
                    .body(bytes)
                    .send()
                    .await
            } else {
                self.probe(&session, total).await
            };
            match response {
                Ok(r) if r.status().is_success() => return receipt(r, upload.scheduled_at).await,
                Ok(r) if r.status().as_u16() == 308 => {
                    let next = position(&r, total)?;
                    if next < pos {
                        return Err(failure(
                            FailureKind::NeedsCheck,
                            "YouTube сообщил противоречивую позицию загрузки. Проверь канал",
                        ));
                    }
                    if next == pos {
                        failures += 1;
                    } else {
                        failures = 0;
                    }
                    pos = next;
                }
                other => {
                    failures += 1;
                    let wait = match other {
                        Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
                            retry_after(&r).min(60)
                        }
                        Ok(r) => return Err(api_error(r, true).await),
                        Err(_) => 1,
                    };
                    if failures > 3 {
                        return Err(failure(
                            FailureKind::NeedsCheck,
                            "Загрузка YouTube прервалась. Проверь канал перед повтором",
                        ));
                    }
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    match self.probe(&session, total).await {
                        Ok(r) if r.status().is_success() => {
                            return receipt(r, upload.scheduled_at).await
                        }
                        Ok(r) if r.status().as_u16() == 308 => {
                            let next = position(&r, total)?;
                            if next < pos {
                                return Err(failure(
                                    FailureKind::NeedsCheck,
                                    "YouTube потерял позицию загрузки",
                                ));
                            }
                            pos = next;
                        }
                        Ok(r) if !r.status().is_server_error() && r.status().as_u16() != 429 => {
                            return Err(api_error(r, true).await)
                        }
                        _ => continue,
                    }
                }
            }
            if failures > 3 {
                return Err(failure(
                    FailureKind::NeedsCheck,
                    "YouTube не подтверждает продвижение загрузки. Проверь канал",
                ));
            }
            on(
                (pos as f64 / total as f64).min(0.99),
                "Загружаю ролик в YouTube".into(),
            );
        }
    }
    async fn probe(
        &self,
        session: &reqwest::Url,
        total: u64,
    ) -> Result<reqwest::Response, reqwest::Error> {
        self.http
            .put(session.clone())
            .bearer_auth(&self.access)
            .header("Content-Length", 0)
            .header("Content-Range", format!("bytes */{total}"))
            .send()
            .await
    }
}
fn retry_after(r: &reqwest::Response) -> u64 {
    r.headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .clamp(1, 86400)
}
fn position(r: &reqwest::Response, total: u64) -> Result<u64, Failure> {
    let Some(raw) = r.headers().get("range") else {
        return Ok(0);
    };
    let pos = raw
        .to_str()
        .ok()
        .and_then(|s| s.strip_prefix("bytes=0-"))
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|n| n.checked_add(1))
        .filter(|n| *n <= total);
    pos.ok_or_else(|| {
        failure(
            FailureKind::NeedsCheck,
            "YouTube вернул неверную позицию загрузки",
        )
    })
}
async fn api_error(r: reqwest::Response, during_upload: bool) -> Failure {
    let status = r.status().as_u16();
    let retry = retry_after(&r);
    let body = r.json::<Value>().await.unwrap_or_default();
    let reason = body["error"]["errors"][0]["reason"].as_str().unwrap_or("");
    if status == 401 {
        return failure(
            FailureKind::Auth,
            "Вход YouTube истёк. Подключи аккаунт заново",
        );
    }
    if ["quotaExceeded", "dailyLimitExceeded", "uploadLimitExceeded"].contains(&reason) {
        return failure(
            FailureKind::Rejected,
            "Исчерпана квота YouTube. Проверь лимит проекта и аккаунта",
        );
    }
    if status == 403 {
        return failure(
            FailureKind::Auth,
            "YouTube отклонил доступ. Проверь разрешения приложения и канала",
        );
    }
    if status == 429 && !during_upload {
        return failure(
            FailureKind::RetryAfter(retry),
            "YouTube ограничил частоту запросов",
        );
    }
    if during_upload || status >= 500 {
        failure(
            FailureKind::NeedsCheck,
            "Результат загрузки YouTube неизвестен. Проверь канал перед повтором",
        )
    } else {
        failure(FailureKind::Rejected, "YouTube отклонил параметры загрузки")
    }
}
async fn receipt(response: reqwest::Response, scheduled: Option<i64>) -> Result<Receipt, Failure> {
    let body = response.json::<Value>().await.map_err(|_| {
        failure(
            FailureKind::NeedsCheck,
            "Не удалось прочитать результат загрузки YouTube",
        )
    })?;
    let id = body["id"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or_else(|| {
            failure(
                FailureKind::NeedsCheck,
                "YouTube не вернул id ролика. Проверь канал",
            )
        })?;
    Ok(Receipt {
        external_id: id.into(),
        url: Some(format!("https://www.youtube.com/watch?v={id}")),
        published: scheduled.is_none(),
    })
}
impl Platform for YouTube {
    fn caps(&self) -> Caps {
        YOUTUBE
    }
    fn publish<'a>(
        &'a self,
        upload: &'a Upload,
        on: &'a mut (dyn FnMut(f64, String) + Send),
    ) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(self.send(upload, on))
    }
}
