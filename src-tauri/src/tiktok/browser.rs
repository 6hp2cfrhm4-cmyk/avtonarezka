//! Запасной путь: ролик заливается через обычную страницу загрузки TikTok в Chromium.
//!
//! Программа запускает Chromium с отдельным профилем и управляет им по протоколу отладки (CDP) на локальном порту.
//! Входит в TikTok сам пользователь, в окне браузера: пароль программа не видит и не хранит, а куки остаются в профиле.
//!
//! Хрупкость: путь зависит от вёрстки сайта TikTok. Селекторы ниже — рабочая гипотеза, не гарантия; сверить их с настоящей
//! страницей загрузки может только человек с аккаунтом. Они вынесены в `Selectors`, чтобы чинить в одном месте.
//!
//! Чек-пойнт фабрики, 2026-11-14: проверка на живом аккаунте ожидает пользователя.
//! Не подтверждены: приём 3999 символов веб-полем, success_url, confirm_button и
//! видимость по умолчанию. Мок-тесты CDP не являются проверкой действующей страницы.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::host::{AppHandle, Emitter, Manager, State};
use crate::publish::browser::open_tab;
use anyhow::anyhow;

use crate::{
    accounts,
    drafts::get as get_draft,
    state::AppState,
    tiktok::api::make_caption,
    tiktok_cmds::{
        begin_post, finish_post, mark_sent, PostDone, PostFailed, PostProgress, PublishArgs,
        PublishStart,
    },
};

pub const UPLOAD_URL: &str = "https://www.tiktok.com/tiktokstudio/upload";

#[derive(Clone, Debug)]
pub struct Selectors {
    pub file_input: String,
    pub caption: String,
    pub post_button: String,
    /// Кнопка в окне «Продолжить публикацию?» (TikTok так спрашивает, пока идёт проверка авторских прав). Пусто — окна нет.
    pub confirm_button: String,
    /// Признак успеха: адрес страницы после публикации содержит эту строку…
    pub success_url: String,
    /// …или на странице появился этот элемент. Если не задано ни то, ни другое, очередь не стартует: подтвердить публикацию нечем.
    pub success_selector: String,
    /// Окно проверки «я не робот». Видимое — значит, дальше решает человек: обходить такие проверки программа не пытается.
    pub captcha: String,
}

impl Default for Selectors {
    fn default() -> Self {
        // Рабочая гипотеза, не проверенная на живой странице (см. заголовок файла): сверять нужно человеку с аккаунтом.
        Self {
            file_input: r#"input[type="file"]"#.into(),
            caption: r#".public-DraftEditor-content[contenteditable="true"], div[contenteditable="true"]"#.into(),
            post_button: r#"button[data-e2e="post_video_button"]"#.into(),
            confirm_button: String::new(),
            success_url: "tiktokstudio/content".into(),
            success_selector: String::new(),
            captcha: r#"[id*="captcha" i], [class*="captcha" i]"#.into(),
        }
    }
}

pub use crate::publish::browser::{
    ensure_browser, find_chromium, AfterClick, Cdp, DriveOutcome, NeedsHuman,
};

pub struct DriveConfig {
    pub chromium: PathBuf,
    pub profile: PathBuf,
    pub headless: bool,
    pub upload_url: String,
    pub file: PathBuf,
    pub caption: String,
    pub auto_publish: bool,
    pub selectors: Selectors,
    /// Сколько ждать, пока пользователь войдёт в аккаунт.
    pub login_timeout: Duration,
    /// Сколько ждать каждого следующего шага страницы (загрузка файла, редактор, кнопка).
    pub step_timeout: Duration,
    /// После клика дождаться, пока TikTok подтвердит публикацию (адрес или элемент из `Selectors`). Для очереди обязательно.
    pub require_confirmation: bool,
    /// Сколько ждать этого подтверждения.
    pub confirm_timeout: Duration,
    /// После подтверждённой публикации закрыть вкладку: очередь из десятков роликов не должна копить окна.
    pub close_on_success: bool,
}

/// Прежний интерфейс TikTok использует общий браузерный механизм.
pub async fn drive(
    cfg: &DriveConfig,
    on: &mut (dyn FnMut(f64, &str) + Send),
) -> anyhow::Result<DriveOutcome> {
    let s = &cfg.selectors;
    crate::publish::browser::drive(&crate::publish::browser::DriveConfig {
        label: "TikTok".into(), caption_limit: crate::caption::TIKTOK_MAX,
        upload_failed_expression: r#"(() => { const t = document.body ? document.body.innerText : ''; return /Не удалось загрузить|Ошибка загрузки/.test(t); })()"#.into(),
        chromium: cfg.chromium.clone(), profile: cfg.profile.clone(), headless: cfg.headless,
        upload_url: cfg.upload_url.clone(), file: cfg.file.clone(), caption: cfg.caption.clone(),
        auto_publish: cfg.auto_publish,
        selectors: crate::publish::browser::Selectors {
            file_input: s.file_input.clone(), caption: s.caption.clone(), post_button: s.post_button.clone(),
            confirm_button: s.confirm_button.clone(), success_url: s.success_url.clone(),
            success_selector: s.success_selector.clone(), captcha: s.captcha.clone(),
        },
        login_timeout: cfg.login_timeout, step_timeout: cfg.step_timeout,
        require_confirmation: cfg.require_confirmation, confirm_timeout: cfg.confirm_timeout,
        close_on_success: cfg.close_on_success,
    }, on).await.map_err(|error| {
        // Общий механизм сохраняет тип исхода, адаптер — понятное место проверки.
        if let Some(after) = error.downcast_ref::<AfterClick>() {
            anyhow::Error::new(AfterClick(after.0.replace("в своём аккаунте", "в TikTok Studio")))
        } else if let Some(human) = error.downcast_ref::<NeedsHuman>() {
            anyhow::Error::new(NeedsHuman(human.0.replace("Площадка", "TikTok")))
        } else {
            error
        }
    })
}

// ---------------------------------------------------------------- стык с приложением

/// Длина подписи, которую принимает страница загрузки TikTok Studio (у API `title` короче: 2200). Сверена по сторонним
/// источникам, не по живой странице: ошибка здесь поймается сверкой текста в поле до клика (см. `drive`).
pub const CAPTION_LIMIT: usize = 4000;

/// Что и как отправить через страницу загрузки.
pub(crate) struct BrowserPost {
    pub file: PathBuf,
    pub caption: String,
    pub auto_publish: bool,
    /// Дождаться подтверждения публикации от TikTok. Для очереди обязательно.
    pub require_confirmation: bool,
    pub close_on_success: bool,
    /// Профиль Chromium аккаунта, с которого идёт выкладка: в нём лежит вход в TikTok.
    pub profile: PathBuf,
}

/// Окно Chromium с профилем аккаунта на странице загрузки, чтобы человек вошёл в TikTok. Не ждёт входа:
/// окно остаётся открытым, а очередь потом пользуется тем же профилем.
pub(crate) async fn open_login_window(chromium: &Path, profile: &Path) -> anyhow::Result<()> {
    let port = ensure_browser(chromium, profile, false, "about:blank").await?;
    open_tab(port, &upload_url()).await?;
    Ok(())
}

fn upload_url() -> String {
    #[cfg(debug_assertions)]
    {
        std::env::var("AVTO_TIKTOK_UPLOAD_URL").unwrap_or_else(|_| UPLOAD_URL.into())
    }
    #[cfg(not(debug_assertions))]
    {
        UPLOAD_URL.to_string()
    }
}

/// Один ролик через браузер, с событиями хода `post-progress`. Записей в журнал не делает: это забота вызывающего.
pub(crate) async fn run_browser_post(
    app: &AppHandle,
    draft_id: i64,
    p: BrowserPost,
) -> anyhow::Result<DriveOutcome> {
    let chromium = find_chromium()
        .ok_or_else(|| anyhow!("Не найден Chromium. Установи пакет chromium или Google Chrome"))?;
    // по одной выкладке за раз: очередь и ручной диалог не пишут в браузер одновременно
    let state = app.state::<AppState>();
    let _turn = state.browser_gate.lock().await;
    crate::factory::rights::require_clip(&state.db.0.lock().unwrap(), draft_id)
        .map_err(anyhow::Error::msg)?;
    #[allow(unused_mut)] // в release подмен нет
    let mut selectors = Selectors::default();
    #[cfg(debug_assertions)]
    {
        // макет страницы для проверок без настоящего TikTok
        if let Ok(v) = std::env::var("AVTO_TIKTOK_SUCCESS_URL") {
            selectors.success_url = v;
        }
        if let Ok(v) = std::env::var("AVTO_TIKTOK_CONFIRM") {
            selectors.confirm_button = v;
        }
    }
    // в проверках без человека окно браузера не нужно (только debug-сборка)
    let headless =
        cfg!(debug_assertions) && std::env::var("AVTO_TIKTOK_HEADLESS").is_ok_and(|v| v == "1");
    let cfg = DriveConfig {
        chromium,
        profile: p.profile,
        headless,
        upload_url: upload_url(),
        file: p.file,
        caption: p.caption,
        auto_publish: p.auto_publish,
        selectors,
        login_timeout: Duration::from_secs(600),
        step_timeout: Duration::from_secs(180),
        require_confirmation: p.require_confirmation,
        confirm_timeout: Duration::from_secs(120),
        close_on_success: p.close_on_success,
    };
    let reporter = app.clone();
    drive(&cfg, &mut |fraction, message| {
        let _ = reporter.emit(
            "post-progress",
            PostProgress {
                draft_id,
                fraction,
                message: message.to_string(),
            },
        );
    })
    .await
}

pub(crate) async fn start(
    app: AppHandle,
    state: State<'_, AppState>,
    args: PublishArgs,
    file: PathBuf,
    auto_publish: bool,
) -> Result<PublishStart, String> {
    find_chromium()
        .ok_or("Не найден Chromium. Установи пакет chromium или выбери способ «через API»")?;
    // ручной диалог выкладывает с основного аккаунта: его вход лежит в его профиле
    let (post_id, caption, account, profile) = {
        let db = state.db.0.lock().unwrap();
        let d = get_draft(&db, args.draft_id)?;
        crate::factory::rights::require_project(&db, d.project_id)?;
        db.execute(
            "UPDATE moments SET caption = ?2, hashtags = ?3 WHERE id = ?1",
            rusqlite::params![
                d.moment_id,
                args.caption.trim(),
                serde_json::to_string(&crate::tiktok::api::normalize_hashtags(&args.hashtags))
                    .unwrap_or_else(|_| "[]".into())
            ],
        )
        .map_err(|e| e.to_string())?;
        let account = accounts::main_id(&db);
        let profile = accounts::profile_dir(&db, &state.data_dir, account)?;
        (
            begin_post(&db, args.draft_id, "browser", "browser").map_err(|e| e.to_string())?,
            make_caption(&args.caption, &args.hashtags, CAPTION_LIMIT),
            account,
            profile,
        )
    };
    state.posting.lock().unwrap().insert(args.draft_id);
    let draft_id = args.draft_id;

    crate::host::spawn(async move {
        let result = run_browser_post(
            &app,
            draft_id,
            BrowserPost {
                file,
                caption,
                auto_publish,
                require_confirmation: false,
                close_on_success: false,
                profile,
            },
        )
        .await;

        let state = app.state::<AppState>();
        state.posting.lock().unwrap().remove(&draft_id);
        let stored = match &result {
            Ok(o) if o.clicked_post => Ok((
                "Нажата кнопка «Опубликовать» в окне TikTok. Проверь, что ролик вышел.".to_string(),
                None,
            )),
            Ok(_) => Ok((
                "Файл и подпись внесены в окне TikTok. Осталось проверить и нажать «Опубликовать»."
                    .to_string(),
                None,
            )),
            Err(e) => Err(format!("{e:#}")),
        };
        {
            let db = state.db.0.lock().unwrap();
            finish_post(&db, post_id, &stored);
            if matches!(&result, Ok(o) if o.clicked_post) {
                mark_sent(&db, post_id, account);
            }
        }
        let _ = match stored {
            Ok((detail, _)) => app.emit("post-done", PostDone { draft_id, detail }),
            Err(message) => app.emit("post-error", PostFailed { draft_id, message }),
        };
    });
    Ok(PublishStart {
        post_id: Some(post_id),
        already_sent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_selectors_cover_the_three_page_elements() {
        let s = Selectors::default();
        assert!(
            s.file_input.contains("file")
                && s.caption.contains("contenteditable")
                && s.post_button.contains("post_video_button")
        );
    }
}
