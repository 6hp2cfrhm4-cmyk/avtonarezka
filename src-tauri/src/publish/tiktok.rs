//! Адаптер прежнего проверенного API-конвейера. Общий CDP-механизм находится в publish/browser.rs.
use super::*;
use crate::host::AppHandle;
use crate::tiktok::{
    api::{Mode, TikTok},
    publish::{self, PostRequest, Timing},
};

/// Использует сохранённый профиль конкретного аккаунта и прежний CDP-конвейер.
/// Успех подтверждается страницей; внешний ID браузер пока не предоставляет.
pub(crate) struct TikTokBrowser {
    pub app: AppHandle,
    pub draft: i64,
    pub profile: PathBuf,
}
impl Platform for TikTokBrowser {
    fn caps(&self) -> Caps {
        TIKTOK
    }
    fn method(&self) -> &'static str {
        "browser"
    }
    fn publish<'a>(
        &'a self,
        upload: &'a Upload,
        on: &'a mut (dyn FnMut(f64, String) + Send),
    ) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(async move {
            validate(upload, TIKTOK)?;
            on(0.0, "Открываю TikTok с профилем выбранного аккаунта".into());
            let result = crate::tiktok::browser::run_browser_post(
                &self.app,
                self.draft,
                crate::tiktok::browser::BrowserPost {
                    file: upload.path.clone(),
                    caption: upload.caption.clone(),
                    auto_publish: true,
                    require_confirmation: true,
                    close_on_success: true,
                    profile: self.profile.clone(),
                },
            )
            .await;
            match result {
                Ok(out) if out.confirmed => {
                    on(1.0, "TikTok подтвердил публикацию".into());
                    Ok(Receipt {
                        external_id: String::new(),
                        url: None,
                        published: true,
                    })
                }
                Ok(_) => Err(Failure::new(
                    FailureKind::NeedsCheck,
                    "TikTok не подтвердил публикацию. Проверь аккаунт",
                )),
                Err(e) => Err(Failure::new(
                    if e.downcast_ref::<crate::tiktok::browser::AfterClick>()
                        .is_some()
                    {
                        FailureKind::NeedsCheck
                    } else {
                        FailureKind::Rejected
                    },
                    format!("{e:#}"),
                )),
            }
        })
    }
}
pub struct TikTokApi {
    pub client: TikTok,
    pub access: String,
    pub mode: Mode,
    pub privacy: String,
    pub timing: Timing,
}
impl Platform for TikTokApi {
    fn caps(&self) -> Caps {
        TIKTOK
    }
    fn publish<'a>(
        &'a self,
        upload: &'a Upload,
        on: &'a mut (dyn FnMut(f64, String) + Send),
    ) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(async move {
            validate(upload, TIKTOK)?;
            if upload
                .scheduled_at
                .is_some_and(|t| t > crate::tiktok::api::now_unix())
            {
                return Err(Failure::new(
                    FailureKind::Rejected,
                    "TikTok отправляет через календарь приложения в назначенное время",
                ));
            }
            let req = PostRequest {
                video: upload.path.clone(),
                duration_sec: upload.duration,
                mode: self.mode,
                title: upload.caption.clone(),
                privacy: self.privacy.clone(),
                allow_comment: false,
                allow_duet: false,
                allow_stitch: false,
            };
            let out = publish::publish(&self.client, &self.access, &req, &self.timing, &mut |p| {
                on(p.fraction, p.message)
            })
            .await
            .map_err(|_| {
                Failure::new(
                    FailureKind::NeedsCheck,
                    "Отправка TikTok не подтверждена. Проверь аккаунт перед повтором",
                )
            })?;
            Ok(Receipt {
                external_id: out.publish_id,
                url: None,
                published: out.status == "PUBLISH_COMPLETE",
            })
        })
    }
}
