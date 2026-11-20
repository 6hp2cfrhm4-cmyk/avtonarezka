//! Facebook Login: REELS container → streaming rupload → status → media_publish.
//! media_publish никогда не повторяется после неопределённого ответа.
pub mod accounts;
use super::*;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::time::Duration;

const VERSION: &str = "v27.0"; // Версия из официального Content Publishing guide, 14.11.2026.
pub struct Instagram {
    http: Client,
    graph: Url,
    upload_base: Url,
    token: String,
    user: String,
    poll: Duration,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub id: String,
    pub username: String,
}
fn id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 40 && value.bytes().all(|b| b.is_ascii_digit())
}
fn local(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}
fn endpoint(raw: &str, host: &str) -> Result<Url, Failure> {
    let u = Url::parse(raw)
        .map_err(|_| Failure::new(FailureKind::Rejected, "Некорректный адрес Instagram API"))?;
    if !(local(&u) || u.scheme() == "https" && u.host_str() == Some(host) && u.port().is_none())
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
    {
        return Err(Failure::new(
            FailureKind::Rejected,
            "Instagram API разрешён только на официальном адресе или тестовом loopback",
        ));
    }
    Ok(u)
}
impl Instagram {
    pub fn new(token: String, user: String) -> Result<Self, Failure> {
        Self::with_endpoints(
            token,
            user,
            "https://graph.facebook.com/",
            "https://rupload.facebook.com/",
        )
    }
    pub fn with_endpoints(
        token: String,
        user: String,
        graph: &str,
        upload: &str,
    ) -> Result<Self, Failure> {
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) || !id(&user) {
            return Err(Failure::new(
                FailureKind::Auth,
                "Нужны токен Facebook и числовой ID профессионального аккаунта Instagram",
            ));
        }
        let graph = endpoint(graph, "graph.facebook.com")?;
        let upload_base = endpoint(upload, "rupload.facebook.com")?;
        if local(&graph) != local(&upload_base)
            || local(&graph) && graph.origin() != upload_base.origin()
        {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Тестовые Graph и rupload должны принадлежать одному loopback серверу",
            ));
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(1800))
            .build()
            .map_err(|_| {
                Failure::new(FailureKind::Rejected, "Не удалось создать клиент Instagram")
            })?;
        let poll = if local(&graph) {
            Duration::from_millis(5)
        } else {
            Duration::from_secs(60)
        };
        Ok(Self {
            http,
            graph,
            upload_base,
            token,
            user,
            poll,
        })
    }
    fn url(&self, path: &str) -> Url {
        self.graph.join(&format!("{VERSION}/{path}")).unwrap()
    }
    fn fields(&self, path: &str, fields: &str) -> Url {
        let mut url = self.url(path);
        url.query_pairs_mut().append_pair("fields", fields);
        url
    }
    async fn response(
        r: Result<reqwest::Response, reqwest::Error>,
        uncertain: bool,
    ) -> Result<Value, Failure> {
        let unknown = if uncertain {
            FailureKind::NeedsCheck
        } else {
            FailureKind::Rejected
        };
        let r = r.map_err(|_| {
            Failure::new(
                unknown.clone(),
                "Instagram не ответил. Проверь аккаунт перед повтором",
            )
        })?;
        let status = r.status();
        let retry = r
            .headers()
            .get("retry-after")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(1, 86400);
        let v = r.json::<Value>().await.map_err(|_| {
            Failure::new(
                unknown.clone(),
                "Не удалось прочитать ответ Instagram. Проверь аккаунт",
            )
        })?;
        let code = v["error"]["code"].as_u64().unwrap_or(0);
        if status.as_u16() == 401 || code == 190 {
            return Err(Failure::new(
                FailureKind::Auth,
                "Токен Instagram истёк или отклонён. Подключи аккаунт заново",
            ));
        }
        if status.as_u16() == 429 || matches!(code, 4 | 17 | 32 | 613) {
            return Err(Failure::new(
                if uncertain {
                    FailureKind::NeedsCheck
                } else {
                    FailureKind::RetryAfter(retry)
                },
                "Instagram ограничил частоту запросов",
            ));
        }
        if !status.is_success() || v.get("error").is_some() {
            return Err(Failure::new(
                if uncertain {
                    FailureKind::NeedsCheck
                } else {
                    FailureKind::Rejected
                },
                "Instagram отклонил запрос. Проверь права приложения и формат ролика",
            ));
        }
        Ok(v)
    }
    pub async fn identity(&self) -> Result<Identity, Failure> {
        let v = Self::response(
            self.http
                .get(self.fields(&self.user, "id,username,account_type"))
                .bearer_auth(&self.token)
                .send()
                .await,
            false,
        )
        .await?;
        if v["id"].as_str() != Some(&self.user)
            || !matches!(
                v["account_type"].as_str(),
                Some("BUSINESS" | "MEDIA_CREATOR")
            )
        {
            return Err(Failure::new(
                FailureKind::Auth,
                "Нужен Instagram Business/Creator, связанный со страницей Facebook",
            ));
        }
        Ok(Identity {
            id: self.user.clone(),
            username: v["username"].as_str().unwrap_or("Instagram").into(),
        })
    }
    pub async fn limit(&self) -> Result<(u64, u64), Failure> {
        let v = Self::response(
            self.http
                .get(self.fields(
                    &format!("{}/content_publishing_limit", self.user),
                    "quota_usage,config",
                ))
                .bearer_auth(&self.token)
                .send()
                .await,
            false,
        )
        .await?;
        let row = &v["data"][0];
        let usage = row["quota_usage"].as_u64();
        let total = row["config"]["quota_total"].as_u64().filter(|n| *n > 0);
        usage.zip(total).ok_or_else(|| {
            Failure::new(
                FailureKind::Rejected,
                "Instagram не сообщил текущий лимит публикаций. Проверь разрешения приложения",
            )
        })
    }
    fn upload_url(&self, raw: &str, container: &str) -> Result<Url, Failure> {
        let u = Url::parse(raw)
            .map_err(|_| Failure::new(FailureKind::Rejected, "Некорректная сессия rupload"))?;
        if u.origin() != self.upload_base.origin()
            || u.path() != format!("/ig-api-upload/{VERSION}/{container}")
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
        {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Instagram вернул неподтверждённый адрес загрузки",
            ));
        }
        Ok(u)
    }
    async fn send(
        &self,
        upload: &Upload,
        on: &mut (dyn FnMut(f64, String) + Send),
    ) -> Result<Receipt, Failure> {
        validate(upload, INSTAGRAM)?;
        if upload.duration < 3.0 {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Reels должен длиться не меньше 3 секунд",
            ));
        }
        if upload
            .scheduled_at
            .is_some_and(|at| at > crate::tiktok::api::now_unix())
        {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Instagram отправляет по календарю приложения",
            ));
        }
        let size = tokio::fs::metadata(&upload.path)
            .await
            .map_err(|_| Failure::new(FailureKind::Rejected, "Финальный файл не найден"))?
            .len();
        if size == 0 || size > INSTAGRAM.file_max.unwrap() {
            return Err(Failure::new(
                FailureKind::Rejected,
                "Файл Reels должен быть не больше 300 МБ",
            ));
        }
        self.identity().await?;
        let (used, total) = self.limit().await?;
        if used >= total {
            return Err(Failure::new(
                FailureKind::RetryAfter(3600),
                "Достигнут текущий лимит Instagram за 24 часа",
            ));
        }
        on(
            0.01,
            format!("Instagram: использовано {used} из {total} публикаций"),
        );
        let v = Self::response(self.http.post(self.url(&format!("{}/media", self.user))).bearer_auth(&self.token)
            .json(&json!({"media_type":"REELS","upload_type":"resumable","caption":upload.caption}))
            .send().await, false).await?;
        let container = v["id"].as_str().filter(|s| id(s)).ok_or_else(|| {
            Failure::new(FailureKind::Rejected, "Instagram не вернул ID контейнера")
        })?;
        let url = self.upload_url(
            v["uri"].as_str().ok_or_else(|| {
                Failure::new(FailureKind::Rejected, "Instagram не вернул адрес rupload")
            })?,
            container,
        )?;
        let file = tokio::fs::File::open(&upload.path)
            .await
            .map_err(|_| Failure::new(FailureKind::Rejected, "Не удалось открыть ролик"))?;
        // Документированный rupload принимает поток с offset/file_size. При обрыве не
        // угадываем позицию и не создаём второй контейнер: результат требует проверки.
        Self::response(
            self.http
                .post(url)
                .header("Authorization", format!("OAuth {}", self.token))
                .header("offset", "0")
                .header("file_size", size)
                .header("Content-Length", size)
                .header("Content-Type", "application/octet-stream")
                .body(reqwest::Body::from(file))
                .send()
                .await,
            true,
        )
        .await
        .and_then(|v| {
            if v["success"] == true {
                Ok(())
            } else {
                Err(Failure::new(
                    FailureKind::NeedsCheck,
                    "rupload не подтвердил загрузку. Проверь контейнер Instagram",
                ))
            }
        })?;
        on(0.7, "Instagram обрабатывает Reels".into());
        let mut ready = false;
        for attempt in 0..6 {
            if attempt > 0 {
                tokio::time::sleep(self.poll).await;
            }
            let status = Self::response(
                self.http
                    .get(self.fields(container, "status_code"))
                    .bearer_auth(&self.token)
                    .send()
                    .await,
                true,
            )
            .await?;
            match status["status_code"].as_str() {
                Some("FINISHED") => {
                    ready = true;
                    break;
                }
                Some("IN_PROGRESS") => {}
                Some("ERROR" | "EXPIRED") => {
                    return Err(Failure::new(
                        FailureKind::Rejected,
                        "Instagram не смог обработать контейнер Reels",
                    ))
                }
                _ => {
                    return Err(Failure::new(
                        FailureKind::NeedsCheck,
                        "Неизвестный статус контейнера Instagram",
                    ))
                }
            }
        }
        if !ready {
            return Err(Failure::new(
                FailureKind::NeedsCheck,
                "Instagram обрабатывает ролик больше 5 минут. Проверь аккаунт перед повтором",
            ));
        }
        on(0.9, "Публикую готовый контейнер Instagram".into());
        let v = Self::response(
            self.http
                .post(self.url(&format!("{}/media_publish", self.user)))
                .bearer_auth(&self.token)
                .json(&json!({"creation_id":container}))
                .send()
                .await,
            true,
        )
        .await?;
        let external = v["id"]
            .as_str()
            .filter(|s| id(s))
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::NeedsCheck,
                    "Instagram не подтвердил ID публикации. Проверь аккаунт",
                )
            })?
            .to_string();
        let permalink = Self::response(
            self.http
                .get(self.fields(&external, "permalink"))
                .bearer_auth(&self.token)
                .send()
                .await,
            false,
        )
        .await
        .ok()
        .and_then(|v| v["permalink"].as_str().map(str::to_string))
        .filter(|s| {
            Url::parse(s).is_ok_and(|u| {
                u.scheme() == "https"
                    && matches!(u.host_str(), Some("www.instagram.com" | "instagram.com"))
                    && u.username().is_empty()
                    && u.password().is_none()
            })
        });
        on(1.0, "Instagram подтвердил публикацию".into());
        Ok(Receipt {
            external_id: external,
            url: permalink,
            published: true,
        })
    }
}
impl Platform for Instagram {
    fn caps(&self) -> Caps {
        INSTAGRAM
    }
    fn publish<'a>(
        &'a self,
        upload: &'a Upload,
        on: &'a mut (dyn FnMut(f64, String) + Send),
    ) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(self.send(upload, on))
    }
}
