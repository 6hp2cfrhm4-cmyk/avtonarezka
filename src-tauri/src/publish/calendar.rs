//! Публикации календаря используют publications и общий publisher, как очередь Студии.
use super::{
    schedule::{next_slot, Policy},
    FailureKind, Platform, PlatformId, Upload,
};
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::{
    caption,
    publish_queue::{Action, QueueSettings},
    state::AppState,
};
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn platform(s: &str) -> Result<PlatformId, String> {
    serde_json::from_value(serde_json::json!(s)).map_err(err)
}
fn now() -> i64 {
    crate::tiktok::api::now_unix()
}
pub(crate) fn changed(app: &AppHandle) {
    if let Err(e) = super::autofill::reconcile(&app.state::<AppState>().db.0.lock().unwrap()) {
        tracing::error!("Не удалось связать публикации со стадиями фабрики: {e}");
    }
    let _ = app.emit("publications-changed", ());
    let _ = app.emit("factory-changed", ());
}
fn event(db: &Connection, id: i64, stage: &str, message: &str) -> Result<(), String> {
    db.execute("INSERT INTO events(item_id,channel_id,kind,stage,message) SELECT f.id,f.channel_id,'publication',?2,?3 FROM publications j LEFT JOIN drafts d ON d.id=j.draft_id LEFT JOIN moments m ON m.id=d.moment_id LEFT JOIN ready_videos r ON r.id=j.ready_id LEFT JOIN factory_items f ON f.project_id=COALESCE(m.project_id,r.project_id) WHERE j.id=?1",params![id,stage,format!("Публикация #{id}: {message}")]).map_err(err)?;
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: i64,
    pub label: String,
    pub platform: String,
    pub external_ref: String,
    pub enabled: bool,
    pub policy: Policy,
    pub has_secret: bool,
    pub youtube_settings: Option<super::youtube::Settings>,
}
pub(crate) fn account_list(db: &Connection) -> Result<Vec<Account>, String> {
    let mut st=db.prepare("SELECT id,label,platform,external_ref,enabled,schedule_json FROM accounts ORDER BY position,id").map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok(Account {
                id: r.get(0)?,
                label: r.get(1)?,
                platform: r.get(2)?,
                external_ref: r.get(3)?,
                enabled: r.get(4)?,
                policy: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                has_secret: false,
                youtube_settings: None,
            })
        })
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}
fn secret_key(id: i64) -> String {
    format!("platform:telegram:account:{id}:bot_token")
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn publication_accounts(state: State<'_, AppState>) -> Result<Vec<Account>, String> {
    let mut list = account_list(&state.db.0.lock().unwrap())?;
    for a in &mut list {
        if a.platform == "telegram" {
            a.has_secret = state
                .secrets
                .get(&secret_key(a.id))
                .await
                .map_err(|_| "Не удалось прочитать связку ключей")?
                .is_some();
        } else if a.platform == "instagram" {
            a.has_secret = state
                .secrets
                .get(&super::instagram::accounts::key(a.id))
                .await
                .map_err(|_| "Не удалось прочитать токен Instagram")?
                .is_some();
        } else if a.platform == "vk" {
            a.has_secret = state
                .secrets
                .get(&crate::analytics::vk::key(a.id))
                .await
                .map_err(|_| "Не удалось прочитать токен VK")?
                .is_some();
        } else if a.platform == "youtube" {
            a.has_secret = super::youtube::accounts::has_secret(&state, a.id).await?;
            a.youtube_settings = Some(super::youtube::accounts::settings(
                &state.db.0.lock().unwrap(),
                a.id,
            ));
        }
    }
    Ok(list)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publication_account_policy(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    policy: Policy,
) -> Result<(), String> {
    policy.validate()?;
    let n = state
        .db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET schedule_json=?2 WHERE id=?1 AND platform='tiktok'",
            params![id, serde_json::to_string(&policy).map_err(err)?],
        )
        .map_err(err)?;
    if n != 1 {
        return Err("Аккаунт TikTok не найден".into());
    }
    changed(&app);
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramAccount {
    pub id: Option<i64>,
    pub label: String,
    pub chat: String,
    pub token: Option<String>,
    pub enabled: bool,
    pub policy: Policy,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramCheckArgs {
    pub id: Option<i64>,
    pub chat: String,
    pub token: Option<String>,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn telegram_account_check(
    state: State<'_, AppState>,
    args: TelegramCheckArgs,
) -> Result<super::telegram::ConnectionCheck, String> {
    if let Some(id) = args.id {
        let found = state
            .db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM accounts WHERE id=?1 AND platform='telegram'",
                [id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(err)?;
        if found != 1 {
            return Err("Аккаунт Telegram не найден".into());
        }
    }
    let token = if let Some(token) = args.token.filter(|s| !s.is_empty()) {
        token
    } else if let Some(id) = args.id {
        state
            .secrets
            .get(&secret_key(id))
            .await
            .map_err(|_| "Не удалось прочитать токен Telegram")?
            .ok_or("Введи токен бота для проверки")?
    } else {
        return Err("Введи токен бота для проверки".into());
    };
    #[cfg(debug_assertions)]
    let endpoint = std::env::var("AVTO_TEST_TELEGRAM_ENDPOINT")
        .unwrap_or_else(|_| "https://api.telegram.org".into());
    #[cfg(not(debug_assertions))]
    let endpoint = "https://api.telegram.org".into();
    super::telegram::Telegram::with_endpoint(token, args.chat, endpoint)
        .map_err(|e| e.message)?
        .check_connection()
        .await
        .map_err(|e| e.message)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn telegram_account_save(
    app: AppHandle,
    state: State<'_, AppState>,
    args: TelegramAccount,
) -> Result<i64, String> {
    args.policy.validate()?;
    let label = args.label.trim();
    let raw_chat = args.chat.trim();
    let canonical_chat = if raw_chat.starts_with('@') {
        raw_chat.to_ascii_lowercase()
    } else {
        raw_chat
            .parse::<i64>()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| raw_chat.to_string())
    };
    let chat = canonical_chat.as_str();
    if label.is_empty() || label.chars().count() > 60 {
        return Err("Название аккаунта: от 1 до 60 знаков".into());
    }
    let username = chat.strip_prefix('@').is_some_and(|s| {
        s.len() >= 5 && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    });
    let numeric = chat.parse::<i64>().is_ok_and(|n| n != 0);
    if !username && !numeric {
        return Err("Укажи @имя канала или числовой chat_id".into());
    }
    let new_token = args.token.as_deref().filter(|t| !t.is_empty());
    if let Some(t) = new_token {
        super::telegram::Telegram::new(t.into(), chat.into()).map_err(|e| e.message)?;
    }
    if args.id.is_none() && new_token.is_none() {
        return Err("Нужен токен бота Telegram".into());
    }
    // При ошибке keyring новая запись остаётся отключённой и не может отправлять.
    let id = {
        let db = state.db.0.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(err)?;
        if tx.query_row::<i64,_,_>("SELECT count(*) FROM accounts WHERE platform='telegram' AND external_ref=?1 AND id!=COALESCE(?2,-1)",params![chat,args.id],|r|r.get(0)).map_err(err)?>0{return Err("Этот канал Telegram уже добавлен".into());}
        let id = if let Some(id) = args.id {
            let exists: i64 = tx
                .query_row(
                    "SELECT count(*) FROM accounts WHERE id=?1 AND platform='telegram'",
                    [id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if exists == 0 {
                return Err("Аккаунт Telegram не найден".into());
            }
            let used:i64=tx.query_row("SELECT count(*) FROM publications WHERE target_account_id=?1 AND state!='cancelled'",[id],|r|r.get(0)).map_err(err)?;
            let previous: String = tx
                .query_row("SELECT external_ref FROM accounts WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .map_err(err)?;
            if used > 0 && previous != chat {
                return Err(
                    "У этого аккаунта уже есть публикации. Для другого канала создай новый аккаунт"
                        .into(),
                );
            }
            id
        } else {
            tx.execute("INSERT INTO accounts(label,platform,external_ref,enabled,position)VALUES(?1,'telegram',?2,0,(SELECT COALESCE(MAX(position),0)+1 FROM accounts))",params![label,chat]).map_err(err)?;
            tx.last_insert_rowid()
        };
        tx.commit().map_err(err)?;
        id
    };
    if let Some(t) = new_token {
        state
            .secrets
            .set(&secret_key(id), t)
            .await
            .map_err(|_| "Не удалось сохранить токен в связке ключей")?;
    }
    if args.enabled
        && state
            .secrets
            .get(&secret_key(id))
            .await
            .map_err(|_| "Не удалось прочитать токен")?
            .is_none()
    {
        return Err("Для включения нужен токен бота".into());
    }
    let db = state.db.0.lock().unwrap();
    db.execute(
        "UPDATE accounts SET label=?2,external_ref=?3,enabled=?4,schedule_json=?5 WHERE id=?1",
        params![
            id,
            label,
            chat,
            args.enabled,
            serde_json::to_string(&args.policy).map_err(err)?
        ],
    )
    .map_err(err)?;
    drop(db);
    changed(&app);
    Ok(id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HoldKind {
    Channel,
    Final,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Publication {
    pub id: i64,
    pub draft_id: Option<i64>,
    pub ready_id: Option<i64>,
    pub project_id: i64,
    pub title: String,
    pub platform: String,
    pub account_id: Option<i64>,
    pub account_label: String,
    pub state: String,
    pub not_before: i64,
    pub caption: String,
    pub external_id: Option<String>,
    pub url: Option<String>,
    pub error: Option<String>,
    pub legacy: bool,
    pub hold_reason: Option<String>,
    pub hold_kind: Option<HoldKind>,
}
fn final_hold(status: Option<&str>, path: Option<&str>) -> Option<String> {
    if status == Some("final_done") && path.is_some_and(|p| !p.trim().is_empty()) {
        return None;
    }
    Some(
        match status {
            Some("queued" | "rendering") => {
                "Черновик монтируется заново. После проверки одобри его и собери финал в Студии."
            }
            Some("ready") => "Черновик обновлён. Одобри его и собери финал в Студии.",
            Some("approved") => "Черновик одобрен. Собери финал в Студии.",
            Some("final_queued" | "final_rendering") => {
                "Финал монтируется. Отправка продолжится после готовности."
            }
            Some("failed" | "final_failed") => {
                "Монтаж не завершён. Исправь ошибку и повтори сборку в Студии."
            }
            Some("rejected") => "Черновик отклонён. Верни его на проверку или отмени публикацию.",
            _ => "Финальный ролик ещё не готов. Проверь его в Студии.",
        }
        .into(),
    )
}
pub(crate) fn list(db: &Connection) -> Result<Vec<Publication>, String> {
    let mut st=db.prepare("SELECT j.id,j.draft_id,j.ready_id,COALESCE(m.project_id,r.project_id),COALESCE(NULLIF(m.title,''),r.name,p.title),j.platform,COALESCE(j.target_account_id,j.wanted_account_id,j.account_id),COALESCE(a.label,'Ротация TikTok'),j.state,j.not_before,j.caption,j.external_id,j.url,j.error,j.legacy,d.status,d.final_path FROM publications j LEFT JOIN drafts d ON d.id=j.draft_id LEFT JOIN moments m ON m.id=d.moment_id LEFT JOIN ready_videos r ON r.id=j.ready_id JOIN projects p ON p.id=COALESCE(m.project_id,r.project_id) LEFT JOIN accounts a ON a.id=COALESCE(j.target_account_id,j.wanted_account_id,j.account_id) ORDER BY j.not_before,j.id").map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                Publication {
                    id: r.get(0)?,
                    draft_id: r.get(1)?,
                    ready_id: r.get(2)?,
                    project_id: r.get(3)?,
                    title: r.get(4)?,
                    platform: r.get(5)?,
                    account_id: r.get(6)?,
                    account_label: r.get(7)?,
                    state: r.get(8)?,
                    not_before: r.get(9)?,
                    caption: r.get(10)?,
                    external_id: r.get(11)?,
                    url: r.get(12)?,
                    error: r.get(13)?,
                    legacy: r.get(14)?,
                    hold_reason: None,
                    hold_kind: None,
                },
                r.get::<_, Option<String>>(15)?,
                r.get::<_, Option<String>>(16)?,
            ))
        })
        .map_err(err)?;
    let mut list = Vec::new();
    for row in rows {
        let (mut p, status, path) = row.map_err(err)?;
        if ["scheduled", "queued", "ready", "failed"].contains(&p.state.as_str()) {
            if let Some(reason) = crate::factory::rights::project_block(db, p.project_id)? {
                p.hold_kind = Some(HoldKind::Channel);
                p.hold_reason = Some(reason);
            } else if !p.legacy {
                p.hold_reason = final_hold(status.as_deref(), path.as_deref());
                if p.hold_reason.is_some() {
                    p.hold_kind = Some(HoldKind::Final);
                }
            }
        }
        list.push(p);
    }
    Ok(list)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarState {
    pub publications: Vec<Publication>,
    pub running: bool,
    pub note: Option<String>,
    pub secret_backend: String,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publications_list(state: State<'_, AppState>) -> Result<CalendarState, String> {
    let publications = list(&state.db.0.lock().unwrap())?;
    Ok(CalendarState {
        publications,
        running: state.publisher.lock().unwrap().is_some(),
        note: state.queue_note.lock().unwrap().clone(),
        secret_backend: state.secrets.backend().into(),
    })
}
pub(crate) fn reserved(
    db: &Connection,
    account: i64,
    exclude: Option<i64>,
) -> Result<Vec<i64>, String> {
    reservation_times(db, account, exclude, true)
}
/// Перед фактической отправкой лимиты расходуют только отправки и неопределённые
/// результаты. Будущие слоты учитываются при назначении/переносе, но не могут
/// вытеснять задержавшийся первый ролик бесконечно вперёд. Следующий ролик при
/// запуске проверит паузу уже от фактического времени предыдущего.
fn runtime_reserved(
    db: &Connection,
    account: i64,
    exclude: Option<i64>,
) -> Result<Vec<i64>, String> {
    reservation_times(db, account, exclude, false)
}
fn reservation_times(
    db: &Connection,
    account: i64,
    exclude: Option<i64>,
    planned: bool,
) -> Result<Vec<i64>, String> {
    let mut st=db.prepare("SELECT CASE WHEN p.state='done' THEN p.sent_at ELSE p.not_before END FROM publications p WHERE p.id!=COALESCE(?2,-1) AND p.state IN ('scheduled','queued','sending','done','needs_check') AND (?3 OR p.state NOT IN ('scheduled','queued')) AND COALESCE(p.target_account_id,p.account_id,p.wanted_account_id,CASE WHEN p.platform='tiktok' THEN (SELECT id FROM accounts WHERE platform='tiktok' ORDER BY position,id LIMIT 1) END)=?1 AND CASE WHEN p.state='done' THEN p.sent_at ELSE p.not_before END IS NOT NULL AND (p.state!='done' OR NOT EXISTS(SELECT 1 FROM publication_attempts h WHERE h.publication_id=p.id AND h.sent_at IS NOT NULL))").map_err(err)?;
    let rows = st
        .query_map(params![account, exclude, planned], |r| r.get(0))
        .map_err(err)?;
    let mut times = rows.collect::<Result<Vec<_>, _>>().map_err(err)?;
    // Ручные отправки и история старой очереди также расходуют лимит аккаунта.
    // У done без receipt (например, ручная отметка) время берётся из publications.
    // Если старый пользователь принудительно отправлял ролик повторно, считаем каждую
    // фактическую отправку из истории, а не только последнюю запись publications.
    let mut st=db.prepare("SELECT h.sent_at FROM publication_attempts h WHERE h.sent_at IS NOT NULL AND COALESCE(h.account_id,CASE WHEN h.platform='tiktok' THEN (SELECT id FROM accounts WHERE platform='tiktok' ORDER BY position,id LIMIT 1) END)=?1").map_err(err)?;
    times.extend(
        st.query_map([account], |r| r.get::<_, i64>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?,
    );
    Ok(times)
}
pub(crate) fn offset(at: i64) -> i64 {
    Local
        .timestamp_opt(at, 0)
        .single()
        .map_or(0, |t| i64::from(t.offset().local_minus_utc()))
}
#[derive(Serialize, Deserialize)]
struct YouTubeMetadata {
    title: String,
    settings: super::youtube::Settings,
}
fn youtube_metadata(
    db: &Connection,
    id: i64,
    draft: i64,
    account: i64,
) -> Result<YouTubeMetadata, String> {
    if let Some(raw) = crate::drafts::kv_get(db, &format!("publication_metadata:{id}")) {
        return serde_json::from_str(&raw)
            .map_err(|_| "Повреждены сохранённые параметры публикации".into());
    }
    Ok(YouTubeMetadata {
        title: caption::commands::load(db, draft, PlatformId::Youtube)?
            .fields
            .title,
        settings: super::youtube::accounts::settings(db, account),
    })
}
pub(crate) fn schedule(
    db: &Connection,
    draft: i64,
    account: i64,
    requested: Option<i64>,
    time: i64,
) -> Result<i64, String> {
    let tx = db.unchecked_transaction().map_err(err)?;
    crate::factory::rights::require_clip(&tx, draft)?;
    let a = account_list(&tx)?
        .into_iter()
        .find(|a| a.id == account && a.enabled)
        .ok_or("Выбери включённый аккаунт")?;
    if !["telegram", "youtube", "instagram", "tiktok"].contains(&a.platform.as_str()) {
        return Err("Площадка ещё не подключена к календарю".into());
    }
    if a.platform == "tiktok" {
        let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM publications WHERE legacy=1 AND draft_id=?1 AND state IN ('queued','sending','done','needs_check') AND (COALESCE(account_id,wanted_account_id) IS NULL OR COALESCE(account_id,wanted_account_id)=?2))",params![draft,account],|r|r.get(0)).map_err(err)?;
        if conflict {
            return Err(
                "Ролик уже учтён очередью TikTok в Студии. Проверь её перед назначением слота"
                    .into(),
            );
        }
    }
    let path: String = tx
        .query_row(
            "SELECT final_path FROM drafts WHERE id=?1 AND status='final_done'",
            [draft],
            |r| r.get(0),
        )
        .map_err(|_| "Нужен готовый финальный ролик")?;
    if !std::path::Path::new(&path).is_file() {
        return Err("Финальный файл не найден".into());
    }
    let existing:Option<i64>=tx.query_row("SELECT id FROM publications WHERE legacy=0 AND draft_id=?1 AND platform=?2 AND target_account_id=?3",params![draft,a.platform,account],|r|r.get(0)).ok();
    if let Some(id) = existing {
        let state: String = tx
            .query_row("SELECT state FROM publications WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .map_err(err)?;
        if !["scheduled", "failed", "cancelled"].contains(&state.as_str()) {
            return Err("Ролик уже отправляется, опубликован или ждёт проверки".into());
        }
    }
    let saved =
        caption::commands::load_for_account(&tx, draft, platform(&a.platform)?, Some(account))?;
    let text = caption::compose(&saved.spec, &saved.fields)?.text;
    let from = requested.unwrap_or(time).max(time);
    if requested.is_some_and(|t| t < time) {
        return Err("Время публикации уже прошло".into());
    }
    let slot = next_slot(
        &a.policy,
        from,
        offset(from),
        &reserved(&tx, account, existing)?,
    )?;
    if requested.is_some() && slot != from {
        return Err(format!(
            "Слот занят или вне лимитов. Ближайшее время: {}",
            Local
                .timestamp_opt(slot, 0)
                .single()
                .map(|t| t.format("%d.%m %H:%M").to_string())
                .unwrap_or_default()
        ));
    }
    let id = if let Some(id) = existing {
        tx.execute("UPDATE publications SET caption=?2,not_before=?3,state='scheduled',error=NULL,updated_at=unixepoch() WHERE id=?1",params![id,text,slot]).map_err(err)?;
        id
    } else {
        tx.execute("INSERT INTO publications(draft_id,platform,legacy,target_account_id,caption,state,not_before)VALUES(?1,?2,0,?3,?4,'scheduled',?5)",params![draft,a.platform,account,text,slot]).map_err(err)?;
        tx.last_insert_rowid()
    };
    if a.platform == "youtube" {
        let meta = YouTubeMetadata {
            title: saved.fields.title,
            settings: super::youtube::accounts::settings(&tx, account),
        };
        crate::drafts::kv_set(
            &tx,
            &format!("publication_metadata:{id}"),
            &serde_json::to_string(&meta).map_err(err)?,
        )
        .map_err(err)?;
    }
    super::output::freeze(&tx, id, platform(&a.platform)?)?;
    event(&tx, id, "scheduled", "назначен слот")?;
    tx.commit().map_err(err)?;
    Ok(id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publication_schedule(
    app: AppHandle,
    state: State<'_, AppState>,
    draft_id: i64,
    account_id: i64,
    at: Option<i64>,
) -> Result<i64, String> {
    let id = schedule(&state.db.0.lock().unwrap(), draft_id, account_id, at, now())?;
    changed(&app);
    Ok(id)
}
pub(crate) fn move_slot(db: &Connection, id: i64, at: i64, time: i64) -> Result<(), String> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let account:i64=tx.query_row("SELECT target_account_id FROM publications WHERE id=?1 AND legacy=0 AND state IN ('scheduled','failed')",[id],|r|r.get(0)).map_err(|_|"Эту публикацию нельзя перенести")?;
    let a = account_list(&tx)?
        .into_iter()
        .find(|a| a.id == account && a.enabled)
        .ok_or("Аккаунт отключён")?;
    if at < time {
        return Err("Время публикации уже прошло".into());
    }
    if next_slot(
        &a.policy,
        at,
        offset(at),
        &reserved(&tx, account, Some(id))?,
    )? != at
    {
        return Err("Слот занят или выходит за окно/лимит аккаунта".into());
    }
    tx.execute("UPDATE publications SET not_before=?2,state='scheduled',error=NULL,updated_at=unixepoch() WHERE id=?1",params![id,at]).map_err(err)?;
    event(&tx, id, "scheduled", "перенесён слот, содержание сохранено")?;
    tx.commit().map_err(err)?;
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publication_move(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    at: i64,
) -> Result<(), String> {
    move_slot(&state.db.0.lock().unwrap(), id, at, now())?;
    changed(&app);
    Ok(())
}
/// Явное обновление снимка подписи. Перенос слота по-прежнему сохраняет содержание.
/// Та же DB-блокировка, что у publisher, исключает изменение уже начатой отправки.
pub(crate) fn refresh_caption(db: &Connection, id: i64) -> Result<(), String> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let (draft, account, name, state): (i64, i64, String, String) = tx.query_row(
        "SELECT draft_id,target_account_id,platform,state FROM publications WHERE id=?1 AND legacy=0 AND state IN ('scheduled','failed')",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).map_err(|_| "Подпись можно обновить только до отправки. Проверь состояние публикации")?;
    let saved = caption::commands::load_for_account(&tx, draft, platform(&name)?, Some(account))?;
    let composed = caption::compose(&saved.spec, &saved.fields)?;
    if composed.body_trimmed {
        return Err(
            "Текст из редактора не помещается в лимит. Сократи его перед обновлением слота".into(),
        );
    }
    if name == "youtube" {
        let mut meta = youtube_metadata(&tx, id, draft, account)?;
        meta.title = saved.fields.title;
        crate::drafts::kv_set(
            &tx,
            &format!("publication_metadata:{id}"),
            &serde_json::to_string(&meta).map_err(err)?,
        )
        .map_err(err)?;
    }
    tx.execute(
        "UPDATE publications SET caption=?2,updated_at=unixepoch() WHERE id=?1",
        params![id, composed.text],
    )
    .map_err(err)?;
    event(
        &tx,
        id,
        &state,
        "подпись обновлена из редактора, слот сохранён",
    )?;
    tx.commit().map_err(err)?;
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publication_refresh_caption(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    refresh_caption(&state.db.0.lock().unwrap(), id)?;
    changed(&app);
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publication_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    let n=state.db.0.lock().unwrap().execute("UPDATE publications SET state='cancelled',updated_at=unixepoch() WHERE id=?1 AND legacy=0 AND state IN ('scheduled','failed')",[id]).map_err(err)?;
    if n == 0 {
        return Err("Публикация уже отправляется, завершена или требует проверки".into());
    }
    changed(&app);
    Ok(())
}
pub(crate) fn resolve(db: &Connection, id: i64, action: &str, time: i64) -> Result<(), String> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let account:i64=tx.query_row("SELECT target_account_id FROM publications WHERE id=?1 AND legacy=0 AND state='needs_check'",[id],|r|r.get(0)).map_err(|_|"Эта публикация не требует проверки")?;
    match action {
        "mark_done" => {
            tx.execute("UPDATE publications SET state='done',sent_at=?2,error=NULL,updated_at=unixepoch() WHERE id=?1",params![id,time]).map_err(err)?;
        }
        "retry" => {
            let a = account_list(&tx)?
                .into_iter()
                .find(|a| a.id == account && a.enabled)
                .ok_or("Аккаунт отключён")?;
            let at = next_slot(
                &a.policy,
                time,
                offset(time),
                &reserved(&tx, account, Some(id))?,
            )?;
            tx.execute("UPDATE publications SET state='scheduled',not_before=?2,error=NULL,updated_at=unixepoch() WHERE id=?1",params![id,at]).map_err(err)?;
        }
        _ => return Err("Неизвестный результат проверки".into()),
    }
    event(
        &tx,
        id,
        "checked",
        if action == "mark_done" {
            "пользователь подтвердил публикацию"
        } else {
            "пользователь проверил канал и назначил повтор"
        },
    )?;
    tx.commit().map_err(err)?;
    Ok(())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn publication_resolve(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    action: String,
) -> Result<(), String> {
    resolve(&state.db.0.lock().unwrap(), id, &action, now())?;
    changed(&app);
    Ok(())
}
pub(crate) fn next_due(db: &Connection, time: i64) -> Result<Action, String> {
    // Повторный монтаж оставляет назначенный слот. Его нельзя снова и снова
    // выбирать до готовности финала: send_snapshot вернёт None без ожидания.
    let mut st = db.prepare("SELECT p.id,p.target_account_id,p.not_before FROM publications p JOIN accounts a ON a.id=p.target_account_id JOIN drafts d ON d.id=p.draft_id WHERE p.legacy=0 AND p.state='scheduled' AND a.enabled=1 AND a.platform=p.platform AND p.platform IN ('telegram','youtube','instagram','tiktok') AND d.status='final_done' AND NULLIF(trim(d.final_path),'') IS NOT NULL AND NOT EXISTS(SELECT 1 FROM publications uncertain WHERE uncertain.state='needs_check' AND (uncertain.target_account_id=p.target_account_id OR (p.platform='tiktok' AND uncertain.legacy=1 AND (COALESCE(uncertain.account_id,uncertain.wanted_account_id) IS NULL OR COALESCE(uncertain.account_id,uncertain.wanted_account_id)=p.target_account_id)))) ORDER BY p.not_before,p.id").map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(err)?;
    for row in rows {
        let (job, account, at) = row.map_err(err)?;
        if crate::factory::rights::publication_block(db, job)?.is_some() {
            continue;
        }
        return Ok(if at <= time {
            Action::Send { job, account }
        } else {
            Action::WaitUntil(at)
        });
    }
    Ok(Action::Idle)
}

pub(crate) fn recover(db: &Connection) {
    let result = (|| -> Result<(), String> {
        let tx = db.unchecked_transaction().map_err(err)?;
        let ids: Vec<i64> = {
            let mut st = tx
                .prepare("SELECT id FROM publications WHERE legacy=0 AND state='sending'")
                .map_err(err)?;
            let rows = st.query_map([], |r| r.get(0)).map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };
        for id in ids {
            tx.execute("UPDATE publications SET state='needs_check',error='Отправка прервана. Проверь канал перед повтором',updated_at=unixepoch() WHERE id=?1",[id]).map_err(err)?;
            tx.execute("UPDATE publication_attempts SET status='failed',error='Отправка прервана. Требуется проверка канала',updated_at=unixepoch() WHERE publication_id=?1 AND status='uploading'",[id]).map_err(err)?;
            event(
                &tx,
                id,
                "needs_check",
                "отправка прервана, требуется проверка канала",
            )?;
        }
        tx.commit().map_err(err)?;
        Ok(())
    })();
    if let Err(e) = result {
        tracing::error!("Не удалось восстановить публикации: {e}");
    }
}

/// Подготовка выходного файла выполняется без блокировки БД. Перед сетевой отправкой
/// сравниваем содержание целиком: updated_at имеет секундную точность и для этого не годится.
#[derive(Debug, PartialEq)]
struct SendSnapshot {
    draft: i64,
    final_path: String,
    caption: String,
    external: String,
    platform: String,
    not_before: i64,
    output: Option<String>,
    default_output: Option<String>,
    metadata: Option<String>,
}
fn send_snapshot(db: &Connection, id: i64, account: i64) -> Result<Option<SendSnapshot>, String> {
    let row = db.query_row(
        "SELECT d.id,d.final_path,p.caption,a.external_ref,p.platform,p.not_before FROM publications p JOIN drafts d ON d.id=p.draft_id JOIN accounts a ON a.id=p.target_account_id WHERE p.id=?1 AND p.target_account_id=?2 AND p.platform=a.platform AND p.platform IN ('telegram','youtube','instagram','tiktok') AND p.legacy=0 AND p.state='scheduled' AND a.enabled=1 AND d.status='final_done'",
        params![id,account], |r| Ok(SendSnapshot {
            draft:r.get(0)?,final_path:r.get(1)?,caption:r.get(2)?,external:r.get(3)?,platform:r.get(4)?,not_before:r.get(5)?,
            output:None,default_output:None,metadata:None,
        }),
    ).optional().map_err(err)?;
    let Some(mut snapshot) = row else {
        return Ok(None);
    };
    snapshot.output = crate::drafts::kv_get(db, &format!("publication_output:{id}"));
    if snapshot.output.is_none() {
        snapshot.default_output =
            crate::drafts::kv_get(db, &format!("output_profile:{}", snapshot.platform));
    }
    if snapshot.platform == "youtube" {
        snapshot.metadata = Some(
            serde_json::to_string(&youtube_metadata(db, id, snapshot.draft, account)?)
                .map_err(err)?,
        );
    }
    Ok(Some(snapshot))
}
fn snapshot_current(
    db: &Connection,
    id: i64,
    account: i64,
    snapshot: &SendSnapshot,
) -> Result<bool, String> {
    Ok(send_snapshot(db, id, account)?.as_ref() == Some(snapshot))
}

pub(crate) async fn send(app: &AppHandle, id: i64, account: i64) -> Result<(), String> {
    let state = app.state::<AppState>();
    let snapshot = {
        let db = state.db.0.lock().unwrap();
        if crate::factory::rights::publication_block(&db, id)?.is_some() {
            return Ok(());
        }
        let Some(snapshot) = send_snapshot(&db, id, account)? else {
            return Ok(());
        };
        snapshot
    };
    let draft = snapshot.draft;
    let platform_name = snapshot.platform.clone();
    let external = snapshot.external.clone();
    // До отправки проверяем актуальные лимиты: сон ПК, ручная отправка и изменение
    // политики могли сделать сохранённый слот недействительным.
    {
        let db = state.db.0.lock().unwrap();
        if !snapshot_current(&db, id, account, &snapshot)? {
            return Ok(());
        }
        let Some(current) = account_list(&db)?
            .into_iter()
            .find(|a| a.id == account && a.enabled)
        else {
            return Ok(());
        };
        let at = now();
        if snapshot.not_before > at {
            return Ok(());
        }
        let next = next_slot(
            &current.policy,
            at,
            offset(at),
            &runtime_reserved(&db, account, Some(id))?,
        )?;
        if next > at {
            db.execute(
                "UPDATE publications SET not_before=?2 WHERE id=?1 AND state='scheduled'",
                params![id, next],
            )
            .map_err(err)?;
            drop(db);
            changed(app);
            return Ok(());
        }
    }
    if !state.posting.lock().unwrap().insert(draft) {
        return Err("Этот ролик уже отправляется другим способом".into());
    }
    struct Posting<'a>(&'a std::sync::Mutex<std::collections::HashSet<i64>>, i64);
    impl Drop for Posting<'_> {
        fn drop(&mut self) {
            self.0.lock().unwrap().remove(&self.1);
        }
    }
    let _posting = Posting(&state.posting, draft);
    let prepared = async {
        let (path, _) =
            super::output::prepare(app, draft, platform(&platform_name)?, Some(id)).await?;
        let info = crate::media::probe(&path)
            .await
            .map_err(|_| "Готовый файл не открывается как видео")?;
        let upload = Upload {
            path: path.into(),
            caption: snapshot.caption.clone(),
            duration: info.duration,
            scheduled_at: None,
        };
        let caps = if platform_name == "youtube" {
            super::YOUTUBE
        } else if platform_name == "instagram" {
            super::INSTAGRAM
        } else if platform_name == "tiktok" {
            super::TIKTOK
        } else {
            super::TELEGRAM
        };
        super::validate(&upload, caps).map_err(|e| e.message)?;
        let bytes = tokio::fs::metadata(&upload.path)
            .await
            .map_err(|_| "Файл не найден")?
            .len();
        if bytes == 0 || caps.file_max.is_some_and(|max| bytes > max) {
            return Err(format!("Размер файла превышает лимит {}", caps.name));
        }
        let connector: Box<dyn Platform> = if platform_name == "tiktok" {
            let profile = crate::accounts::profile_dir(
                &state.db.0.lock().unwrap(),
                &state.data_dir,
                Some(account),
            )?;
            Box::new(super::tiktok::TikTokBrowser {
                app: app.clone(),
                draft,
                profile,
            })
        } else if platform_name == "youtube" {
            if info.width > info.height {
                return Err("Для YouTube Shorts нужен вертикальный или квадратный ролик".into());
            }
            let access = super::youtube::accounts::access(&state, account).await?;
            super::youtube::accounts::verify_channel(&access, &external).await?;
            let YouTubeMetadata { title, settings } = serde_json::from_str(
                snapshot
                    .metadata
                    .as_deref()
                    .ok_or("Нет сохранённых параметров YouTube")?,
            )
            .map_err(err)?;
            #[cfg(debug_assertions)]
            let client = match std::env::var("AVTO_TEST_GOOGLE_ENDPOINT") {
                Ok(url) => super::youtube::YouTube::with_endpoint(access, title, settings, &url),
                Err(_) => super::youtube::YouTube::new(access, title, settings),
            }
            .map_err(|e| e.message)?;
            #[cfg(not(debug_assertions))]
            let client =
                super::youtube::YouTube::new(access, title, settings).map_err(|e| e.message)?;
            Box::new(client)
        } else if platform_name == "instagram" {
            if info.width > 1920 || !(23.0..=60.0).contains(&info.fps) {
                return Err("Reels: ширина до 1920 пикселей и частота 23–60 кадров/с".into());
            }
            let token = state
                .secrets
                .get(&super::instagram::accounts::key(account))
                .await
                .map_err(|_| "Не удалось прочитать токен Instagram")?
                .ok_or("Подключи токен Instagram")?;
            Box::new(super::instagram::accounts::connector(token, external)?)
        } else {
            let token = state
                .secrets
                .get(&secret_key(account))
                .await
                .map_err(|_| "Не удалось прочитать токен из связки ключей")?
                .ok_or("Подключи токен бота Telegram")?;
            #[cfg(debug_assertions)]
            let connector = match std::env::var("AVTO_TEST_TELEGRAM_ENDPOINT") {
                Ok(endpoint) => super::telegram::Telegram::with_endpoint(token, external, endpoint),
                Err(_) => super::telegram::Telegram::new(token, external),
            }
            .map_err(|e| e.message)?;
            #[cfg(not(debug_assertions))]
            let connector =
                super::telegram::Telegram::new(token, external).map_err(|e| e.message)?;
            Box::new(connector)
        };
        Ok::<_, String>((connector, upload))
    }
    .await;
    let (connector, upload) = match prepared {
        Ok(v) => v,
        Err(e) => {
            let db = state.db.0.lock().unwrap();
            let tx = db.unchecked_transaction().map_err(err)?;
            // Пользователь мог отменить/перенести слот либо исправить данные, пока
            // готовился файл. Ошибка старой подготовки не портит новое задание.
            if !snapshot_current(&tx, id, account, &snapshot)? {
                return Ok(());
            }
            if crate::factory::rights::publication_block(&tx, id)?.is_some() {
                return Ok(());
            }
            tx.execute("UPDATE publications SET state='failed',error=?2,updated_at=unixepoch() WHERE id=?1 AND state='scheduled'",params![id,e]).map_err(err)?;
            event(&tx, id, "needs_attention", &e)?;
            tx.commit().map_err(err)?;
            drop(db);
            changed(app);
            return Err(e);
        }
    };
    let attempt = {
        let db = state.db.0.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(err)?;
        // Слот остаётся редактируемым до sending. При изменении очередь заново
        // прочитает его вместе с новой подписью/датой, без попытки со старым текстом.
        if !snapshot_current(&tx, id, account, &snapshot)? {
            return Ok(());
        }
        if crate::factory::rights::publication_block(&tx, id)?.is_some() {
            return Ok(());
        }
        let Some(current) = account_list(&tx)?
            .into_iter()
            .find(|a| a.id == account && a.enabled)
        else {
            return Ok(());
        };
        let at = now();
        let next = next_slot(
            &current.policy,
            at,
            offset(at),
            &runtime_reserved(&tx, account, Some(id))?,
        )?;
        if next > at {
            tx.execute(
                "UPDATE publications SET not_before=?2 WHERE id=?1 AND state='scheduled'",
                params![id, next],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            drop(db);
            changed(app);
            return Ok(());
        }
        let n=tx.execute("UPDATE publications SET state='sending',attempts=attempts+1,account_id=?2,error=NULL,updated_at=unixepoch() WHERE id=?1 AND state='scheduled'",params![id,account]).map_err(err)?;
        if n != 1 {
            return Err("Эта публикация уже взята в работу".into());
        }
        tx.execute("INSERT INTO publication_attempts(draft_id,ready_id,publication_id,platform,account_id,method,mode,status) SELECT draft_id,ready_id,id,platform,?2,?3,'direct','uploading' FROM publications WHERE id=?1",params![id,account,connector.method()]).map_err(err)?;
        let attempt = tx.last_insert_rowid();
        event(
            &tx,
            id,
            "publishing",
            &format!("начата отправка: {platform_name}"),
        )?;
        tx.commit().map_err(err)?;
        attempt
    };
    changed(app);
    let outcome = connector
        .publish(&upload, &mut |_, message| {
            let _ = app.emit(
                "publication-progress",
                serde_json::json!({"id":id,"message":message}),
            );
        })
        .await;
    let result = {
        let db = state.db.0.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(err)?;
        let result = match outcome {
            Ok(receipt) if receipt.published => {
                let external = (!receipt.external_id.is_empty()).then_some(receipt.external_id);
                tx.execute("UPDATE publications SET state='done',external_id=?2,url=?3,sent_at=?4,error=NULL,updated_at=unixepoch() WHERE id=?1 AND state='sending'",params![id,external,receipt.url,now()]).map_err(err)?;
                tx.execute("UPDATE publication_attempts SET status='done',detail='Площадка подтвердила загрузку',publish_id=?2,sent_at=?3,updated_at=unixepoch() WHERE id=?1",params![attempt,external,now()]).map_err(err)?;
                Ok(())
            }
            other => {
                let failure = other.err().unwrap_or_else(|| {
                    super::Failure::new(
                        FailureKind::NeedsCheck,
                        "Площадка не подтвердила публикацию. Проверь канал",
                    )
                });
                let (status, retry) = match failure.kind {
                    FailureKind::RetryAfter(seconds) => ("scheduled", Some(now() + seconds as i64)),
                    FailureKind::NeedsCheck => ("needs_check", None),
                    _ => ("failed", None),
                };
                tx.execute("UPDATE publications SET state=?2,error=?3,not_before=COALESCE(?4,not_before),updated_at=unixepoch() WHERE id=?1 AND state='sending'",params![id,status,failure.message,retry]).map_err(err)?;
                tx.execute("UPDATE publication_attempts SET status='failed',error=?2,updated_at=unixepoch() WHERE id=?1",params![attempt,failure.message]).map_err(err)?;
                if retry.is_some() {
                    Ok(())
                } else {
                    Err(failure.message)
                }
            }
        };
        event(
            &tx,
            id,
            if result.is_ok() {
                "publication_updated"
            } else {
                "needs_attention"
            },
            if result.is_ok() {
                "сохранён результат отправки"
            } else {
                "отправка требует внимания"
            },
        )?;
        tx.commit().map_err(err)?;
        result
    };
    changed(app);
    result
}

// Тип прежних настроек остаётся в одном месте; новые аккаунты хранят ту же политику.
impl From<QueueSettings> for Policy {
    fn from(s: QueueSettings) -> Self {
        Self {
            pause_min: s.pause_min,
            daily_limit: s.daily_limit,
            window_start: s.window_start,
            window_end: s.window_end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestFile(std::path::PathBuf);
    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn fixture() -> (Connection, TestFile) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::db::migrate(&db).unwrap();
        let file = TestFile(std::env::temp_dir().join(format!(
                "avto-calendar-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::write(&file.0, b"fixture").unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'v','h','Запись',60,640,360,25,1); INSERT INTO moments(id,project_id,start,\"end\",title,transcript)VALUES(1,1,0,30,'Клип','Содержательная речь'),(2,1,30,60,'Второй','Ещё речь'); INSERT INTO drafts(id,moment_id,status)VALUES(1,1,'final_done'),(2,2,'final_done'); INSERT INTO channels(id,name,credit)VALUES(1,'Источник','Автор'); INSERT INTO factory_items(position,kind,source,project_id,channel_id)VALUES(1,'file','v',1,1); INSERT INTO accounts(id,label,platform,external_ref)VALUES(2,'Первый','telegram','@first'),(3,'Второй','telegram','@second');").unwrap();
        db.execute(
            "UPDATE drafts SET final_path=?1",
            [file.0.to_str().unwrap()],
        )
        .unwrap();
        (db, file)
    }
    fn tiktok_caption(db: &Connection, draft: i64) {
        let saved = caption::commands::Saved {
            ad_locked: false,
            spec: caption::CaptionSpec {
                platform: PlatformId::Tiktok,
                template: caption::DEFAULT_TEMPLATE.into(),
            },
            fields: caption::Fields {
                title: "Момент".into(),
                body: "Содержательный рассказ о событии. ".repeat(180),
                ..Default::default()
            },
        };
        crate::drafts::kv_set(
            db,
            &format!("caption_draft:{draft}:tiktok"),
            &serde_json::to_string(&saved).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn tiktok_calendar_reuses_accounts_freezes_caption_and_guards_both_queues() {
        let (db, _file) = fixture();
        tiktok_caption(&db, 1);
        tiktok_caption(&db, 2);
        let first = schedule(&db, 1, 1, None, 100).unwrap();
        assert_eq!(list(&db).unwrap()[0].caption.encode_utf16().count(), 3999);
        assert!(list(&db).unwrap()[0].caption.contains("Источник: Автор"));
        assert_eq!(schedule(&db, 1, 1, None, 100).unwrap(), first);
        assert_eq!(
            crate::publish_queue::start_jobs(&db, 1).unwrap_err(),
            "Нет готовых задач: назначь текст или добавь ролики без текста"
        );
        crate::publish_queue::apply_text(&db, 1, &"т".repeat(3984), &["тест".into()]).unwrap();
        assert!(crate::publish_queue::start_jobs(&db, 1)
            .unwrap_err()
            .contains("Календаре TikTok"));
        db.execute(
            "UPDATE publications SET state='cancelled' WHERE legacy=0",
            [],
        )
        .unwrap();
        crate::publish_queue::start_jobs(&db, 1).unwrap();
        assert!(schedule(&db, 1, 1, None, 100)
            .unwrap_err()
            .contains("Студии"));
        db.execute("UPDATE publications SET state=CASE draft_id WHEN 1 THEN 'needs_check' ELSE 'cancelled' END WHERE legacy=1",[]).unwrap();
        let second = schedule(&db, 2, 1, None, 100).unwrap();
        assert_eq!(
            next_due(&db, 10000).unwrap(),
            Action::Idle,
            "неопределённый результат старой очереди блокирует аккаунт календаря"
        );
        db.execute(
            "UPDATE publications SET state='cancelled' WHERE legacy=1",
            [],
        )
        .unwrap();
        assert_eq!(
            next_due(&db, 10000).unwrap(),
            Action::Send {
                job: second,
                account: 1
            }
        );
        db.execute(
            "UPDATE publications SET state='sending' WHERE id=?1",
            [second],
        )
        .unwrap();
        recover(&db);
        assert_eq!(
            list(&db)
                .unwrap()
                .iter()
                .find(|p| p.id == second)
                .unwrap()
                .state,
            "needs_check"
        );
        assert_eq!(next_due(&db, 10000).unwrap(), Action::Idle);
    }
    #[test]
    fn calendar_reserves_manual_history_without_double_counting_linked_receipts() {
        let (db, _file) = fixture();
        db.execute_batch("INSERT INTO publication_attempts(draft_id,platform,method,mode,status,sent_at)VALUES(1,'tiktok','browser','browser','done',9000)").unwrap();
        assert_eq!(reserved(&db, 1, None).unwrap(), vec![9000]);
        db.execute_batch("INSERT INTO publications(draft_id,state,sent_at)VALUES(1,'done',9000); UPDATE publication_attempts SET publication_id=last_insert_rowid() WHERE id=1;").unwrap();
        assert_eq!(
            reserved(&db, 1, None).unwrap(),
            vec![9000],
            "прежний основной аккаунт без account_id не теряется и не считается дважды"
        );
        tiktok_caption(&db, 2);
        let id = schedule(&db, 2, 1, None, 9001).unwrap();
        assert_eq!(
            list(&db)
                .unwrap()
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .not_before,
            11700,
            "пауза от ручной публикации"
        );
        db.execute(
            "UPDATE publications SET state='done',sent_at=11700 WHERE id=?1",
            [id],
        )
        .unwrap();
        db.execute("INSERT INTO publication_attempts(draft_id,publication_id,platform,account_id,method,mode,status,sent_at)VALUES(2,?1,'tiktok',1,'browser','direct','done',11700)",[id]).unwrap();
        let mut times = reserved(&db, 1, None).unwrap();
        times.sort();
        assert_eq!(times, vec![9000, 11700]);
        assert!(
            reserved(&db, 2, None).unwrap().is_empty(),
            "история другого аккаунта изолирована"
        );
    }
    #[test]
    fn delayed_dispatch_is_not_displaced_by_future_slots_and_next_waits_for_actual_send() {
        for platform in ["telegram", "youtube", "instagram", "tiktok"] {
            let (db, _file) = fixture();
            let policy = Policy {
                pause_min: 1,
                daily_limit: 2,
                window_start: 0,
                window_end: 0,
            };
            db.execute(
                "UPDATE accounts SET platform=?1,schedule_json=?2 WHERE id=2",
                params![platform, serde_json::to_string(&policy).unwrap()],
            )
            .unwrap();
            let first = schedule(&db, 1, 2, Some(10000), 100).unwrap();
            let second = schedule(&db, 2, 2, Some(10065), 100).unwrap();
            assert!(
                move_slot(&db, second, 10050, 100).is_err(),
                "calendar still protects planned slots"
            );
            let late = 10020;
            assert_eq!(
                next_due(&db, late).unwrap(),
                Action::Send {
                    job: first,
                    account: 2
                }
            );
            assert!(
                next_slot(&policy, late, 0, &reserved(&db, 2, Some(first)).unwrap()).unwrap()
                    > late,
                "the original dispatch guard would postpone this due job"
            );
            assert_eq!(
                next_slot(
                    &policy,
                    late,
                    0,
                    &runtime_reserved(&db, 2, Some(first)).unwrap()
                )
                .unwrap(),
                late,
                "{platform}: an already due job keeps its turn after slow preparation"
            );
            db.execute(
                "UPDATE publications SET state='done',sent_at=?2 WHERE id=?1",
                params![first, late],
            )
            .unwrap();
            db.execute("INSERT INTO publication_attempts(draft_id,publication_id,platform,account_id,method,mode,status,sent_at)VALUES(1,?1,?2,2,'api','direct','done',?3)",params![first,platform,late]).unwrap();
            assert_eq!(
                runtime_reserved(&db, 2, Some(second)).unwrap(),
                vec![late],
                "linked receipt counted once"
            );
            assert_eq!(
                next_slot(
                    &policy,
                    10065,
                    0,
                    &runtime_reserved(&db, 2, Some(second)).unwrap()
                )
                .unwrap(),
                10080,
                "{platform}: pause is measured from the actual previous send"
            );
            assert_eq!(
                next_slot(
                    &policy,
                    10080,
                    0,
                    &runtime_reserved(&db, 2, Some(second)).unwrap()
                )
                .unwrap(),
                10080
            );
            db.execute(
                "UPDATE publications SET state='done',sent_at=10080 WHERE id=?1",
                [second],
            )
            .unwrap();
            assert_eq!(
                next_slot(&policy, 10081, 0, &runtime_reserved(&db, 2, None).unwrap()).unwrap(),
                86400,
                "actual sends still exhaust the daily limit"
            );
        }
    }
    #[test]
    fn rerendered_scheduled_clip_waits_without_starving_other_ready_clips() {
        for platform in ["telegram", "youtube", "instagram", "tiktok"] {
            let (db, _file) = fixture();
            db.execute(
                "UPDATE accounts SET platform=?1 WHERE id IN (2,3)",
                [platform],
            )
            .unwrap();
            let waiting = schedule(&db, 1, 2, Some(10000), 100).unwrap();
            let ready = schedule(&db, 2, 3, Some(10010), 100).unwrap();
            for status in [
                "queued",
                "rendering",
                "ready",
                "approved",
                "final_queued",
                "final_rendering",
                "failed",
                "final_failed",
                "rejected",
            ] {
                db.execute("UPDATE drafts SET status=?1 WHERE id=1", [status])
                    .unwrap();
                let rows = list(&db).unwrap();
                let held = rows.iter().find(|p| p.id == waiting).unwrap();
                assert!(
                    matches!(held.hold_kind, Some(HoldKind::Final)) && held.hold_reason.is_some()
                );
                assert_eq!(
                    next_due(&db, 10005).unwrap(),
                    Action::WaitUntil(10010),
                    "{platform}/{status}: wait for the next ready clip"
                );
                assert_eq!(
                    next_due(&db, 10020).unwrap(),
                    Action::Send {
                        job: ready,
                        account: 3
                    },
                    "{platform}/{status}: an unfinished clip must not monopolize the publisher"
                );
            }
            db.execute(
                "UPDATE publications SET state='done',sent_at=10020 WHERE id=?1",
                [ready],
            )
            .unwrap();
            assert_eq!(next_due(&db, 10020).unwrap(), Action::Idle);
            db.execute("UPDATE drafts SET status='final_done' WHERE id=1", [])
                .unwrap();
            for path in [None, Some(""), Some("   ")] {
                db.execute_batch("SAVEPOINT missing_final").unwrap();
                db.execute("UPDATE drafts SET final_path=?1 WHERE id=1", [path])
                    .unwrap();
                assert_eq!(next_due(&db, 10020).unwrap(), Action::Idle);
                assert!(list(&db)
                    .unwrap()
                    .iter()
                    .find(|p| p.id == waiting)
                    .unwrap()
                    .hold_reason
                    .is_some());
                db.execute_batch("ROLLBACK TO missing_final; RELEASE missing_final")
                    .unwrap();
            }
            assert_eq!(
                next_due(&db, 10020).unwrap(),
                Action::Send {
                    job: waiting,
                    account: 2
                }
            );
            assert!(list(&db)
                .unwrap()
                .iter()
                .all(|p| p.hold_reason.is_none() && p.hold_kind.is_none()));
            assert_eq!(
                db.query_row(
                    "SELECT state,not_before,attempts FROM publications WHERE id=?1",
                    [waiting],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?
                    ))
                )
                .unwrap(),
                ("scheduled".into(), 10000, 0)
            );
        }
    }
    #[test]
    fn caption_and_slot_are_atomic_per_clip_account_and_reserve_future_capacity() {
        let (db, _file) = fixture();
        let a = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let same = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        assert_eq!(a, same);
        let b = schedule(&db, 2, 2, None, 10000).unwrap();
        assert_ne!(a, b);
        let rows = list(&db).unwrap();
        assert_eq!(rows.iter().find(|p| p.id == b).unwrap().not_before, 12700);
        assert!(rows[0].caption.contains("Источник: Автор"));
        assert!(move_slot(&db, b, 10010, 100).is_err());
        assert_eq!(
            list(&db)
                .unwrap()
                .iter()
                .find(|p| p.id == b)
                .unwrap()
                .not_before,
            12700
        );
        let other = schedule(&db, 1, 3, Some(10000), 100).unwrap();
        assert_ne!(other, a);
        db.execute(
            "UPDATE publications SET state='done',external_id='published' WHERE id=?1",
            [a],
        )
        .unwrap();
        assert!(schedule(&db, 1, 2, None, 200).is_err());
        assert!(db.execute("INSERT INTO publications(draft_id,platform,legacy,target_account_id)VALUES(1,'telegram',0,2)",[]).is_err());
        assert_eq!(crate::accounts::enabled_ids(&db).unwrap(), vec![1]);
        assert!(crate::accounts::list(&db, 0)
            .unwrap()
            .iter()
            .all(|a| a.id == 1));
        assert!(!db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
    #[test]
    fn recovery_does_not_send_uncertain_results_and_legacy_queue_cannot_pick_telegram() {
        let (db, _file) = fixture();
        let a = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let b = schedule(&db, 2, 3, Some(10000), 100).unwrap();
        crate::publish_queue::apply_text(&db, 1, "Текст", &["тест".into()]).unwrap();
        crate::publish_queue::start_jobs(&db, 1).unwrap();
        assert_eq!(
            list(&db)
                .unwrap()
                .iter()
                .filter(|p| !p.legacy && p.state == "scheduled")
                .count(),
            2
        );
        db.execute("UPDATE publications SET state='sending' WHERE id=?1", [a])
            .unwrap();
        recover(&db);
        crate::publish_queue::recover_interrupted_jobs(&db);
        assert!(matches!(next_due(&db,10000).unwrap(),Action::Send{job,..}if job==b));
        assert!(schedule(&db, 1, 2, None, 100).is_err());
        db.execute("UPDATE accounts SET enabled=0 WHERE id=3", [])
            .unwrap();
        assert_eq!(next_due(&db, 10000).unwrap(), Action::Idle);
        assert!(crate::publish_queue::update_job(&db, a, "Изменить", &["тест".into()]).is_err());
    }
    #[test]
    fn every_platform_recovers_uncertain_send_and_receipt_conflict_without_replay() {
        for platform in ["telegram", "youtube", "instagram", "tiktok"] {
            let (db, _file) = fixture();
            db.execute("UPDATE accounts SET platform=?1 WHERE id=2", [platform])
                .unwrap();
            db.execute("INSERT INTO publications(draft_id,platform,legacy,target_account_id,state,external_id,sent_at)VALUES(1,?1,0,2,'done','receipt-1',100)", [platform]).unwrap();
            db.execute("INSERT INTO publications(draft_id,platform,legacy,target_account_id,state,not_before)VALUES(2,?1,0,2,'sending',100)", [platform]).unwrap();
            let id = db.last_insert_rowid();
            db.execute("INSERT INTO publication_attempts(draft_id,publication_id,platform,account_id,method,mode,status)VALUES(2,?1,?2,2,'api','direct','uploading')", params![id,platform]).unwrap();
            // Внешний ответ уже мог прийти; уникальность запрещает сохранить чужой receipt.
            {
                let tx = db.unchecked_transaction().unwrap();
                assert!(tx
                    .execute(
                        "UPDATE publications SET state='done',external_id='receipt-1' WHERE id=?1",
                        [id]
                    )
                    .is_err());
            }
            recover(&db);
            recover(&db);
            let row: (String, Option<String>, i64) = db
                .query_row(
                    "SELECT state,external_id,attempts FROM publications WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(row, ("needs_check".into(), None, 0), "{platform}");
            assert_eq!(next_due(&db, 10000).unwrap(), Action::Idle, "{platform}");
            assert_eq!(
                db.query_row(
                    "SELECT status FROM publication_attempts WHERE publication_id=?1",
                    [id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "failed"
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM publication_attempts WHERE publication_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
            resolve(&db, id, "mark_done", 101).unwrap();
            assert_eq!(next_due(&db, 10000).unwrap(), Action::Idle);
            assert!(
                resolve(&db, id, "retry", 102).is_err(),
                "подтверждённый ролик не повторяется: {platform}"
            );
            assert!(db
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none());
        }
    }
    #[test]
    fn prepared_send_is_invalidated_by_edits_cancellation_and_account_changes_on_every_platform() {
        for platform in ["telegram", "youtube", "instagram", "tiktok"] {
            let (db, _file) = fixture();
            db.execute("UPDATE accounts SET platform=?1 WHERE id=2", [platform])
                .unwrap();
            let id = schedule(&db, 1, 2, Some(10000), 100).unwrap();
            let snapshot = send_snapshot(&db, id, 2).unwrap().unwrap();
            assert!(snapshot_current(&db, id, 2, &snapshot).unwrap());
            // Никакой зависимости от секундной точности updated_at.
            for change in [
                "UPDATE publications SET caption='Новое описание' WHERE id=?1",
                "UPDATE publications SET not_before=20000 WHERE id=?1",
                "UPDATE publications SET state='cancelled' WHERE id=?1",
                "UPDATE publications SET state='sending' WHERE id=?1",
                "UPDATE publications SET target_account_id=3 WHERE id=?1",
                "UPDATE accounts SET enabled=0 WHERE id=2 AND ?1>0",
                "UPDATE accounts SET external_ref='other-channel' WHERE id=2 AND ?1>0",
                "UPDATE drafts SET final_path='new-final.mp4' WHERE id=1 AND ?1>0",
                "UPDATE drafts SET status='drafting' WHERE id=1 AND ?1>0",
            ] {
                db.execute_batch("SAVEPOINT edit_during_prepare").unwrap();
                db.execute(change, [id]).unwrap();
                assert!(
                    !snapshot_current(&db, id, 2, &snapshot).unwrap(),
                    "{platform}: {change}"
                );
                db.execute_batch("ROLLBACK TO edit_during_prepare; RELEASE edit_during_prepare")
                    .unwrap();
            }
            db.execute_batch("SAVEPOINT output_edit").unwrap();
            crate::drafts::kv_set(&db, &format!("publication_output:{id}"), "changed output")
                .unwrap();
            assert!(!snapshot_current(&db, id, 2, &snapshot).unwrap());
            db.execute_batch("ROLLBACK TO output_edit; RELEASE output_edit")
                .unwrap();
            assert!(snapshot_current(&db, id, 2, &snapshot).unwrap());
            assert_eq!(
                db.query_row("SELECT attempts FROM publications WHERE id=?1", [id], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM publication_attempts", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn prepared_youtube_metadata_and_explicit_caption_refresh_invalidate_the_old_send() {
        let (db, _file) = fixture();
        db.execute("UPDATE accounts SET platform='youtube' WHERE id=2", [])
            .unwrap();
        let id = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let snapshot = send_snapshot(&db, id, 2).unwrap().unwrap();
        let mut saved =
            caption::commands::load_for_account(&db, 1, PlatformId::Youtube, Some(2)).unwrap();
        saved.fields.title = "Исправленный заголовок".into();
        caption::commands::save_for_account(&db, 1, saved, Some(2)).unwrap();
        // Редактор сам по себе не меняет замороженную публикацию.
        assert!(snapshot_current(&db, id, 2, &snapshot).unwrap());
        refresh_caption(&db, id).unwrap();
        assert!(!snapshot_current(&db, id, 2, &snapshot).unwrap());
        let current = send_snapshot(&db, id, 2).unwrap().unwrap();
        assert_eq!(current.caption, snapshot.caption);
        assert_ne!(current.metadata, snapshot.metadata);
        crate::drafts::kv_set(&db, &format!("publication_metadata:{id}"), "broken").unwrap();
        assert!(snapshot_current(&db, id, 2, &current).is_err());
        assert_eq!(list(&db).unwrap()[0].state, "scheduled");
    }
    #[test]
    fn refreshing_caption_uses_account_text_and_preserves_slot_output_and_failed_state() {
        let (db, _file) = fixture();
        let first = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let second = schedule(&db, 1, 3, Some(10000), 100).unwrap();
        let output = crate::drafts::kv_get(&db, &format!("publication_output:{first}"));
        let old_second = list(&db)
            .unwrap()
            .into_iter()
            .find(|p| p.id == second)
            .unwrap()
            .caption;
        let mut saved =
            caption::commands::load_for_account(&db, 1, PlatformId::Telegram, Some(2)).unwrap();
        saved.fields.body = "Мой отдельный текст".into();
        saved.fields.hashtags = vec!["#прогулка".into()];
        caption::commands::save_for_account(&db, 1, saved, Some(2)).unwrap();
        db.execute("UPDATE publications SET state='failed',error='Ошибка авторизации',attempts=2 WHERE id=?1",[first]).unwrap();
        refresh_caption(&db, first).unwrap();
        let row: (String, i64, String, String, i64) = db
            .query_row(
                "SELECT caption,not_before,state,error,attempts FROM publications WHERE id=?1",
                [first],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "Мой отдельный текст\n\nИсточник: Автор\n\n#прогулка".into(),
                10000,
                "failed".into(),
                "Ошибка авторизации".into(),
                2
            )
        );
        assert_eq!(
            crate::drafts::kv_get(&db, &format!("publication_output:{first}")),
            output
        );
        assert_eq!(
            list(&db)
                .unwrap()
                .into_iter()
                .find(|p| p.id == second)
                .unwrap()
                .caption,
            old_second
        );
        // Удаление текста — тоже явное корректное обновление. Аккаунт можно править на паузе.
        let mut saved =
            caption::commands::load_for_account(&db, 1, PlatformId::Telegram, Some(2)).unwrap();
        saved.fields.body.clear();
        saved.fields.hashtags.clear();
        caption::commands::save_for_account(&db, 1, saved, Some(2)).unwrap();
        db.execute("UPDATE accounts SET enabled=0 WHERE id=2", [])
            .unwrap();
        refresh_caption(&db, first).unwrap();
        assert_eq!(
            list(&db)
                .unwrap()
                .into_iter()
                .find(|p| p.id == first)
                .unwrap()
                .caption,
            "Источник: Автор"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM events WHERE message LIKE '%подпись обновлена%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }
    #[test]
    fn refreshing_caption_refuses_uncertain_sent_legacy_and_invalid_content_atomically() {
        let (db, _file) = fixture();
        let id = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let old = crate::drafts::kv_get(&db, &format!("publication_output:{id}"));
        for status in ["sending", "done", "needs_check", "cancelled", "queued"] {
            db.execute(
                "UPDATE publications SET state=?2 WHERE id=?1",
                params![id, status],
            )
            .unwrap();
            assert!(refresh_caption(&db, id).is_err(), "{status}");
        }
        db.execute(
            "UPDATE publications SET state='scheduled',legacy=1,platform='tiktok' WHERE id=?1",
            [id],
        )
        .unwrap();
        assert!(refresh_caption(&db, id).is_err());
        db.execute(
            "UPDATE publications SET legacy=0,platform='telegram' WHERE id=?1",
            [id],
        )
        .unwrap();
        let text = list(&db).unwrap()[0].caption.clone();
        let mut saved =
            caption::commands::load_for_account(&db, 1, PlatformId::Telegram, Some(2)).unwrap();
        saved.fields.body = "Слишком длинный текст. ".repeat(200);
        caption::commands::save_for_account(&db, 1, saved, Some(2)).unwrap();
        assert!(refresh_caption(&db, id).is_err());
        assert_eq!(list(&db).unwrap()[0].caption, text);
        assert_eq!(
            crate::drafts::kv_get(&db, &format!("publication_output:{id}")),
            old
        );
        assert!(refresh_caption(&db, 999).is_err());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM events WHERE message LIKE '%подпись обновлена%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn refreshing_youtube_caption_changes_title_but_preserves_visibility_and_rolls_back_on_error() {
        let (db, _file) = fixture();
        db.execute("UPDATE accounts SET platform='youtube' WHERE id=2", [])
            .unwrap();
        let id = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let output = crate::drafts::kv_get(&db, &format!("publication_output:{id}"));
        let mut saved =
            caption::commands::load_for_account(&db, 1, PlatformId::Youtube, Some(2)).unwrap();
        saved.fields.title = "Мой новый заголовок".into();
        saved.fields.body = "Мой текст".into();
        caption::commands::save_for_account(&db, 1, saved.clone(), Some(2)).unwrap();
        crate::drafts::kv_set(
            &db,
            "youtube_settings:2",
            r#"{"privacy":"public","madeForKids":true}"#,
        )
        .unwrap();
        refresh_caption(&db, id).unwrap();
        let meta = youtube_metadata(&db, id, 1, 2).unwrap();
        assert_eq!(meta.title, saved.fields.title);
        assert_eq!(meta.settings.privacy, "private");
        assert!(!meta.settings.made_for_kids);
        assert_eq!(list(&db).unwrap()[0].not_before, 10000);
        assert!(list(&db).unwrap()[0].caption.starts_with("Мой текст"));
        assert_eq!(
            crate::drafts::kv_get(&db, &format!("publication_output:{id}")),
            output
        );
        let old_meta = crate::drafts::kv_get(&db, &format!("publication_metadata:{id}"));
        let old_text = list(&db).unwrap()[0].caption.clone();
        saved.fields.title = "Следующий заголовок".into();
        saved.fields.body = "Следующий текст".into();
        caption::commands::save_for_account(&db, 1, saved, Some(2)).unwrap();
        db.execute_batch("CREATE TRIGGER fail_caption_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event failure'); END;").unwrap();
        assert!(refresh_caption(&db, id).is_err());
        assert_eq!(list(&db).unwrap()[0].caption, old_text);
        assert_eq!(
            crate::drafts::kv_get(&db, &format!("publication_metadata:{id}")),
            old_meta
        );
        db.execute_batch("DROP TRIGGER fail_caption_event").unwrap();
        crate::drafts::kv_set(&db, &format!("publication_metadata:{id}"), "broken").unwrap();
        assert!(refresh_caption(&db, id).is_err());
        assert_eq!(list(&db).unwrap()[0].caption, old_text);
        assert!(db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none());
    }
    #[test]
    fn moving_youtube_slot_preserves_title_visibility_and_caption_snapshot() {
        let (db, _file) = fixture();
        db.execute(
            "UPDATE accounts SET platform='youtube',external_ref='UC_test' WHERE id=2",
            [],
        )
        .unwrap();
        let id = schedule(&db, 1, 2, Some(10000), 100).unwrap();
        let text = list(&db)
            .unwrap()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap()
            .caption;
        db.execute(
            "UPDATE moments SET title='Другое название',transcript='Новый текст' WHERE id=1",
            [],
        )
        .unwrap();
        crate::drafts::kv_set(
            &db,
            "youtube_settings:2",
            r#"{"privacy":"public","madeForKids":true}"#,
        )
        .unwrap();
        move_slot(&db, id, 11000, 100).unwrap();
        let meta = youtube_metadata(&db, id, 1, 2).unwrap();
        assert_eq!(meta.title, "Клип");
        assert_eq!(meta.settings.privacy, "private");
        assert!(!meta.settings.made_for_kids);
        assert_eq!(
            list(&db)
                .unwrap()
                .into_iter()
                .find(|p| p.id == id)
                .unwrap()
                .caption,
            text
        );
    }
}
