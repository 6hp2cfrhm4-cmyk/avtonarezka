//! Официальный API TikTok: вход (Login Kit, PKCE) и методы Content Posting API.
//! Сверено с developers.tiktok.com 2026-11-07. Отличия от привычного OAuth отмечены в местах, где они есть.

use std::{io::SeekFrom, path::Path, time::Duration};

use anyhow::{anyhow, bail, Context};
use bytes::Bytes;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub const SCOPE_UPLOAD: &str = "video.upload";
pub const SCOPE_PUBLISH: &str = "video.publish";

/// Лимит TikTok на файл — 4 ГБ; берём с запасом, чтобы не зависеть от того, десятичные это гигабайты или двоичные.
pub const MAX_VIDEO_BYTES: u64 = 4_000_000_000;
/// Размер куска. 10 000 000 — это больше 5 МБ и меньше 64 МБ при любом прочтении «МБ».
pub const CHUNK_SIZE: u64 = 10_000_000;
/// Файл до этого размера уходит одним куском.
pub const SINGLE_CHUNK_MAX: u64 = 50_000_000;

#[derive(Clone, Debug)]
pub struct Endpoints {
    /// Страница входа, только HTTPS.
    pub auth: String,
    pub api: String,
}

impl Endpoints {
    pub fn production() -> Self {
        Self { auth: "https://www.tiktok.com".into(), api: "https://open.tiktokapis.com".into() }
    }

    /// В debug-сборке адреса можно подменить (`AVTO_TIKTOK_AUTH`, `AVTO_TIKTOK_API`): так весь поток проверяется
    /// на локальном макете. В release-сборке подмена невозможна, иначе токены можно было бы увести на чужой адрес.
    pub fn from_env() -> Self {
        #[allow(unused_mut)]
        let mut e = Self::production();
        #[cfg(debug_assertions)]
        {
            if let Ok(v) = std::env::var("AVTO_TIKTOK_AUTH") {
                e.auth = v;
            }
            if let Ok(v) = std::env::var("AVTO_TIKTOK_API") {
                e.api = v;
            }
        }
        e
    }

    pub fn is_production(&self) -> bool {
        self.api.starts_with("https://")
    }
}

// ---------------------------------------------------------------- PKCE

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// У TikTok вызов считается как **hex** от SHA-256 (а не base64url, как в стандарте).
pub fn challenge_of(verifier: &str) -> String {
    hex::encode(Sha256::digest(verifier.as_bytes()))
}

/// Проверочная строка: 64 символа из разрешённого набора (допустимо 43–128).
pub fn new_pkce(rng: &mut impl Rng) -> Pkce {
    const SET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let verifier: String = (0..64).map(|_| SET[rng.random_range(0..SET.len())] as char).collect();
    let challenge = challenge_of(&verifier);
    Pkce { verifier, challenge }
}

pub fn random_state(rng: &mut impl Rng) -> String {
    let b: [u8; 16] = rng.random();
    hex::encode(b)
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback/")
}

pub fn authorize_url(ep: &Endpoints, client_key: &str, scope: &str, redirect_uri: &str, state: &str, challenge: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/v2/auth/authorize/", ep.auth.trim_end_matches('/')),
        [
            ("client_key", client_key),
            ("response_type", "code"),
            ("scope", scope),
            ("redirect_uri", redirect_uri),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
    )
    .context("неверный адрес входа")?;
    Ok(url.to_string())
}

// ---------------------------------------------------------------- ошибки

/// Ошибка, о которой TikTok сообщил сам. `code` — как у TikTok, `human` — понятное объяснение по-русски.
#[derive(Debug, thiserror::Error)]
#[error("{human}")]
pub struct ApiError {
    pub code: String,
    pub human: String,
    pub http: u16,
    pub log_id: Option<String>,
}

pub fn humanize(code: &str, message: &str) -> String {
    match code {
        "access_token_invalid" => "Вход в TikTok устарел или недействителен. Подключи аккаунт заново.".into(),
        "scope_not_authorized" => "У приложения нет нужного разрешения. Проверь, что в кабинете TikTok добавлены продукты Login Kit и Content Posting API, и подключи аккаунт заново.".into(),
        "spam_risk_too_many_posts" => "Достигнут дневной лимит публикаций TikTok. Попробуй завтра.".into(),
        "spam_risk_too_many_pending_share" => "В черновиках TikTok уже 5 неопубликованных роликов за сутки. Опубликуй или удали их в приложении TikTok.".into(),
        "spam_risk_user_banned_from_posting" => "TikTok запретил этому аккаунту публиковать.".into(),
        "unaudited_client_can_only_post_to_private_accounts" => {
            "Приложение не прошло аудит TikTok: публиковать можно только приватно («Только я»), и сам аккаунт должен быть приватным.".into()
        }
        "privacy_level_option_mismatch" => "Выбранная видимость недоступна для этого аккаунта.".into(),
        "reached_active_user_cap" => "Приложение достигло лимита активных пользователей TikTok.".into(),
        "url_ownership_unverified" => "Адрес файла не подтверждён в кабинете TikTok.".into(),
        "rate_limit_exceeded" => "TikTok просит подождать минуту: слишком много запросов.".into(),
        "internal_error" => "Ошибка на стороне TikTok. Попробуй позже.".into(),
        "invalid_publish_id" | "token_not_authorized_for_specified_publish_id" => "TikTok не узнал эту отправку.".into(),
        _ if message.is_empty() => format!("TikTok: {code}"),
        _ => format!("TikTok: {code} — {message}"),
    }
}

/// Тело ответа Content Posting API → данные или ошибка. Успех — `error.code == "ok"`.
pub fn parse_envelope(status: u16, body: &Value) -> Result<Value, ApiError> {
    let code = body["error"]["code"].as_str().unwrap_or("");
    if code == "ok" || (code.is_empty() && (200..300).contains(&status) && body.get("error").is_none()) {
        return Ok(body["data"].clone());
    }
    // OAuth-ошибки приходят в другой форме: {"error":"...","error_description":"..."}
    let (code, message) = match &body["error"] {
        Value::String(c) => (c.clone(), body["error_description"].as_str().unwrap_or("").to_string()),
        Value::Object(_) => (code.to_string(), body["error"]["message"].as_str().unwrap_or("").to_string()),
        _ => (format!("http_{status}"), String::new()),
    };
    Err(ApiError {
        human: humanize(&code, &message),
        code,
        http: status,
        log_id: body["error"]["log_id"].as_str().or_else(|| body["log_id"].as_str()).map(str::to_string),
    })
}

// ---------------------------------------------------------------- токены

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix-время, когда истекает access-токен.
    pub expires_at: i64,
    pub refresh_expires_at: i64,
    pub open_id: String,
    pub scope: String,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn parse_tokens(status: u16, v: &Value, now: i64) -> anyhow::Result<Tokens> {
    let Some(access) = v["access_token"].as_str().filter(|a| !a.is_empty()) else {
        return Err(parse_envelope(status, v).err().map(anyhow::Error::from).unwrap_or_else(|| anyhow!("TikTok не вернул токен доступа")));
    };
    Ok(Tokens {
        access_token: access.to_string(),
        refresh_token: v["refresh_token"].as_str().unwrap_or("").to_string(),
        expires_at: now + v["expires_in"].as_i64().unwrap_or(7200),
        refresh_expires_at: now + v["refresh_expires_in"].as_i64().unwrap_or(180 * 86_400),
        open_id: v["open_id"].as_str().unwrap_or("").to_string(),
        scope: v["scope"].as_str().unwrap_or("").to_string(),
    })
}

pub type SaveFuture = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;

/// Обновляет токен заранее: за 5 минут до конца. `save` вызывается сразу после обновления — старый
/// refresh-токен после этого перестаёт работать, и потерять новый значит потерять вход.
pub async fn ensure_fresh(
    api: &TikTok,
    client_key: &str,
    client_secret: &str,
    tokens: Tokens,
    now: i64,
    save: &mut (dyn FnMut(Tokens) -> SaveFuture + Send),
) -> anyhow::Result<Tokens> {
    if now + 300 < tokens.expires_at {
        return Ok(tokens);
    }
    if tokens.refresh_token.is_empty() || now >= tokens.refresh_expires_at {
        bail!("Срок входа в TikTok истёк. Подключи аккаунт заново.");
    }
    let fresh = api.refresh(client_key, client_secret, &tokens.refresh_token, now).await?;
    save(fresh.clone()).await.context("не удалось сохранить обновлённый вход")?;
    Ok(fresh)
}

// ---------------------------------------------------------------- куски файла

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkPlan {
    pub chunk_size: u64,
    pub total: u64,
}

/// Число кусков TikTok считает **округлением вниз**, остаток уходит в последний кусок.
pub fn plan_chunks(video_size: u64) -> anyhow::Result<ChunkPlan> {
    if video_size == 0 {
        bail!("Файл пустой");
    }
    if video_size > MAX_VIDEO_BYTES {
        bail!("Файл больше 4 ГБ, TikTok такой не принимает");
    }
    Ok(if video_size <= SINGLE_CHUNK_MAX {
        ChunkPlan { chunk_size: video_size, total: 1 }
    } else {
        ChunkPlan { chunk_size: CHUNK_SIZE, total: video_size / CHUNK_SIZE }
    })
}

/// Диапазоны байт (включительно) для каждого куска.
pub fn chunk_ranges(video_size: u64, plan: &ChunkPlan) -> Vec<(u64, u64)> {
    (0..plan.total)
        .map(|i| {
            let start = i * plan.chunk_size;
            let end = if i + 1 == plan.total { video_size - 1 } else { start + plan.chunk_size - 1 };
            (start, end)
        })
        .collect()
}

pub fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        _ => "video/mp4",
    }
}

/// Хэштеги в единый вид `#тег`: без пробелов внутри, без повторов, без пустых.
pub fn normalize_hashtags(tags: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.iter()
        .map(|t| t.trim().trim_start_matches('#').split_whitespace().collect::<String>())
        .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
        .map(|t| format!("#{t}"))
        .collect()
}

/// Подпись: текст и хэштеги через пробел, не длиннее 2200 «рун» TikTok (считаются по UTF-16).
pub fn make_title(caption: &str, hashtags: &[String]) -> String {
    let mut s = caption.trim().to_string();
    for t in hashtags {
        let t = t.trim().trim_start_matches('#');
        if !t.is_empty() {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push('#');
            s.push_str(t);
        }
    }
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > 2200 {
            break;
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------- клиент

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// В черновики TikTok (`video.upload`): подпись и публикация — в приложении TikTok.
    Inbox,
    /// Сразу публикация (`video.publish`).
    Direct,
}

impl Mode {
    pub fn scope(self) -> &'static str {
        match self {
            Mode::Inbox => SCOPE_UPLOAD,
            Mode::Direct => SCOPE_PUBLISH,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreatorInfo {
    pub nickname: String,
    pub privacy_options: Vec<String>,
    pub max_duration_sec: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PostState {
    Processing,
    /// Лежит в черновиках пользователя.
    InInbox,
    Complete(Vec<i64>),
    Failed(String),
}

pub fn fail_reason_human(reason: &str) -> String {
    match reason {
        "file_format_check_failed" => "TikTok не принял формат файла".into(),
        "duration_check_failed" => "Длительность ролика не подходит этому аккаунту".into(),
        "frame_rate_check_failed" => "Частота кадров не подходит TikTok".into(),
        "picture_size_check_failed" => "Размер кадра не подходит TikTok".into(),
        "publish_cancelled" => "Публикация отменена".into(),
        "auth_removed" => "Доступ приложения к аккаунту отозван".into(),
        r if r.starts_with("spam_risk") => "TikTok заподозрил спам и отклонил публикацию".into(),
        "internal" => "Внутренняя ошибка TikTok".into(),
        "" => "TikTok отклонил ролик без объяснения".into(),
        r => format!("TikTok отклонил ролик: {r}"),
    }
}

pub fn parse_status(data: &Value) -> PostState {
    match data["status"].as_str().unwrap_or("") {
        "PUBLISH_COMPLETE" => PostState::Complete(
            // в документации поле написано с опечаткой — принимаем оба написания
            data["publicaly_available_post_id"].as_array().or_else(|| data["publicly_available_post_id"].as_array()).map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default(),
        ),
        "SEND_TO_USER_INBOX" => PostState::InInbox,
        "FAILED" => PostState::Failed(fail_reason_human(data["fail_reason"].as_str().unwrap_or(""))),
        _ => PostState::Processing,
    }
}

pub struct Init {
    pub publish_id: String,
    pub upload_url: String,
}

#[derive(Clone)]
pub struct TikTok {
    http: reqwest::Client,
    pub ep: Endpoints,
}

pub fn cut(s: &str) -> String {
    s.chars().take(200).collect()
}

impl TikTok {
    pub fn new(ep: Endpoints) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("avtonarezka/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("не удалось создать HTTP-клиент")?;
        Ok(Self { http, ep })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.ep.api.trim_end_matches('/'))
    }

    async fn read(&self, resp: reqwest::Response) -> anyhow::Result<(u16, Value)> {
        let status = resp.status().as_u16();
        let text = resp.text().await.context("не удалось прочитать ответ TikTok")?;
        let v = serde_json::from_str(&text).map_err(|_| anyhow!("TikTok вернул не JSON (HTTP {status}): {}", cut(&text)))?;
        Ok((status, v))
    }

    async fn token_request(&self, form: &[(&str, &str)], now: i64) -> anyhow::Result<Tokens> {
        let resp = self.http.post(self.url("/v2/oauth/token/")).form(form).send().await.context("нет связи с TikTok")?;
        let (status, v) = self.read(resp).await?;
        parse_tokens(status, &v, now)
    }

    pub async fn exchange_code(&self, key: &str, secret: &str, code: &str, redirect_uri: &str, verifier: &str, now: i64) -> anyhow::Result<Tokens> {
        self.token_request(
            &[("client_key", key), ("client_secret", secret), ("code", code), ("grant_type", "authorization_code"), ("redirect_uri", redirect_uri), ("code_verifier", verifier)],
            now,
        )
        .await
    }

    pub async fn refresh(&self, key: &str, secret: &str, refresh_token: &str, now: i64) -> anyhow::Result<Tokens> {
        self.token_request(&[("client_key", key), ("client_secret", secret), ("grant_type", "refresh_token"), ("refresh_token", refresh_token)], now).await
    }

    async fn post_json(&self, path: &str, access: &str, body: Value) -> anyhow::Result<Value> {
        let resp = self.http.post(self.url(path)).bearer_auth(access).json(&body).send().await.context("нет связи с TikTok")?;
        let (status, v) = self.read(resp).await?;
        Ok(parse_envelope(status, &v)?)
    }

    pub async fn creator_info(&self, access: &str) -> anyhow::Result<CreatorInfo> {
        let d = self.post_json("/v2/post/publish/creator_info/query/", access, json!({})).await?;
        let options: Vec<String> = d["privacy_level_options"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        Ok(CreatorInfo {
            nickname: d["creator_nickname"].as_str().unwrap_or("").to_string(),
            // если TikTok не прислал варианты, безопасно предполагаем только приватную публикацию
            privacy_options: if options.is_empty() { vec!["SELF_ONLY".into()] } else { options },
            max_duration_sec: d["max_video_post_duration_sec"].as_f64(),
        })
    }

    pub async fn init_upload(&self, access: &str, mode: Mode, post_info: Option<Value>, video_size: u64, plan: &ChunkPlan) -> anyhow::Result<Init> {
        let source = json!({
            "source": "FILE_UPLOAD",
            "video_size": video_size,
            "chunk_size": plan.chunk_size,
            "total_chunk_count": plan.total,
        });
        let (path, body) = match mode {
            Mode::Inbox => ("/v2/post/publish/inbox/video/init/", json!({ "source_info": source })),
            Mode::Direct => ("/v2/post/publish/video/init/", json!({ "post_info": post_info.unwrap_or_else(|| json!({})), "source_info": source })),
        };
        let d = self.post_json(path, access, body).await?;
        let publish_id = d["publish_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("TikTok не вернул номер отправки"))?.to_string();
        let upload_url = d["upload_url"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("TikTok не вернул адрес для загрузки"))?.to_string();
        Ok(Init { publish_id, upload_url })
    }

    pub async fn status(&self, access: &str, publish_id: &str) -> anyhow::Result<PostState> {
        let d = self.post_json("/v2/post/publish/status/fetch/", access, json!({ "publish_id": publish_id })).await?;
        Ok(parse_status(&d))
    }

    /// Адрес загрузки приходит от TikTok. В боевом режиме принимаем только HTTPS.
    fn check_upload_url(&self, url: &str) -> anyhow::Result<()> {
        let u = reqwest::Url::parse(url).context("TikTok прислал неверный адрес загрузки")?;
        if self.ep.is_production() && u.scheme() != "https" {
            bail!("TikTok прислал небезопасный адрес загрузки");
        }
        Ok(())
    }

    /// Куски идут строго по порядку. Ошибки сервера (5xx) и обрывы связи повторяются до трёх раз.
    pub async fn upload_file(
        &self,
        upload_url: &str,
        path: &Path,
        size: u64,
        plan: &ChunkPlan,
        mime: &str,
        retry_delay: Duration,
        mut on_progress: impl FnMut(u64),
    ) -> anyhow::Result<()> {
        self.check_upload_url(upload_url)?;
        let mut file = tokio::fs::File::open(path).await.with_context(|| format!("не удалось открыть {}", path.display()))?;
        for (start, end) in chunk_ranges(size, plan) {
            let mut buf = vec![0u8; (end - start + 1) as usize];
            file.seek(SeekFrom::Start(start)).await?;
            file.read_exact(&mut buf).await.context("файл изменился во время загрузки")?;
            let body = Bytes::from(buf);

            let mut attempt = 0u32;
            loop {
                let sent = self
                    .http
                    .put(upload_url)
                    .header("Content-Type", mime)
                    .header("Content-Range", format!("bytes {start}-{end}/{size}"))
                    .timeout(Duration::from_secs(900))
                    .body(body.clone())
                    .send()
                    .await;
                match sent {
                    Ok(r) if r.status().is_success() => break,
                    Ok(r) if r.status().is_server_error() && attempt < 3 => {}
                    Ok(r) => bail!("{}", upload_error(r.status().as_u16())),
                    Err(e) if attempt < 3 && (e.is_timeout() || e.is_connect() || e.is_request()) => {}
                    Err(e) => return Err(anyhow!(e).context("связь с TikTok оборвалась при загрузке")),
                }
                attempt += 1;
                tokio::time::sleep(retry_delay * 2u32.pow(attempt - 1)).await;
            }
            on_progress(end + 1);
        }
        Ok(())
    }
}

fn upload_error(status: u16) -> String {
    match status {
        400 => "TikTok отклонил кусок файла (неверные заголовки или размер)".into(),
        403 => "Ссылка загрузки истекла (она живёт час). Начни отправку заново.".into(),
        404 => "TikTok не нашёл задачу загрузки. Начни отправку заново.".into(),
        416 => "TikTok получил кусок не в том порядке. Начни отправку заново.".into(),
        s => format!("TikTok ответил на загрузку кодом {s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_hex_sha256_not_base64() {
        // sha256("abc") — известный вектор
        assert_eq!(challenge_of("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn verifier_follows_the_documented_alphabet_and_length() {
        let mut rng = rand::rng();
        for _ in 0..100 {
            let p = new_pkce(&mut rng);
            assert!((43..=128).contains(&p.verifier.len()));
            assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
            assert_eq!(p.challenge, challenge_of(&p.verifier));
        }
        assert_ne!(new_pkce(&mut rng).verifier, new_pkce(&mut rng).verifier);
    }

    #[test]
    fn authorize_url_carries_every_required_parameter_encoded() {
        let url = authorize_url(&Endpoints::production(), "key 1", "video.upload", &redirect_uri(38475), "st", "ch").unwrap();
        let u = reqwest::Url::parse(&url).unwrap();
        assert_eq!(u.host_str(), Some("www.tiktok.com"));
        assert_eq!(u.path(), "/v2/auth/authorize/");
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["client_key"], "key 1");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["scope"], "video.upload");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:38475/callback/");
        assert_eq!((q["state"].as_str(), q["code_challenge"].as_str(), q["code_challenge_method"].as_str()), ("st", "ch", "S256"));
    }

    #[test]
    fn chunk_plan_matches_the_documented_example() {
        // из документации: 50 000 123 байт при куске 10 000 000 → 5 кусков, последний 10 000 123
        let size = 50_000_123;
        let plan = ChunkPlan { chunk_size: 10_000_000, total: size / 10_000_000 };
        assert_eq!(plan.total, 5);
        let r = chunk_ranges(size, &plan);
        assert_eq!(r[0], (0, 9_999_999));
        assert_eq!(*r.last().unwrap(), (40_000_000, 50_000_122));
    }

    #[test]
    fn chunk_plan_rules() {
        assert_eq!(plan_chunks(3_000_000).unwrap(), ChunkPlan { chunk_size: 3_000_000, total: 1 });
        assert_eq!(plan_chunks(50_000_000).unwrap().total, 1);
        let big = plan_chunks(55_000_123).unwrap();
        assert_eq!((big.chunk_size, big.total), (10_000_000, 5));
        assert!(plan_chunks(0).is_err());
        assert!(plan_chunks(MAX_VIDEO_BYTES + 1).is_err());
    }

    #[test]
    fn ranges_are_contiguous_and_cover_the_file_exactly() {
        for size in [1u64, 49_999_999, 50_000_001, 123_456_789, 999_999_999, MAX_VIDEO_BYTES] {
            let plan = plan_chunks(size).unwrap();
            let r = chunk_ranges(size, &plan);
            assert_eq!(r.len() as u64, plan.total);
            assert_eq!(r[0].0, 0);
            assert_eq!(r.last().unwrap().1, size - 1);
            assert!(r.windows(2).all(|w| w[0].1 + 1 == w[1].0), "{size}");
            // кроме последнего, все куски одинаковы и в пределах 5–64 МБ; последний не больше 128 МБ
            let n = r.len();
            for (i, (s, e)) in r.iter().enumerate() {
                let len = e - s + 1;
                if n > 1 && i + 1 < n {
                    assert!((5_242_880..=64_000_000).contains(&len), "{size}: {len}");
                }
                if i + 1 == n && n > 1 {
                    assert!(len <= 128_000_000, "{size}: {len}");
                }
            }
        }
    }

    #[test]
    fn title_joins_hashtags_and_counts_utf16_units() {
        assert_eq!(make_title(" Смотрите ", &["#юмор".into(), "кот".into(), "  ".into()]), "Смотрите #юмор #кот");
        assert_eq!(make_title("", &["a".into()]), "#a");
        // эмодзи занимает две единицы UTF-16
        let long = "😀".repeat(1500);
        let t = make_title(&long, &[]);
        assert_eq!(t.encode_utf16().count(), 2200);
        assert_eq!(t.chars().count(), 1100);
    }

    #[test]
    fn hashtags_are_normalised_deduplicated_and_idempotent() {
        let n = normalize_hashtags(&["#Юмор".into(), "юмор".into(), " ## кот ".into(), "два слова".into(), "".into(), "#".into()]);
        assert_eq!(n, vec!["#Юмор", "#кот", "#дваслова"]);
        assert_eq!(normalize_hashtags(&n), n, "повторная нормализация ничего не меняет");
    }

    #[test]
    fn envelope_distinguishes_ok_errors_and_oauth_errors() {
        let ok = json!({"data": {"publish_id": "x"}, "error": {"code": "ok", "message": "", "log_id": "l"}});
        assert_eq!(parse_envelope(200, &ok).unwrap()["publish_id"], "x");

        let err = json!({"data": {}, "error": {"code": "spam_risk_too_many_pending_share", "message": "m", "log_id": "L1"}});
        let e = parse_envelope(403, &err).unwrap_err();
        assert_eq!((e.code.as_str(), e.http, e.log_id.as_deref()), ("spam_risk_too_many_pending_share", 403, Some("L1")));
        assert!(e.human.contains("5 неопубликованных"));

        let oauth = json!({"error": "invalid_request", "error_description": "bad code", "log_id": "L2"});
        let e = parse_envelope(400, &oauth).unwrap_err();
        assert_eq!(e.code, "invalid_request");
        assert!(e.human.contains("bad code") && e.log_id.as_deref() == Some("L2"));

        let unknown = json!({"error": {"code": "brand_new_error", "message": ""}});
        assert_eq!(parse_envelope(500, &unknown).unwrap_err().human, "TikTok: brand_new_error");
    }

    #[test]
    fn tokens_parse_with_expiry_and_report_errors() {
        let v = json!({"access_token": "a", "refresh_token": "r", "expires_in": 7200, "refresh_expires_in": 100, "open_id": "o", "scope": "video.upload", "token_type": "Bearer"});
        let t = parse_tokens(200, &v, 1000).unwrap();
        assert_eq!((t.expires_at, t.refresh_expires_at, t.scope.as_str()), (8200, 1100, "video.upload"));
        let e = parse_tokens(400, &json!({"error": "invalid_grant", "error_description": "code expired"}), 0).unwrap_err();
        assert!(e.to_string().contains("code expired"), "{e}");
        assert!(parse_tokens(200, &json!({}), 0).is_err());
    }

    #[test]
    fn status_values_map_to_states() {
        assert_eq!(parse_status(&json!({"status": "PROCESSING_UPLOAD"})), PostState::Processing);
        assert_eq!(parse_status(&json!({"status": "SEND_TO_USER_INBOX"})), PostState::InInbox);
        assert_eq!(parse_status(&json!({"status": "PUBLISH_COMPLETE", "publicaly_available_post_id": [7, 8]})), PostState::Complete(vec![7, 8]));
        assert_eq!(parse_status(&json!({"status": "PUBLISH_COMPLETE", "publicly_available_post_id": [9]})), PostState::Complete(vec![9]));
        assert!(matches!(parse_status(&json!({"status": "FAILED", "fail_reason": "spam_risk_text"})), PostState::Failed(m) if m.contains("спам")));
        assert!(matches!(parse_status(&json!({"status": "FAILED"})), PostState::Failed(m) if m.contains("без объяснения")));
    }

    #[test]
    fn release_endpoints_cannot_be_redirected_to_plain_http() {
        let ep = Endpoints::production();
        assert!(ep.is_production());
        let api = TikTok::new(ep).unwrap();
        assert!(api.check_upload_url("https://upload.tiktok.example/x?y=1").is_ok());
        assert!(api.check_upload_url("http://evil.example/x").is_err());
        assert!(api.check_upload_url("не адрес").is_err());
    }
}
