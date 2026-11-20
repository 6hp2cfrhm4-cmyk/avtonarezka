use super::*;
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::{
    drafts::{kv_get, kv_set},
    state::AppState,
};
use rusqlite::Connection;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub spec: CaptionSpec,
    pub fields: Fields,
    #[serde(default)]
    pub ad_locked: bool,
}
fn key(id: i64, platform: PlatformId) -> Result<String, String> {
    let name = match platform {
        PlatformId::Tiktok => "tiktok",
        PlatformId::Telegram => "telegram",
        PlatformId::Youtube => "youtube",
        PlatformId::Instagram => "instagram",
        _ => return Err("Подписи этой площадки пока недоступны".into()),
    };
    Ok(format!("caption_draft:{id}:{name}"))
}
fn context(db: &Connection, id: i64) -> Result<Fields, String> {
    let (title,tags,credit,snapshot):(String,String,Option<String>,Option<String>)=db.query_row(
        "SELECT COALESCE(NULLIF(m.title,''),p.title),m.hashtags,c.credit,f.profile_json
        FROM drafts d JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id
        LEFT JOIN factory_items f ON f.project_id=p.id LEFT JOIN channels c ON c.id=f.channel_id WHERE d.id=?1",[id],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|_|"Ролик не найден".to_string())?;
    let credit = credit.unwrap_or_else(|| {
        snapshot
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|s| s["channel"]["credit"].as_str().map(str::to_string))
            .unwrap_or_default()
    });
    Ok(Fields {
        title,
        body: String::new(),
        credit,
        hashtags: serde_json::from_str(&tags).unwrap_or_default(),
        ..Default::default()
    })
}
fn campaign_fields(db: &Connection, id: i64, saved: &mut Saved) -> Result<(), String> {
    let platform = serde_json::to_value(saved.spec.platform).map_err(|e| e.to_string())?;
    saved.ad_locked = false;
    if let Some((link, promo)) =
        crate::campaigns::caption_fields(db, id, platform.as_str().unwrap_or_default())?
    {
        saved.fields.link = link;
        saved.fields.promo = promo;
        saved.ad_locked = true;
    }
    Ok(())
}
pub(crate) fn load(db: &Connection, id: i64, platform: PlatformId) -> Result<Saved, String> {
    let mut context = context(db, id)?;
    if platform != PlatformId::Youtube {
        context.title.clear();
    }
    let key = key(id, platform)?;
    let mut saved = kv_get(db, &key)
        .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
        .unwrap_or_else(|| Saved {
            spec: CaptionSpec {
                platform,
                template: DEFAULT_TEMPLATE.into(),
            },
            fields: context.clone(),
            ad_locked: false,
        });
    saved.spec.platform = platform;
    saved.fields.credit = context.credit;
    campaign_fields(db, id, &mut saved)?;
    Ok(saved)
}
pub(crate) fn storage_key(
    db: &Connection,
    id: i64,
    platform: PlatformId,
    account: Option<i64>,
) -> Result<String, String> {
    let base = key(id, platform)?;
    if let Some(account) = account {
        let expected = serde_json::to_value(platform).map_err(|e| e.to_string())?;
        let actual: String = db
            .query_row(
                "SELECT platform FROM accounts WHERE id=?1",
                [account],
                |r| r.get(0),
            )
            .map_err(|_| "Аккаунт подписи не найден")?;
        if expected.as_str() != Some(actual.as_str()) {
            return Err("Аккаунт относится к другой площадке".into());
        }
        Ok(format!("{base}:account:{account}"))
    } else {
        Ok(base)
    }
}
pub(crate) fn load_for_account(
    db: &Connection,
    id: i64,
    platform: PlatformId,
    account: Option<i64>,
) -> Result<Saved, String> {
    let key = storage_key(db, id, platform, account)?;
    let default = load(db, id, platform)?;
    let mut saved = kv_get(db, &key)
        .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
        .unwrap_or(default);
    saved.spec.platform = platform;
    saved.fields.credit = context(db, id)?.credit;
    campaign_fields(db, id, &mut saved)?;
    Ok(saved)
}
#[cfg(test)]
pub(crate) fn has_saved(
    db: &Connection,
    id: i64,
    platform: PlatformId,
    account: Option<i64>,
) -> bool {
    let exists = [storage_key(db, id, platform, account), key(id, platform)]
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|k| kv_get(db, &k))
        .any(|raw| serde_json::from_str::<Saved>(&raw).is_ok_and(|s| s.spec.platform == platform));
    exists
        && load_for_account(db, id, platform, account)
            .is_ok_and(|s| compose(&s.spec, &s.fields).is_ok())
}
fn save(db: &Connection, id: i64, saved: Saved) -> Result<Composed, String> {
    save_for_account(db, id, saved, None)
}
pub(crate) fn save_for_account(
    db: &Connection,
    id: i64,
    mut saved: Saved,
    account: Option<i64>,
) -> Result<Composed, String> {
    let key = storage_key(db, id, saved.spec.platform, account)?;
    saved.fields.credit = context(db, id)?.credit;
    campaign_fields(db, id, &mut saved)?;
    let composed = compose(&saved.spec, &saved.fields)?;
    kv_set(
        db,
        &key,
        &serde_json::to_string(&saved).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(composed)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn caption_draft_load(
    state: State<'_, AppState>,
    draft_id: i64,
    platform: PlatformId,
) -> Result<Saved, String> {
    load(&state.db.0.lock().unwrap(), draft_id, platform)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn caption_draft_save(
    state: State<'_, AppState>,
    draft_id: i64,
    saved: Saved,
) -> Result<Composed, String> {
    save(&state.db.0.lock().unwrap(), draft_id, saved)
}

pub(crate) fn apply_body(
    db: &Connection,
    ids: &[i64],
    platform: PlatformId,
    account: Option<i64>,
    body: &str,
) -> Result<usize, String> {
    if ids.is_empty() || ids.len() > 500 {
        return Err("Выбери от 1 до 500 роликов".into());
    }
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut project = None;
    let mut seen = std::collections::HashSet::new();
    for &id in ids {
        if !seen.insert(id) {
            continue;
        }
        let current:i64=tx.query_row("SELECT m.project_id FROM drafts d JOIN moments m ON m.id=d.moment_id WHERE d.id=?1 AND d.status='final_done'",[id],|r|r.get(0)).map_err(|_|format!("Ролик #{id} не найден или ещё не готов"))?;
        if project.is_some_and(|old| old != current) {
            return Err("Общий текст применяется к роликам одного проекта".into());
        }
        project = Some(current);
        let mut accounts = vec![account];
        if account.is_none() {
            let base = key(id, platform)?;
            let platform = serde_json::to_value(platform).map_err(|e| e.to_string())?;
            let mut query=tx.prepare("SELECT a.id FROM kv JOIN accounts a ON kv.key=(?1 || a.id) WHERE a.platform=?2").map_err(|e|e.to_string())?;
            let ids = query
                .query_map(
                    rusqlite::params![format!("{base}:account:"), platform.as_str()],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())?;
            for id in ids {
                accounts.push(Some(id.map_err(|e| e.to_string())?));
            }
        }
        for account in accounts {
            let mut saved = load_for_account(&tx, id, platform, account)?;
            saved.fields.body = body.into();
            if compose(&saved.spec, &saved.fields)?.body_trimmed {
                return Err(format!(
                    "Общий текст не помещается в подпись ролика #{id}. Сократи его"
                ));
            }
            save_for_account(&tx, id, saved, account)?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(seen.len())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn caption_apply_body(
    state: State<'_, AppState>,
    draft_ids: Vec<i64>,
    platform: PlatformId,
    account_id: Option<i64>,
    body: String,
) -> Result<usize, String> {
    apply_body(
        &state.db.0.lock().unwrap(),
        &draft_ids,
        platform,
        account_id,
        &body,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Generated {
    pub saved: Saved,
    pub composed: Composed,
}
fn retry_on_cpu(error: &str) -> bool {
    let error = error.to_lowercase();
    [
        "llm-worker закрыл вывод",
        "broken pipe",
        "vulkan",
        "out of memory",
        "allocation",
        "ошибка чтения запроса",
        "ошибка генерации",
    ]
    .iter()
    .any(|s| error.contains(s))
}
/// Один общий ML permit включает ожидание Whisper и весь срок жизни LLM-воркера.
pub(crate) async fn generate_for(
    app: &AppHandle,
    id: i64,
    platform: PlatformId,
    account: Option<i64>,
    input: Option<Saved>,
) -> Result<Generated, String> {
    use crate::{
        analyze::{
            llm::{self, LlmClient},
            pipeline::Options,
        },
        models::ModelPaths,
    };
    let state = app.state::<AppState>();
    let _permit = state
        .ml_gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| e.to_string())?;
    let (mut saved, transcript, title, key, old) = {
        let db = state.db.0.lock().unwrap();
        let mut saved = input.unwrap_or(load_for_account(&db, id, platform, account)?);
        saved.spec.platform = platform;
        saved.fields.credit = context(&db, id)?.credit;
        campaign_fields(&db, id, &mut saved)?;
        let (transcript,title):(String,String)=db.query_row("SELECT m.transcript,COALESCE(NULLIF(m.title,''),p.title) FROM drafts d JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id WHERE d.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"Ролик не найден")?;
        let key = storage_key(&db, id, platform, account)?;
        let old = kv_get(&db, &key);
        (saved, transcript, title, key, old)
    };
    let models = ModelPaths::in_dir(&state.models_dir());
    if !models.llm.is_file() {
        return Err("Нет локальной языковой модели. Скачай её через раздел моделей или введи подпись вручную".into());
    }
    if llm::find_worker().is_none() {
        return Err("Не найден локальный llm-worker рядом с приложением".into());
    }
    let opt = Options::default();
    let seed = (id as u32)
        .wrapping_mul(31)
        .wrapping_add(account.unwrap_or(0) as u32)
        .wrapping_add(rand::random::<u32>());
    let mut gpu = opt.llm_gpu_layers;
    let mut worker = match LlmClient::start(&models.llm, opt.threads, gpu).await {
        Ok(w) => w,
        Err(_) if gpu > 0 => {
            gpu = 0;
            LlmClient::start(&models.llm, opt.threads, 0)
                .await
                .map_err(|e| format!("Не удалось запустить локальную модель: {e:#}"))?
        }
        Err(e) => return Err(format!("Не удалось запустить локальную модель: {e:#}")),
    };
    if !worker.on_gpu() {
        gpu = 0;
    }
    let on = |message: &str| {
        let _=app.emit("caption-progress",serde_json::json!({"draftId":id,"platform":platform,"accountId":account,"message":message}));
    };
    on("Подбираю хэштеги локальной моделью");
    let mut result = super::generate::tags(&mut worker, &title, &transcript, seed).await;
    worker.shutdown().await;
    // При сбое GPU повторяем на CPU. Содержательные ошибки генерации не скрываются.
    if result.as_ref().is_err_and(|e| retry_on_cpu(e)) && gpu > 0 {
        let mut worker = LlmClient::start(&models.llm, opt.threads, 0)
            .await
            .map_err(|e| format!("Не удалось запустить модель на CPU: {e:#}"))?;
        result = super::generate::tags(&mut worker, &title, &transcript, seed).await;
        worker.shutdown().await;
    }
    saved.fields.hashtags = result?;
    let db = state.db.0.lock().unwrap();
    if kv_get(&db, &key) != old {
        return Err("Подпись изменена во время генерации. Сохрани текущий текст и повтори".into());
    }
    saved.fields.credit = context(&db, id)?.credit;
    let composed = save_for_account(&db, id, saved.clone(), account)?;
    Ok(Generated { saved, composed })
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn caption_draft_generate(
    app: AppHandle,
    draft_id: i64,
    saved: Saved,
    account_id: Option<i64>,
) -> Result<Generated, String> {
    generate_for(&app, draft_id, saved.spec.platform, account_id, Some(saved)).await
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn caption_account_load(
    state: State<'_, AppState>,
    draft_id: i64,
    platform: PlatformId,
    account_id: Option<i64>,
) -> Result<Saved, String> {
    load_for_account(&state.db.0.lock().unwrap(), draft_id, platform, account_id)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn caption_account_save(
    state: State<'_, AppState>,
    draft_id: i64,
    saved: Saved,
    account_id: Option<i64>,
) -> Result<Composed, String> {
    save_for_account(&state.db.0.lock().unwrap(), draft_id, saved, account_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_fallback_handles_worker_failure_without_repeating_invalid_requests() {
        assert!(retry_on_cpu("llm-worker закрыл вывод"));
        assert!(retry_on_cpu("Broken pipe (os error 32)"));
        assert!(retry_on_cpu("llm-worker: Vulkan out of memory"));
        assert!(!retry_on_cpu(
            "llm-worker: запрос не помещается в контекст: 4000 + 850 > 4096"
        ));
        assert!(!retry_on_cpu(
            "Не удалось составить достаточно полную и отличающуюся подпись"
        ));
    }
    #[test]
    fn actual_draft_context_saved_caption_and_authoritative_credit() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'a','a','Запись',30,640,360,25,1);
            INSERT INTO moments(id,project_id,start,\"end\",title,transcript)VALUES(1,1,0,30,'Хук','Настоящая речь');INSERT INTO drafts(id,moment_id)VALUES(1,1);
            INSERT INTO channels(id,name,credit)VALUES(1,'Автор','Канал автора');
            INSERT INTO factory_items(position,kind,source,project_id,channel_id)VALUES(1,'file','a',1,1);").unwrap();
        let mut s = load(&db, 1, PlatformId::Telegram).unwrap();
        assert!(s.fields.body.is_empty());
        s.fields.credit.clear();
        s.fields.link = "https://example.com".into();
        let composed = save(&db, 1, s).unwrap();
        assert!(composed.text.contains("Источник: Канал автора"));
        assert!(composed.text.contains("https://example.com"));
        let s = load(&db, 1, PlatformId::Telegram).unwrap();
        assert_eq!(s.fields.link, "https://example.com");
        assert_eq!(s.fields.credit, "Канал автора");
        db.execute("UPDATE channels SET credit='Новый кредит' WHERE id=1", [])
            .unwrap();
        assert_eq!(
            load(&db, 1, PlatformId::Telegram).unwrap().fields.credit,
            "Новый кредит"
        );
        assert!(load(&db, 99, PlatformId::Telegram).is_err());
    }
    #[test]
    fn account_variants_are_isolated_and_other_projects_are_preserved() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'a','a','Один',30,640,360,25,1),(2,'b','b','Два',30,640,360,25,1);INSERT INTO moments(id,project_id,start,\"end\",title,transcript)VALUES(1,1,0,30,'Эпизод','Содержательная речь'),(2,2,0,30,'Другой','Другая речь');INSERT INTO drafts(id,moment_id)VALUES(1,1),(2,2);INSERT INTO accounts(id,label,profile,platform)VALUES(90,'Телеграм1','tg1','telegram'),(91,'Телеграм2','tg2','telegram'),(92,'Reels','ig','instagram');").unwrap();
        let mut general = load(&db, 1, PlatformId::Telegram).unwrap();
        general.fields.body = "Общий текст о соревновании.".into();
        save(&db, 1, general.clone()).unwrap();
        let mut first = general.clone();
        first.fields.body = "Первый вариант о событиях этого эпизода.".into();
        save_for_account(&db, 1, first.clone(), Some(90)).unwrap();
        assert_eq!(
            load_for_account(&db, 1, PlatformId::Telegram, Some(90))
                .unwrap()
                .fields
                .body,
            first.fields.body
        );
        assert_eq!(
            load_for_account(&db, 1, PlatformId::Telegram, Some(91))
                .unwrap()
                .fields
                .body,
            general.fields.body
        );
        assert!(load_for_account(&db, 1, PlatformId::Telegram, Some(92)).is_err());
        assert!(save_for_account(&db, 1, first, Some(999)).is_err());
        let mut other = load(&db, 2, PlatformId::Telegram).unwrap();
        other.fields.body = "Посторонний проект.".into();
        save_for_account(&db, 2, other, Some(91)).unwrap();
        assert!(has_saved(&db, 1, PlatformId::Telegram, Some(90)));
        let key = storage_key(&db, 2, PlatformId::Instagram, Some(92)).unwrap();
        kv_set(&db, &key, "broken").unwrap();
        assert!(!has_saved(&db, 2, PlatformId::Instagram, Some(92)));
    }
    #[test]
    fn bulk_manual_text_is_atomic_and_preserves_each_clips_tags_credit_and_ad() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute_batch("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'a','a','Запись',60,640,360,25,1),(2,'b','b','Другая',30,640,360,25,1);INSERT INTO moments(id,project_id,start,\"end\",title,hashtags)VALUES(1,1,0,30,'Один','[\"#заезд\"]'),(2,1,30,60,'Два','[\"#реакция\"]'),(3,2,0,30,'Чужой','[]');INSERT INTO drafts(id,moment_id,status)VALUES(1,1,'final_done'),(2,2,'final_done'),(3,3,'final_done');INSERT INTO accounts(id,label,profile,platform)VALUES(90,'Первый','tg1','telegram'),(91,'Второй','tg2','telegram');INSERT INTO channels(id,name,credit)VALUES(1,'Источник','Автор');INSERT INTO factory_items(position,kind,source,project_id,channel_id)VALUES(1,'file','a',1,1);").unwrap();
        let mut first = load(&db, 1, PlatformId::Telegram).unwrap();
        first.fields.body = "Отдельный текст".into();
        first.fields.link = "https://example.com/one".into();
        first.fields.promo = "КОД1".into();
        save(&db, 1, first.clone()).unwrap();
        let mut variant = first.clone();
        variant.fields.hashtags = vec!["#своитеги".into()];
        save_for_account(&db, 1, variant, Some(90)).unwrap();
        // Сохранённые варианты удалённых аккаунтов не мешают редактированию живых роликов.
        kv_set(&db, "caption_draft:1:telegram:account:999", "orphan").unwrap();
        assert_eq!(
            apply_body(
                &db,
                &[1, 2, 1],
                PlatformId::Telegram,
                None,
                "Общий текст 🙂"
            )
            .unwrap(),
            2
        );
        let one = load(&db, 1, PlatformId::Telegram).unwrap();
        let two = load(&db, 2, PlatformId::Telegram).unwrap();
        assert_eq!(one.fields.body, "Общий текст 🙂");
        assert_eq!(two.fields.body, one.fields.body);
        assert_eq!(one.fields.hashtags, vec!["#заезд"]);
        assert_eq!(two.fields.hashtags, vec!["#реакция"]);
        assert_eq!(one.fields.credit, "Автор");
        assert_eq!(one.fields.promo, "КОД1");
        assert_eq!(one.fields.link, "https://example.com/one");
        assert_eq!(
            kv_get(&db, "caption_draft:1:telegram:account:999").as_deref(),
            Some("orphan")
        );
        let specific = load_for_account(&db, 1, PlatformId::Telegram, Some(90)).unwrap();
        assert_eq!(specific.fields.body, one.fields.body);
        assert_eq!(specific.fields.hashtags, vec!["#своитеги"]);
        assert!(apply_body(&db, &[1, 3], PlatformId::Telegram, None, "Откат").is_err());
        assert_eq!(
            load(&db, 1, PlatformId::Telegram).unwrap().fields.body,
            one.fields.body
        );
        assert!(apply_body(
            &db,
            &[1, 2],
            PlatformId::Telegram,
            None,
            &"Длинно".repeat(300)
        )
        .is_err());
        assert_eq!(
            load(&db, 1, PlatformId::Telegram).unwrap().fields.body,
            one.fields.body
        );
        apply_body(
            &db,
            &[1, 2],
            PlatformId::Telegram,
            Some(90),
            "Текст аккаунта",
        )
        .unwrap();
        assert_eq!(
            load(&db, 1, PlatformId::Telegram).unwrap().fields.body,
            one.fields.body
        );
        assert_eq!(
            load_for_account(&db, 2, PlatformId::Telegram, Some(90))
                .unwrap()
                .fields
                .body,
            "Текст аккаунта"
        );
        apply_body(&db, &[1, 2], PlatformId::Telegram, None, "").unwrap();
        assert!(load_for_account(&db, 2, PlatformId::Telegram, Some(90))
            .unwrap()
            .fields
            .body
            .is_empty());
        assert!(load(&db, 1, PlatformId::Telegram)
            .unwrap()
            .fields
            .body
            .is_empty());
        assert_eq!(
            load(&db, 1, PlatformId::Youtube).unwrap().fields.title,
            "Один"
        );
    }
}
