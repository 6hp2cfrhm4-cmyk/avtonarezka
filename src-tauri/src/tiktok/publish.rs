//! Отправка одного ролика: проверка → запрос на загрузку → куски → ожидание ответа TikTok.

use std::{path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, Context};
use serde_json::json;

use super::api::{self, Mode, PostState, TikTok};

#[derive(Clone, Debug)]
pub struct PostRequest {
    pub video: PathBuf,
    pub duration_sec: f64,
    pub mode: Mode,
    /// Подпись с хэштегами. Только для `Mode::Direct`: у черновиков TikTok подписи нет, её вставляют в приложении.
    pub title: String,
    pub privacy: String,
    pub allow_comment: bool,
    pub allow_duet: bool,
    pub allow_stitch: bool,
}

#[derive(Clone, Debug)]
pub struct Timing {
    pub poll_every: Duration,
    pub poll_timeout: Duration,
    pub retry_delay: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        // у запроса статуса лимит 30 в минуту: 4 секунды дают 15
        Self { poll_every: Duration::from_secs(4), poll_timeout: Duration::from_secs(300), retry_delay: Duration::from_secs(2) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub publish_id: String,
    /// `SEND_TO_USER_INBOX` (лежит в черновиках) или `PUBLISH_COMPLETE`.
    pub status: String,
    pub post_ids: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct Progress {
    /// 0..1 по всей отправке: загрузка занимает 0–0.9, ожидание ответа — остаток.
    pub fraction: f64,
    pub message: String,
}

/// Для публикации сразу: видимость должна быть среди разрешённых аккаунту, ролик не длиннее лимита.
pub fn check_direct(info: &api::CreatorInfo, req: &PostRequest) -> anyhow::Result<()> {
    if !info.privacy_options.contains(&req.privacy) {
        bail!("Видимость «{}» недоступна этому аккаунту. Доступно: {}", req.privacy, info.privacy_options.join(", "));
    }
    if let Some(max) = info.max_duration_sec {
        if req.duration_sec > max + 0.5 {
            bail!("Ролик ({:.0} с) длиннее, чем разрешено аккаунту ({:.0} с)", req.duration_sec, max);
        }
    }
    Ok(())
}

pub async fn publish(api: &TikTok, access: &str, req: &PostRequest, timing: &Timing, on: &mut (dyn FnMut(Progress) + Send)) -> anyhow::Result<Outcome> {
    let size = tokio::fs::metadata(&req.video).await.with_context(|| format!("не найден файл {}", req.video.display()))?.len();
    let plan = api::plan_chunks(size)?;

    let post_info = match req.mode {
        Mode::Inbox => None,
        Mode::Direct => {
            on(Progress { fraction: 0.0, message: "Проверяю аккаунт TikTok".into() });
            let info = api.creator_info(access).await?;
            check_direct(&info, req)?;
            Some(json!({
                "title": req.title,
                "privacy_level": req.privacy,
                "disable_comment": !req.allow_comment,
                "disable_duet": !req.allow_duet,
                "disable_stitch": !req.allow_stitch,
            }))
        }
    };

    on(Progress { fraction: 0.01, message: "Начинаю загрузку".into() });
    let init = api.init_upload(access, req.mode, post_info, size, &plan).await?;

    let mime = api::mime_of(&req.video);
    api.upload_file(&init.upload_url, &req.video, size, &plan, mime, timing.retry_delay, |sent| {
        on(Progress { fraction: 0.9 * sent as f64 / size as f64, message: format!("Загружаю в TikTok: {} из {} МБ", sent / 1_000_000, size / 1_000_000) });
    })
    .await?;

    // TikTok обрабатывает файл не сразу: опрашиваем статус, пока не станет окончательным.
    let started = tokio::time::Instant::now();
    loop {
        match api.status(access, &init.publish_id).await {
            Ok(PostState::InInbox) => {
                on(Progress { fraction: 1.0, message: "Ролик в черновиках TikTok".into() });
                return Ok(Outcome { publish_id: init.publish_id, status: "SEND_TO_USER_INBOX".into(), post_ids: vec![] });
            }
            Ok(PostState::Complete(ids)) => {
                on(Progress { fraction: 1.0, message: "Опубликовано".into() });
                return Ok(Outcome { publish_id: init.publish_id, status: "PUBLISH_COMPLETE".into(), post_ids: ids });
            }
            Ok(PostState::Failed(why)) => return Err(anyhow!(why)),
            Ok(PostState::Processing) => {}
            // один сбой опроса не повод считать отправку проваленной: файл уже загружен
            Err(e) if started.elapsed() > timing.poll_timeout => return Err(e),
            Err(_) => {}
        }
        if started.elapsed() > timing.poll_timeout {
            bail!("TikTok слишком долго обрабатывает ролик. Проверь приложение TikTok: он мог появиться позже.");
        }
        let wait = started.elapsed().as_secs_f64() / timing.poll_timeout.as_secs_f64().max(1.0);
        on(Progress { fraction: 0.9 + 0.09 * wait.min(1.0), message: "TikTok обрабатывает ролик".into() });
        tokio::time::sleep(timing.poll_every).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(options: &[&str], max: Option<f64>) -> api::CreatorInfo {
        api::CreatorInfo { nickname: "n".into(), privacy_options: options.iter().map(|s| s.to_string()).collect(), max_duration_sec: max }
    }

    fn req(privacy: &str, secs: f64) -> PostRequest {
        PostRequest { video: "/v.mp4".into(), duration_sec: secs, mode: Mode::Direct, title: "t".into(), privacy: privacy.into(), allow_comment: false, allow_duet: false, allow_stitch: false }
    }

    #[test]
    fn direct_post_needs_an_allowed_privacy_level() {
        let i = info(&["SELF_ONLY"], None);
        assert!(check_direct(&i, &req("SELF_ONLY", 30.0)).is_ok());
        let e = check_direct(&i, &req("PUBLIC_TO_EVERYONE", 30.0)).unwrap_err().to_string();
        assert!(e.contains("недоступна") && e.contains("SELF_ONLY"), "{e}");
    }

    #[test]
    fn direct_post_checks_account_duration_limit() {
        let i = info(&["SELF_ONLY"], Some(60.0));
        assert!(check_direct(&i, &req("SELF_ONLY", 60.2)).is_ok());
        assert!(check_direct(&i, &req("SELF_ONLY", 75.0)).is_err());
    }
}
