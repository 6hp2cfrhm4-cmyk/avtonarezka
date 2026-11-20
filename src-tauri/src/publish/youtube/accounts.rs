//! Google Desktop OAuth + PKCE. Секреты хранятся отдельными записями по аккаунтам.
use super::*;
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::publish::{oauth, schedule::Policy};
use crate::{
    drafts::{kv_get, kv_set},
    state::AppState,
};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn key(id: i64, field: &str) -> String {
    format!("platform:youtube:account:{id}:{field}")
}
const CLIENT_SECRET: &str = "platform:youtube:client_secret";
pub struct Google {
    http: reqwest::Client,
    token_url: reqwest::Url,
    api: reqwest::Url,
    pub authorize: reqwest::Url,
}
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_in: i64,
}
impl Google {
    pub fn new() -> Result<Self, Failure> {
        Ok(Self {
            http: http()?,
            token_url: reqwest::Url::parse("https://oauth2.googleapis.com/token").unwrap(),
            api: endpoint("https://www.googleapis.com/", "www.googleapis.com")?,
            authorize: reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap(),
        })
    }
    pub fn with_endpoint(base: &str) -> Result<Self, Failure> {
        let base = endpoint(base, "www.googleapis.com")?;
        if !local(&base) {
            return Self::new();
        }
        Ok(Self {
            http: http()?,
            token_url: base.join("token").unwrap(),
            api: base.clone(),
            authorize: base.join("authorize").unwrap(),
        })
    }
    pub async fn exchange(
        &self,
        client: &str,
        secret: &str,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> Result<Tokens, String> {
        self.token(&[
            ("client_id", client),
            ("client_secret", secret),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
        ])
        .await
    }
    pub async fn refresh(
        &self,
        client: &str,
        secret: &str,
        refresh: &str,
    ) -> Result<Tokens, String> {
        self.token(&[
            ("client_id", client),
            ("client_secret", secret),
            ("refresh_token", refresh),
            ("grant_type", "refresh_token"),
        ])
        .await
    }
    async fn token(&self, fields: &[(&str, &str)]) -> Result<Tokens, String> {
        let r = self
            .http
            .post(self.token_url.clone())
            .form(fields)
            .send()
            .await
            .map_err(|_| "Google не ответил при входе")?;
        if !r.status().is_success() {
            return Err(
                "Google отклонил вход. Проверь OAuth-клиент и подключи аккаунт заново".into(),
            );
        }
        let v = r
            .json::<Value>()
            .await
            .map_err(|_| "Не удалось прочитать ответ Google")?;
        if !v["token_type"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            return Err("Неподдерживаемый тип токена Google".into());
        }
        let access = v["access_token"]
            .as_str()
            .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
            .ok_or("Google не вернул токен доступа")?
            .to_string();
        Ok(Tokens {
            access,
            refresh: v["refresh_token"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            expires_in: v["expires_in"].as_i64().unwrap_or(3600).clamp(60, 86400),
        })
    }
    pub async fn channel(&self, access: &str) -> Result<(String, String), String> {
        let r = self
            .http
            .get(
                self.api
                    .join("youtube/v3/channels?part=id,snippet&mine=true")
                    .unwrap(),
            )
            .bearer_auth(access)
            .send()
            .await
            .map_err(|_| "Не удалось проверить канал YouTube")?;
        if !r.status().is_success() {
            return Err("Не удалось подтвердить канал YouTube. Проверь разрешения входа".into());
        }
        let v = r
            .json::<Value>()
            .await
            .map_err(|_| "Неверный ответ YouTube")?;
        let items = v["items"]
            .as_array()
            .filter(|a| a.len() == 1)
            .ok_or("У входа должен быть один канал YouTube. Выбери нужный канал при подключении")?;
        let id = items[0]["id"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 64
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            .ok_or("YouTube не вернул id канала")?;
        let label = items[0]["snippet"]["title"].as_str().unwrap_or("YouTube");
        Ok((id.into(), label.chars().take(60).collect()))
    }
}
pub fn challenge(verifier: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let digest = Sha256::digest(verifier.as_bytes());
    let mut out = String::new();
    for bytes in digest.chunks(3) {
        let n = (u32::from(bytes[0]) << 16)
            | (u32::from(*bytes.get(1).unwrap_or(&0)) << 8)
            | u32::from(*bytes.get(2).unwrap_or(&0));
        for i in 0..bytes.len() + 1 {
            out.push(ALPHABET[((n >> (18 - i * 6)) & 63) as usize] as char);
        }
    }
    out
}
pub fn authorization_url(
    base: &reqwest::Url,
    client: &str,
    redirect: &str,
    state: &str,
    verifier: &str,
) -> String {
    let mut u = base.clone();
    u.query_pairs_mut().extend_pairs([
        ("client_id", client),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", state),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge(verifier)),
    ]);
    u.into()
}
fn google() -> Result<Google, String> {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("AVTO_TEST_GOOGLE_ENDPOINT") {
        return Google::with_endpoint(&url).map_err(|e| e.message);
    }
    Google::new().map_err(|e| e.message)
}
pub(crate) async fn verify_channel(access: &str, expected: &str) -> Result<(), String> {
    let (id, _) = google()?.channel(access).await?;
    if id != expected {
        return Err(
            "Вход YouTube относится к другому каналу. Подключи нужный аккаунт заново".into(),
        );
    }
    Ok(())
}
pub(crate) fn settings(db: &Connection, id: i64) -> Settings {
    kv_get(db, &format!("youtube_settings:{id}"))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSettings {
    pub client_id: String,
    pub has_secret: bool,
    pub secret_backend: String,
    pub login_running: bool,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn youtube_oauth_settings(state: State<'_, AppState>) -> Result<OAuthSettings, String> {
    let client_id = kv_get(&state.db.0.lock().unwrap(), "youtube_client_id").unwrap_or_default();
    let has_secret = state
        .secrets
        .get(CLIENT_SECRET)
        .await
        .map_err(|_| "Не удалось прочитать связку ключей")?
        .is_some();
    Ok(OAuthSettings {
        client_id,
        has_secret,
        secret_backend: state.secrets.backend().into(),
        login_running: state.platform_login.lock().unwrap().is_some(),
    })
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn youtube_oauth_save(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: Option<String>,
) -> Result<(), String> {
    if state.platform_login.lock().unwrap().is_some() {
        return Err("Дождись окончания входа или отмени его".into());
    }
    let id = client_id.trim();
    if !id.ends_with(".apps.googleusercontent.com") || id.contains(char::is_whitespace) {
        return Err("Нужен Client ID приложения Google типа Desktop".into());
    }
    if let Some(s) = client_secret.filter(|s| !s.is_empty()) {
        state
            .secrets
            .set(CLIENT_SECRET, &s)
            .await
            .map_err(|_| "Не удалось сохранить секрет Google")?;
    }
    kv_set(&state.db.0.lock().unwrap(), "youtube_client_id", id).map_err(err)
}
async fn store_tokens(state: &AppState, id: i64, tokens: Tokens) -> Result<(), String> {
    state
        .secrets
        .set(&key(id, "access"), &tokens.access)
        .await
        .map_err(|_| "Не удалось сохранить токен YouTube")?;
    if let Some(r) = tokens.refresh {
        state
            .secrets
            .set(&key(id, "refresh"), &r)
            .await
            .map_err(|_| "Не удалось сохранить refresh token YouTube")?;
    }
    kv_set(
        &state.db.0.lock().unwrap(),
        &format!("youtube_expires:{id}"),
        &(crate::tiktok::api::now_unix() + tokens.expires_in).to_string(),
    )
    .map_err(err)
}
pub(crate) async fn has_secret(state: &AppState, id: i64) -> Result<bool, String> {
    state
        .secrets
        .get(&key(id, "refresh"))
        .await
        .map(|s| s.is_some())
        .map_err(|_| "Не удалось прочитать связку ключей".into())
}
pub(crate) async fn access(state: &AppState, id: i64) -> Result<String, String> {
    let expires = kv_get(
        &state.db.0.lock().unwrap(),
        &format!("youtube_expires:{id}"),
    )
    .and_then(|s| s.parse::<i64>().ok())
    .unwrap_or(0);
    if expires > crate::tiktok::api::now_unix() + 60 {
        if let Some(t) = state
            .secrets
            .get(&key(id, "access"))
            .await
            .map_err(|_| "Не удалось прочитать токен YouTube")?
        {
            return Ok(t);
        }
    }
    let client = kv_get(&state.db.0.lock().unwrap(), "youtube_client_id")
        .ok_or("Укажи OAuth-клиент Google")?;
    let secret = state
        .secrets
        .get(CLIENT_SECRET)
        .await
        .map_err(|_| "Не удалось прочитать секрет Google")?
        .unwrap_or_default();
    let refresh = state
        .secrets
        .get(&key(id, "refresh"))
        .await
        .map_err(|_| "Не удалось прочитать refresh token")?
        .ok_or("Подключи аккаунт YouTube заново")?;
    let tokens = google()?.refresh(&client, &secret, &refresh).await?;
    let access = tokens.access.clone();
    store_tokens(state, id, tokens).await?;
    Ok(access)
}
fn attach(db: &Connection, external: &str, label: &str) -> Result<i64, String> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let old: Option<i64> = tx
        .query_row(
            "SELECT id FROM accounts WHERE platform='youtube' AND external_ref=?1",
            [external],
            |r| r.get(0),
        )
        .ok();
    let id = if let Some(id) = old {
        id
    } else {
        tx.execute("INSERT INTO accounts(label,platform,external_ref,enabled,position,schedule_json)VALUES(?1,'youtube',?2,0,(SELECT COALESCE(MAX(position),0)+1 FROM accounts),?3)",params![label,external,serde_json::to_string(&Policy::default()).map_err(err)?]).map_err(err)?;
        tx.last_insert_rowid()
    };
    tx.commit().map_err(err)?;
    Ok(id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn youtube_login_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let client = kv_get(&state.db.0.lock().unwrap(), "youtube_client_id")
        .ok_or("Сохрани OAuth-клиент Google")?;
    let secret = state
        .secrets
        .get(CLIENT_SECRET)
        .await
        .map_err(|_| "Не удалось прочитать секрет Google")?
        .unwrap_or_default();
    let google = google()?;
    let listener = oauth::bind(0).await.map_err(err)?;
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr().map_err(err)?.port()
    );
    let expected = hex::encode(rand::random::<[u8; 32]>());
    let verifier = hex::encode(rand::random::<[u8; 32]>());
    let url = authorization_url(&google.authorize, &client, &redirect, &expected, &verifier);
    let (cancel, rx) = watch::channel(false);
    {
        let mut login = state.platform_login.lock().unwrap();
        if login.is_some() {
            return Err("Вход YouTube уже открыт".into());
        }
        *login = Some(cancel);
    }
    let start_url = url.clone();
    let task_app = app.clone();
    crate::host::spawn(async move {
        let result = async {
            let code = oauth::wait_for_code(listener, &expected, Duration::from_secs(300), rx)
                .await
                .map_err(err)?;
            let tokens = google
                .exchange(&client, &secret, &code, &redirect, &verifier)
                .await?;
            if tokens.refresh.is_none() {
                return Err(
                    "Google не выдал фоновый доступ. Подключи аккаунт заново и разреши доступ"
                        .into(),
                );
            }
            let (external, label) = google.channel(&tokens.access).await?;
            let state = task_app.state::<AppState>();
            let id = attach(&state.db.0.lock().unwrap(), &external, &label)?;
            store_tokens(&state, id, tokens).await?;
            state
                .db
                .0
                .lock()
                .unwrap()
                .execute(
                    "UPDATE accounts SET enabled=1 WHERE id=?1 AND platform='youtube'",
                    [id],
                )
                .map_err(err)?;
            Ok::<_, String>(label)
        }
        .await;
        task_app
            .state::<AppState>()
            .platform_login
            .lock()
            .unwrap()
            .take();
        let _=task_app.emit("platform-login",json!({"platform":"youtube","ok":result.is_ok(),"message":match result{Ok(label)=>format!("Подключён YouTube: {label}"),Err(e)=>e}}));
        crate::publish::calendar::changed(&task_app);
    });
    #[cfg(debug_assertions)]
    let open = std::env::var_os("AVTO_TEST_GOOGLE_ENDPOINT").is_none();
    #[cfg(not(debug_assertions))]
    let open = true;
    if open {
        if let Err(e) = oauth::open_browser(&url) {
            if let Some(c) = state.platform_login.lock().unwrap().as_ref() {
                let _ = c.send(true);
            }
            return Err(e.to_string());
        }
    }
    Ok(start_url)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn youtube_login_cancel(state: State<'_, AppState>) {
    if let Some(c) = state.platform_login.lock().unwrap().as_ref() {
        let _ = c.send(true);
    }
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn youtube_account_save(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    label: String,
    enabled: bool,
    policy: Policy,
    settings: Settings,
) -> Result<(), String> {
    settings.validate()?;
    policy.validate()?;
    if label.trim().is_empty() || label.chars().count() > 60 {
        return Err("Название аккаунта: от 1 до 60 знаков".into());
    }
    let db = state.db.0.lock().unwrap();
    let tx = db.unchecked_transaction().map_err(err)?;
    if tx.execute("UPDATE accounts SET label=?2,enabled=?3,schedule_json=?4 WHERE id=?1 AND platform='youtube'",params![id,label.trim(),enabled,serde_json::to_string(&policy).map_err(err)?]).map_err(err)?!=1{return Err("Аккаунт YouTube не найден".into());}
    kv_set(
        &tx,
        &format!("youtube_settings:{id}"),
        &serde_json::to_string(&settings).map_err(err)?,
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    drop(db);
    crate::publish::calendar::changed(&app);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_matches_rfc7636() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
    #[test]
    fn attach_is_idempotent_and_default_visibility_is_explicit_private() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let id = attach(&db, "UC_test", "Канал").unwrap();
        assert_eq!(id, attach(&db, "UC_test", "Другой").unwrap());
        assert_eq!(settings(&db, id).privacy, "private");
    }
}
