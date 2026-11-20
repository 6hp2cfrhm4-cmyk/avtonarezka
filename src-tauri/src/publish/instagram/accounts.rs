use super::*;
use crate::host::{AppHandle, State};
use crate::{publish::schedule::Policy, state::AppState};
use rusqlite::params;
use serde::Deserialize;
pub(crate) fn key(id: i64) -> String {
    format!("platform:instagram:account:{id}:page_token")
}
pub(crate) fn connector(token: String, user: String) -> Result<Instagram, String> {
    #[cfg(debug_assertions)]
    let client = match std::env::var("AVTO_TEST_INSTAGRAM_ENDPOINT") {
        Ok(base) => Instagram::with_endpoints(token, user, &base, &base),
        Err(_) => Instagram::new(token, user),
    };
    #[cfg(not(debug_assertions))]
    let client = Instagram::new(token, user);
    client.map_err(|e| e.message)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountArgs {
    pub id: Option<i64>,
    pub label: String,
    pub user: String,
    pub token: Option<String>,
    pub enabled: bool,
    pub policy: Policy,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn instagram_account_save(
    app: AppHandle,
    state: State<'_, AppState>,
    args: AccountArgs,
) -> Result<i64, String> {
    args.policy.validate()?;
    let label = args.label.trim();
    let user = args.user.trim();
    if label.is_empty() || label.chars().count() > 60 || !id(user) {
        return Err("Укажи название до 60 знаков и числовой Instagram User ID".into());
    }
    let existing = if let Some(account) = args.id {
        let db = state.db.0.lock().unwrap();
        let old: String = db
            .query_row(
                "SELECT external_ref FROM accounts WHERE id=?1 AND platform='instagram'",
                [account],
                |r| r.get(0),
            )
            .map_err(|_| "Аккаунт Instagram не найден")?;
        if old != user {
            return Err("Для другого Instagram ID создай новый аккаунт".into());
        }
        Some(account)
    } else {
        None
    };
    let token = match args.token.filter(|s| !s.is_empty()) {
        Some(token) => token,
        None => state
            .secrets
            .get(&key(
                existing.ok_or("Нужен токен Facebook Page с доступом к Instagram")?
            ))
            .await
            .map_err(|_| "Не удалось прочитать токен Instagram")?
            .ok_or("Подключи токен Instagram заново")?,
    };
    // Проверка без публикации: Business/Creator и актуальная квота, никаких угаданных 50/100.
    let client = connector(token.clone(), user.into())?;
    client.identity().await.map_err(|e| e.message)?;
    client.limit().await.map_err(|e| e.message)?;
    let account = {
        let db = state.db.0.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        let duplicate: i64 = tx.query_row("SELECT count(*) FROM accounts WHERE platform='instagram' AND external_ref=?1 AND id!=COALESCE(?2,-1)", params![user, existing], |r| r.get(0)).map_err(|e| e.to_string())?;
        if duplicate > 0 {
            return Err("Этот аккаунт Instagram уже добавлен".into());
        }
        let account = if let Some(account) = existing {
            account
        } else {
            tx.execute("INSERT INTO accounts(label,platform,external_ref,enabled,position)VALUES(?1,'instagram',?2,0,(SELECT COALESCE(MAX(position),0)+1 FROM accounts))", params![label, user]).map_err(|e| e.to_string())?;
            tx.last_insert_rowid()
        };
        // Сбой сохранения keyring не оставляет запись включённой со старым токеном.
        tx.execute("UPDATE accounts SET enabled=0 WHERE id=?1", [account])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        account
    };
    state
        .secrets
        .set(&key(account), &token)
        .await
        .map_err(|_| "Не удалось сохранить токен Instagram в связке ключей")?;
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET label=?2,enabled=?3,schedule_json=?4 WHERE id=?1",
            params![
                account,
                label,
                args.enabled,
                serde_json::to_string(&args.policy).map_err(|e| e.to_string())?
            ],
        )
        .map_err(|e| e.to_string())?;
    crate::publish::calendar::changed(&app);
    Ok(account)
}
