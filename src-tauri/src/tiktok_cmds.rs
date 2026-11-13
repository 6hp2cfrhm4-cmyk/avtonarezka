//! Команды TikTok для интерфейса: настройки, вход, отправка, история отправок.

use std::{path::PathBuf, time::Duration};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

use crate::{
    drafts::{get as get_draft, kv_get, kv_set},
    media,
    state::AppState,
    tiktok::{
        api::{self, Endpoints, Mode, TikTok, Tokens},
        oauth,
        publish::{self, Outcome, PostRequest, Timing},
    },
};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

const K_KEY: &str = "tiktok_client_key";
const K_PORT: &str = "tiktok_port";
const K_MODE: &str = "tiktok_mode";
const K_METHOD: &str = "tiktok_method";
const K_PRIVACY: &str = "tiktok_privacy";
const K_AUTO: &str = "tiktok_auto_publish";
const K_ACCOUNT: &str = "tiktok_account";
const K_HAS_SECRET: &str = "tiktok_has_secret";
const S_SECRET: &str = "tiktok_client_secret";
const S_TOKENS: &str = "tiktok_tokens";

pub const DEFAULT_PORT: u16 = 38475;
const PRIVACY: [&str; 4] = ["SELF_ONLY", "MUTUAL_FOLLOW_FRIENDS", "FOLLOWER_OF_CREATOR", "PUBLIC_TO_EVERYONE"];

// ---------------------------------------------------------------- настройки

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub client_key: String,
    pub port: u16,
    /// `inbox` — в черновики TikTok, `direct` — сразу публикация.
    pub mode: String,
    /// `api` — официальный API, `browser` — через браузер.
    pub method: String,
    pub privacy: String,
    /// Через браузер: нажимать «Опубликовать» самому программе. По умолчанию нет: ролик заполняется, нажимает пользователь.
    pub auto_publish: bool,
}

pub(crate) fn load_settings(db: &Connection) -> Settings {
    Settings {
        client_key: kv_get(db, K_KEY).unwrap_or_default(),
        port: kv_get(db, K_PORT).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PORT),
        mode: kv_get(db, K_MODE).filter(|m| m == "direct").unwrap_or_else(|| "inbox".into()),
        method: kv_get(db, K_METHOD).filter(|m| m == "browser").unwrap_or_else(|| "api".into()),
        privacy: kv_get(db, K_PRIVACY).filter(|p| PRIVACY.contains(&p.as_str())).unwrap_or_else(|| "SELF_ONLY".into()),
        auto_publish: kv_get(db, K_AUTO).is_some_and(|v| v == "1"),
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SettingsArgs {
    pub client_key: String,
    /// `None` или пустая строка — оставить прежний.
    pub client_secret: Option<String>,
    pub port: u16,
    pub mode: String,
    pub method: String,
    pub privacy: String,
    pub auto_publish: bool,
}

pub(crate) fn validate(a: &SettingsArgs) -> Result<(), String> {
    let key = a.client_key.trim();
    if !key.is_empty() && !(4..=128).contains(&key.len()) || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Client key — это буквы и цифры из кабинета TikTok, без пробелов".into());
    }
    if let Some(s) = a.client_secret.as_deref().filter(|s| !s.is_empty()) {
        // секрет — латиница и цифры; байтовая длина у кириллицы вдвое больше, поэтому одной длины мало
        if !(8..=128).contains(&s.len()) || !s.chars().all(|c| c.is_ascii_graphic()) {
            return Err("Client secret выглядит неверно: проверь, что скопирован целиком, без пробелов".into());
        }
    }
    if a.port < 1024 {
        return Err("Порт должен быть от 1024 до 65535".into());
    }
    if !["inbox", "direct"].contains(&a.mode.as_str()) || !["api", "browser"].contains(&a.method.as_str()) || !PRIVACY.contains(&a.privacy.as_str()) {
        return Err("Неизвестное значение в настройках".into());
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Account {
    connected_at: i64,
    refresh_expires_at: i64,
    scope: String,
}

fn account(db: &Connection) -> Option<Account> {
    kv_get(db, K_ACCOUNT).and_then(|j| serde_json::from_str(&j).ok())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TikTokStatus {
    #[serde(flatten)]
    settings: Settings,
    has_secret: bool,
    redirect_uri: String,
    connected: bool,
    refresh_expires_at: Option<i64>,
    scope: String,
    secrets_backend: &'static str,
    chromium: bool,
    login_in_progress: bool,
}

fn status_of(state: &AppState) -> TikTokStatus {
    let db = state.db.0.lock().unwrap();
    let settings = load_settings(&db);
    let acc = account(&db).filter(|a| a.refresh_expires_at > api::now_unix());
    TikTokStatus {
        redirect_uri: api::redirect_uri(settings.port),
        has_secret: kv_get(&db, K_HAS_SECRET).is_some_and(|v| v == "1"),
        connected: acc.is_some(),
        refresh_expires_at: acc.as_ref().map(|a| a.refresh_expires_at),
        scope: acc.map(|a| a.scope).unwrap_or_default(),
        secrets_backend: state.secrets.backend(),
        chromium: crate::tiktok::browser::find_chromium().is_some(),
        login_in_progress: state.login.lock().unwrap().is_some(),
        settings,
    }
}

#[tauri::command]
pub fn tiktok_status(state: State<'_, AppState>) -> TikTokStatus {
    status_of(&state)
}

/// Забывает вход (токены и сведения об аккаунте). Ключи приложения остаются.
async fn forget_login(state: &AppState) {
    let _ = state.secrets.delete(S_TOKENS).await;
    let _ = state.db.0.lock().unwrap().execute("DELETE FROM kv WHERE key = ?1", [K_ACCOUNT]);
}

#[tauri::command]
pub async fn tiktok_save_settings(state: State<'_, AppState>, args: SettingsArgs) -> CmdResult<TikTokStatus> {
    validate(&args)?;
    let new_secret = args.client_secret.as_deref().filter(|s| !s.is_empty());
    // другой ключ, другой секрет или другой режим означают другие разрешения: прежний вход больше не годится
    let (relogin, _old) = {
        let db = state.db.0.lock().unwrap();
        let old = load_settings(&db);
        (old.client_key != args.client_key.trim() || old.mode != args.mode || new_secret.is_some(), old)
    };
    if let Some(secret) = new_secret {
        state.secrets.set(S_SECRET, secret).await.map_err(err)?;
    }
    {
        let db = state.db.0.lock().unwrap();
        for (k, v) in [(K_KEY, args.client_key.trim().to_string()), (K_PORT, args.port.to_string()), (K_MODE, args.mode), (K_METHOD, args.method), (K_PRIVACY, args.privacy), (K_AUTO, if args.auto_publish { "1" } else { "0" }.to_string())] {
            kv_set(&db, k, &v).map_err(err)?;
        }
        if new_secret.is_some() {
            kv_set(&db, K_HAS_SECRET, "1").map_err(err)?;
        }
    }
    if relogin {
        forget_login(&state).await;
    }
    Ok(status_of(&state))
}

#[tauri::command]
pub async fn tiktok_forget_secret(state: State<'_, AppState>) -> CmdResult<TikTokStatus> {
    state.secrets.delete(S_SECRET).await.map_err(err)?;
    forget_login(&state).await;
    let _ = state.db.0.lock().unwrap().execute("DELETE FROM kv WHERE key = ?1", [K_HAS_SECRET]);
    Ok(status_of(&state))
}

// ---------------------------------------------------------------- вход

#[derive(Serialize, Clone)]
struct LoginEvent {
    state: &'static str,
    message: String,
}

async fn save_tokens(state: &AppState, t: &Tokens) -> anyhow::Result<()> {
    state.secrets.set(S_TOKENS, &serde_json::to_string(t)?).await?;
    let acc = Account { connected_at: api::now_unix(), refresh_expires_at: t.refresh_expires_at, scope: t.scope.clone() };
    kv_set(&state.db.0.lock().unwrap(), K_ACCOUNT, &serde_json::to_string(&acc)?)?;
    Ok(())
}

#[tauri::command]
pub async fn tiktok_login_start(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    if state.login.lock().unwrap().is_some() {
        return Err("Вход уже идёт: заверши его в браузере или отмени".into());
    }
    let settings = load_settings(&state.db.0.lock().unwrap());
    if settings.client_key.is_empty() {
        return Err("Сначала укажи Client key приложения TikTok".into());
    }
    let secret = state.secrets.get(S_SECRET).await.map_err(err)?.filter(|s| !s.is_empty()).ok_or("Сначала укажи Client secret приложения TikTok")?;
    let mode = if settings.mode == "direct" { Mode::Direct } else { Mode::Inbox };

    // порт занимаем до открытия браузера: ответ не должен прийти раньше, чем мы готовы его принять
    let listener = oauth::bind(settings.port).await.map_err(err)?;
    let ep = Endpoints::from_env();
    let (pkce, st) = {
        let mut rng = rand::rng();
        (api::new_pkce(&mut rng), api::random_state(&mut rng))
    };
    let redirect = api::redirect_uri(settings.port);
    let url = api::authorize_url(&ep, &settings.client_key, mode.scope(), &redirect, &st, &pkce.challenge).map_err(err)?;

    let (tx, rx) = watch::channel(false);
    *state.login.lock().unwrap() = Some(tx);
    if let Err(e) = oauth::open_browser(&url) {
        *state.login.lock().unwrap() = None;
        return Err(err(e));
    }
    let _ = app.emit("tiktok-login", LoginEvent { state: "waiting", message: "Подтверди доступ в открывшемся браузере".into() });

    tauri::async_runtime::spawn(async move {
        let result = async {
            let code = oauth::wait_for_code(listener, &st, Duration::from_secs(300), rx).await?;
            let tokens = TikTok::new(ep)?.exchange_code(&settings.client_key, &secret, &code, &redirect, &pkce.verifier, api::now_unix()).await?;
            save_tokens(&app.state::<AppState>(), &tokens).await?;
            anyhow::Ok(())
        }
        .await;
        *app.state::<AppState>().login.lock().unwrap() = None;
        let _ = app.emit(
            "tiktok-login",
            match result {
                Ok(()) => LoginEvent { state: "done", message: "Аккаунт подключён".into() },
                Err(e) => LoginEvent { state: "error", message: err(e) },
            },
        );
    });
    Ok(())
}

#[tauri::command]
pub fn tiktok_login_cancel(state: State<'_, AppState>) {
    if let Some(tx) = state.login.lock().unwrap().as_ref() {
        let _ = tx.send(true);
    }
}

#[tauri::command]
pub async fn tiktok_logout(state: State<'_, AppState>) -> CmdResult<TikTokStatus> {
    forget_login(&state).await;
    Ok(status_of(&state))
}

// ---------------------------------------------------------------- журнал отправок

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PostDto {
    pub id: i64,
    pub draft_id: i64,
    pub method: String,
    pub mode: String,
    /// `uploading`, `done`, `failed`.
    pub status: String,
    pub detail: String,
    pub error: Option<String>,
    pub updated_at: i64,
}

pub(crate) fn begin_post(db: &Connection, draft_id: i64, method: &str, mode: &str) -> rusqlite::Result<i64> {
    db.execute("INSERT INTO posts (draft_id, method, mode, status) VALUES (?1, ?2, ?3, 'uploading')", params![draft_id, method, mode])?;
    Ok(db.last_insert_rowid())
}

/// Выкладка готового ролика из папки. Записи без черновика не попадают в список «последние выкладки» по проекту,
/// но учитываются в суточном лимите аккаунта (`sent_at`).
pub(crate) fn begin_ready_post(db: &Connection, ready_id: i64, method: &str, mode: &str) -> rusqlite::Result<i64> {
    db.execute("INSERT INTO posts (ready_id, method, mode, status) VALUES (?1, ?2, ?3, 'uploading')", params![ready_id, method, mode])?;
    Ok(db.last_insert_rowid())
}

pub(crate) fn finish_post(db: &Connection, id: i64, result: &Result<(String, Option<String>), String>) {
    let _ = match result {
        Ok((detail, publish_id)) => db.execute("UPDATE posts SET status = 'done', detail = ?2, publish_id = ?3, error = NULL, updated_at = unixepoch() WHERE id = ?1", params![id, detail, publish_id]),
        Err(e) => db.execute("UPDATE posts SET status = 'failed', error = ?2, updated_at = unixepoch() WHERE id = ?1", params![id, e]),
    };
}

/// Ролик ушёл с аккаунта: время выкладки и аккаунт попадают в суточный лимит. `None` — основной аккаунт.
pub(crate) fn mark_sent(db: &Connection, id: i64, account: Option<i64>) {
    let _ = db.execute("UPDATE posts SET sent_at = unixepoch(), account_id = COALESCE(?2, account_id) WHERE id = ?1", params![id, account]);
}

/// Последняя отправка каждого ролика проекта.
pub(crate) fn latest_posts(db: &Connection, project_id: i64) -> rusqlite::Result<Vec<PostDto>> {
    let mut st = db.prepare(
        "SELECT p.id, p.draft_id, p.method, p.mode, p.status, p.detail, p.error, p.updated_at
         FROM posts p JOIN drafts d ON d.id = p.draft_id JOIN moments m ON m.id = d.moment_id
         WHERE m.project_id = ?1 AND p.id = (SELECT MAX(id) FROM posts WHERE draft_id = p.draft_id)
         ORDER BY p.draft_id",
    )?;
    let rows = st.query_map([project_id], |r| {
        Ok(PostDto { id: r.get(0)?, draft_id: r.get(1)?, method: r.get(2)?, mode: r.get(3)?, status: r.get(4)?, detail: r.get(5)?, error: r.get(6)?, updated_at: r.get(7)? })
    })?;
    rows.collect()
}

/// После перезапуска приложения незавершённые отправки прерваны. Файл мог уйти и после обрыва, поэтому честно просим проверить.
pub(crate) fn recover_interrupted_posts(db: &Connection) {
    let _ = db.execute(
        "UPDATE posts SET status = 'failed', error = 'Отправка прервана: приложение было закрыто. Проверь, не появился ли ролик в TikTok, прежде чем отправлять ещё раз.' WHERE status = 'uploading'",
        [],
    );
}

#[tauri::command]
pub fn posts_list(state: State<'_, AppState>, project_id: i64) -> CmdResult<Vec<PostDto>> {
    latest_posts(&state.db.0.lock().unwrap(), project_id).map_err(err)
}

// ---------------------------------------------------------------- отправка

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PublishArgs {
    pub draft_id: i64,
    pub caption: String,
    pub hashtags: Vec<String>,
    pub privacy: String,
    pub allow_comment: bool,
    pub allow_duet: bool,
    pub allow_stitch: bool,
    /// Отправить ещё раз, хотя прошлая отправка была успешной.
    pub force: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishStart {
    pub(crate) post_id: Option<i64>,
    /// Ролик уже отправлялся: что именно вышло. Интерфейс спрашивает подтверждение и зовёт снова с `force`.
    pub(crate) already_sent: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostProgress {
    pub(crate) draft_id: i64,
    pub(crate) fraction: f64,
    pub(crate) message: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostDone {
    pub(crate) draft_id: i64,
    pub(crate) detail: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostFailed {
    pub(crate) draft_id: i64,
    pub(crate) message: String,
}

fn outcome_text(mode: Mode, o: &Outcome) -> String {
    match (mode, o.status.as_str()) {
        (Mode::Inbox, _) => "Ролик в черновиках TikTok. Открой приложение TikTok, вставь подпись и опубликуй.".into(),
        (Mode::Direct, "PUBLISH_COMPLETE") if !o.post_ids.is_empty() => format!("Опубликовано (id {})", o.post_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")),
        (Mode::Direct, _) => "Опубликовано".into(),
    }
}

#[tauri::command]
pub async fn tiktok_publish(app: AppHandle, state: State<'_, AppState>, args: PublishArgs) -> CmdResult<PublishStart> {
    let (file, settings, previous) = {
        let db = state.db.0.lock().unwrap();
        let d = get_draft(&db, args.draft_id)?;
        let file = PathBuf::from(d.final_path.filter(|_| d.status == "final_done").ok_or("Сначала нужен готовый финальный ролик")?);
        if !file.is_file() {
            return Err(format!("Файл не найден: {}", file.display()));
        }
        let prev = latest_posts(&db, d.project_id).map_err(err)?.into_iter().find(|p| p.draft_id == args.draft_id);
        (file, load_settings(&db), prev)
    };
    if !PRIVACY.contains(&args.privacy.as_str()) {
        return Err("Неизвестная видимость".into());
    }
    if state.posting.lock().unwrap().contains(&args.draft_id) {
        return Err("Этот ролик уже отправляется".into());
    }
    if let (Some(p), false) = (&previous, args.force) {
        if p.status == "done" {
            return Ok(already_sent(p));
        }
    }

    let mode = if settings.mode == "direct" { Mode::Direct } else { Mode::Inbox };
    if settings.method == "browser" {
        return crate::tiktok::browser::start(app, state, args, file, settings.auto_publish).await;
    }

    // заранее проверяем то, что можно проверить без сети: пользователь увидит причину сразу, а не событием
    if settings.client_key.is_empty() {
        return Err("Сначала подключи аккаунт TikTok в настройках".into());
    }
    let secret = state.secrets.get(S_SECRET).await.map_err(err)?.filter(|s| !s.is_empty()).ok_or("Сначала укажи Client secret приложения TikTok")?;
    let tokens: Tokens = serde_json::from_str(&state.secrets.get(S_TOKENS).await.map_err(err)?.ok_or("Сначала подключи аккаунт TikTok")?).map_err(|_| "Вход в TikTok повреждён. Подключи аккаунт заново.".to_string())?;

    let post_id = {
        let db = state.db.0.lock().unwrap();
        db.execute(
            "UPDATE moments SET caption = ?2, hashtags = ?3 WHERE id = (SELECT moment_id FROM drafts WHERE id = ?1)",
            params![args.draft_id, args.caption.trim(), serde_json::to_string(&api::normalize_hashtags(&args.hashtags)).unwrap_or_else(|_| "[]".into())],
        )
        .map_err(err)?;
        begin_post(&db, args.draft_id, "api", &settings.mode).map_err(err)?
    };
    state.posting.lock().unwrap().insert(args.draft_id);

    tauri::async_runtime::spawn(async move {
        let draft_id = args.draft_id;
        let result = run_api_post(&app, draft_id, mode, file, &settings.client_key, &secret, tokens, &args).await;
        let state = app.state::<AppState>();
        state.posting.lock().unwrap().remove(&draft_id);
        let stored = match &result {
            Ok(o) => Ok((outcome_text(mode, o), Some(o.publish_id.clone()))),
            Err(e) => Err(err(e)),
        };
        finish_post(&state.db.0.lock().unwrap(), post_id, &stored);
        let _ = match stored {
            Ok((detail, _)) => app.emit("post-done", PostDone { draft_id, detail }),
            Err(message) => app.emit("post-error", PostFailed { draft_id, message }),
        };
    });
    Ok(PublishStart { post_id: Some(post_id), already_sent: None })
}

fn already_sent(p: &PostDto) -> PublishStart {
    PublishStart { post_id: None, already_sent: Some(if p.detail.is_empty() { "Ролик уже отправлялся".into() } else { p.detail.clone() }) }
}

async fn run_api_post(app: &AppHandle, draft_id: i64, mode: Mode, file: PathBuf, key: &str, secret: &str, tokens: Tokens, args: &PublishArgs) -> anyhow::Result<Outcome> {
    let api = TikTok::new(Endpoints::from_env())?;
    let saver = app.clone();
    let tokens = api::ensure_fresh(&api, key, secret, tokens, api::now_unix(), &mut |t| {
        let app = saver.clone();
        Box::pin(async move { save_tokens(&app.state::<AppState>(), &t).await })
    })
    .await?;

    let duration_sec = media::probe(&file).await?.duration;
    let req = PostRequest {
        title: api::make_title(&args.caption, &args.hashtags),
        video: file,
        duration_sec,
        mode,
        privacy: args.privacy.clone(),
        allow_comment: args.allow_comment,
        allow_duet: args.allow_duet,
        allow_stitch: args.allow_stitch,
    };
    let reporter = app.clone();
    publish::publish(&api, &tokens.access_token, &req, &Timing::default(), &mut |p| {
        let _ = reporter.emit("post-progress", PostProgress { draft_id, fraction: p.fraction, message: p.message });
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> SettingsArgs {
        SettingsArgs { client_key: "aw1b2c3d4e5f".into(), client_secret: None, port: 38475, mode: "inbox".into(), method: "api".into(), privacy: "SELF_ONLY".into(), auto_publish: false }
    }

    #[test]
    fn valid_settings_pass_and_bad_ones_explain_themselves() {
        assert!(validate(&args()).is_ok());
        assert!(validate(&SettingsArgs { client_key: String::new(), ..args() }).is_ok(), "пустой ключ — просто не настроено");
        for (a, needle) in [
            (SettingsArgs { client_key: "ключ с пробелом".into(), ..args() }, "Client key"),
            (SettingsArgs { client_key: "ab".into(), ..args() }, "Client key"),
            (SettingsArgs { client_secret: Some("коротко".into()), ..args() }, "Client secret"),
            (SettingsArgs { client_secret: Some("secret with spaces here".into()), ..args() }, "Client secret"),
            (SettingsArgs { port: 80, ..args() }, "Порт"),
            (SettingsArgs { mode: "hack".into(), ..args() }, "Неизвестное"),
            (SettingsArgs { method: "x".into(), ..args() }, "Неизвестное"),
            (SettingsArgs { privacy: "EVERYONE".into(), ..args() }, "Неизвестное"),
        ] {
            let e = validate(&a).unwrap_err();
            assert!(e.contains(needle), "{a:?}: {e}");
        }
        assert!(validate(&SettingsArgs { client_secret: Some(String::new()), ..args() }).is_ok(), "пустой секрет — оставить прежний");
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute("INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (1, '/v', 'h', 'v', 100, 1, 1, 30, 1)", []).unwrap();
        c.execute("INSERT INTO projects (id, path, hash, title, duration, width, height, fps, has_audio) VALUES (2, '/w', 'h2', 'w', 100, 1, 1, 30, 1)", []).unwrap();
        for (mid, pid) in [(1, 1), (2, 1), (3, 2)] {
            c.execute("INSERT INTO moments (id, project_id, start, \"end\", status) VALUES (?1, ?2, 0, 30, 'accepted')", params![mid, pid]).unwrap();
            c.execute("INSERT INTO drafts (id, moment_id, part) VALUES (?1, ?1, ?1)", [mid]).unwrap();
        }
        c
    }

    #[test]
    fn defaults_are_the_safe_choices() {
        let s = load_settings(&db());
        assert_eq!((s.mode.as_str(), s.method.as_str(), s.privacy.as_str(), s.port, s.auto_publish), ("inbox", "api", "SELF_ONLY", DEFAULT_PORT, false));
        assert_eq!(s.client_key, "");
    }

    #[test]
    fn stored_settings_round_trip_and_garbage_falls_back_to_defaults() {
        let c = db();
        kv_set(&c, K_MODE, "direct").unwrap();
        kv_set(&c, K_METHOD, "browser").unwrap();
        kv_set(&c, K_PRIVACY, "PUBLIC_TO_EVERYONE").unwrap();
        kv_set(&c, K_PORT, "40001").unwrap();
        kv_set(&c, K_AUTO, "1").unwrap();
        let s = load_settings(&c);
        assert_eq!((s.mode.as_str(), s.method.as_str(), s.privacy.as_str(), s.port, s.auto_publish), ("direct", "browser", "PUBLIC_TO_EVERYONE", 40001, true));
        kv_set(&c, K_MODE, "что-то").unwrap();
        kv_set(&c, K_PRIVACY, "ALL").unwrap();
        kv_set(&c, K_PORT, "не число").unwrap();
        let s = load_settings(&c);
        assert_eq!((s.mode.as_str(), s.privacy.as_str(), s.port), ("inbox", "SELF_ONLY", DEFAULT_PORT));
    }

    #[test]
    fn post_lifecycle_keeps_the_latest_attempt_per_draft_and_scopes_by_project() {
        let c = db();
        let first = begin_post(&c, 1, "api", "inbox").unwrap();
        finish_post(&c, first, &Err("сеть".into()));
        let second = begin_post(&c, 1, "api", "inbox").unwrap();
        finish_post(&c, second, &Ok(("в черновиках".into(), Some("v_inbox~1".into()))));
        begin_post(&c, 2, "api", "direct").unwrap();
        begin_post(&c, 3, "api", "inbox").unwrap(); // другой проект

        let posts = latest_posts(&c, 1).unwrap();
        assert_eq!(posts.iter().map(|p| (p.draft_id, p.status.as_str())).collect::<Vec<_>>(), vec![(1, "done"), (2, "uploading")]);
        assert_eq!((posts[0].id, posts[0].detail.as_str(), posts[0].error.as_deref()), (second, "в черновиках", None));
        let p3 = latest_posts(&c, 2).unwrap();
        assert_eq!(p3.len(), 1);
        assert_eq!(p3[0].draft_id, 3);
    }

    #[test]
    fn failure_keeps_the_error_text_and_a_later_success_clears_it() {
        let c = db();
        let id = begin_post(&c, 1, "api", "inbox").unwrap();
        finish_post(&c, id, &Err("TikTok просит подождать".into()));
        let p = &latest_posts(&c, 1).unwrap()[0];
        assert_eq!((p.status.as_str(), p.error.as_deref()), ("failed", Some("TikTok просит подождать")));
        finish_post(&c, id, &Ok(("готово".into(), None)));
        let p = &latest_posts(&c, 1).unwrap()[0];
        assert_eq!((p.status.as_str(), p.error.as_deref()), ("done", None));
    }

    #[test]
    fn interrupted_uploads_are_marked_failed_with_a_warning_to_check_tiktok() {
        let c = db();
        let a = begin_post(&c, 1, "api", "inbox").unwrap();
        let b = begin_post(&c, 2, "api", "inbox").unwrap();
        finish_post(&c, b, &Ok(("ок".into(), None)));
        recover_interrupted_posts(&c);
        let posts = latest_posts(&c, 1).unwrap();
        let pa = posts.iter().find(|p| p.id == a).unwrap();
        assert_eq!(pa.status, "failed");
        assert!(pa.error.as_deref().unwrap().contains("Проверь, не появился ли"));
        assert_eq!(posts.iter().find(|p| p.id == b).unwrap().status, "done", "завершённые не трогаем");
    }

    #[test]
    fn deleting_a_draft_removes_its_posts() {
        let c = db();
        begin_post(&c, 1, "api", "inbox").unwrap();
        c.execute("DELETE FROM drafts WHERE id = 1", []).unwrap();
        assert_eq!(c.query_row::<i64, _, _>("SELECT count(*) FROM posts", [], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn outcome_texts_tell_the_user_what_to_do_next() {
        let o = |s: &str, ids: Vec<i64>| Outcome { publish_id: "p".into(), status: s.into(), post_ids: ids };
        assert!(outcome_text(Mode::Inbox, &o("SEND_TO_USER_INBOX", vec![])).contains("вставь подпись"));
        assert_eq!(outcome_text(Mode::Direct, &o("PUBLISH_COMPLETE", vec![5, 6])), "Опубликовано (id 5, 6)");
        assert_eq!(outcome_text(Mode::Direct, &o("PUBLISH_COMPLETE", vec![])), "Опубликовано");
    }
}
