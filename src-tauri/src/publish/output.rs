//! Варианты под площадки: общий рендер, отдельный файл и замороженные настройки слота.
use super::{Caps, PlatformId};
use crate::host::{AppHandle, Emitter, Manager, State};
use crate::{drafts, render::OutputLimits, state::AppState};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn id(p: PlatformId) -> String {
    serde_json::to_value(p).unwrap().as_str().unwrap().into()
}
fn caps(p: PlatformId) -> Result<Caps, String> {
    match p {
        PlatformId::Tiktok => Ok(super::TIKTOK),
        PlatformId::Telegram => Ok(super::TELEGRAM),
        PlatformId::Youtube => Ok(super::YOUTUBE),
        PlatformId::Instagram => Ok(super::INSTAGRAM),
        _ => Err("Выходной профиль VK ожидает подключения площадки".into()),
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputProfile {
    pub platform: PlatformId,
    pub limits: OutputLimits,
}
fn default_profile(p: PlatformId) -> Result<OutputProfile, String> {
    Ok(OutputProfile {
        platform: p,
        limits: OutputLimits {
            max_seconds: caps(p)?.duration_max,
            video_kbps: 6000,
            audio_kbps: 128,
        },
    })
}
pub(crate) fn load(db: &Connection, p: PlatformId) -> Result<OutputProfile, String> {
    let value = drafts::kv_get(db, &format!("output_profile:{}", id(p)));
    let profile: OutputProfile = match value {
        Some(raw) => serde_json::from_str(&raw).map_err(|_| "Повреждён выходной профиль")?,
        None => default_profile(p)?,
    };
    if profile.platform != p {
        return Err("Площадка выходного профиля не совпадает с настройкой".into());
    }
    validate(&profile)?;
    Ok(profile)
}
fn validate(p: &OutputProfile) -> Result<(), String> {
    p.limits.validate()?;
    let cap = caps(p.platform)?;
    if p.platform == PlatformId::Instagram && p.limits.max_seconds.is_some_and(|s| s < 3.0) {
        return Err("Instagram Reels: длительность варианта от 3 с".into());
    }
    if cap
        .duration_max
        .is_some_and(|max| p.limits.max_seconds.is_none_or(|v| v > max))
    {
        return Err(format!(
            "{}: длительность варианта до {} с",
            cap.name,
            cap.duration_max.unwrap()
        ));
    }
    Ok(())
}
fn save(db: &Connection, p: OutputProfile) -> Result<OutputProfile, String> {
    validate(&p)?;
    drafts::kv_set(
        db,
        &format!("output_profile:{}", id(p.platform)),
        &serde_json::to_string(&p).map_err(err)?,
    )
    .map_err(err)?;
    Ok(p)
}
pub(crate) fn freeze(db: &Connection, publication: i64, p: PlatformId) -> Result<(), String> {
    drafts::kv_set(
        db,
        &format!("publication_output:{publication}"),
        &serde_json::to_string(&load(db, p)?).map_err(err)?,
    )
    .map_err(err)
}
fn publication_profile(
    db: &Connection,
    publication: Option<i64>,
    p: PlatformId,
) -> Result<OutputProfile, String> {
    if let Some(raw) =
        publication.and_then(|n| drafts::kv_get(db, &format!("publication_output:{n}")))
    {
        let profile: OutputProfile = serde_json::from_str(&raw)
            .map_err(|_| "Повреждён профиль выходного варианта публикации")?;
        if profile.platform != p {
            return Err("Площадка выходного варианта не совпадает с публикацией".into());
        }
        validate(&profile)?;
        Ok(profile)
    } else {
        load(db, p)
    }
}
/// Запас на контейнер/AAC и VBV; точный размер проверяется после ffmpeg.
fn budget(
    mut limits: OutputLimits,
    duration: f64,
    file_max: Option<u64>,
) -> Result<OutputLimits, String> {
    let seconds = limits.max_seconds.unwrap_or(duration).min(duration);
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("Неизвестна длительность варианта".into());
    }
    if let Some(bytes) = file_max {
        let video =
            (bytes as f64 * 8.0 * 0.94 / seconds / 1000.0 - f64::from(limits.audio_kbps)).floor();
        if video < 200.0 {
            return Err("Ролик не помещается в лимит файла даже при минимальном битрейте. Уменьши длительность варианта".into());
        }
        limits.video_kbps = limits.video_kbps.min(video as u32);
    }
    Ok(limits)
}
fn remove_part(db: &Connection, draft: i64, p: PlatformId) -> Result<bool, String> {
    let enabled: i64 = db
        .query_row(
            "SELECT count(*) FROM accounts WHERE enabled=1 AND platform=?1",
            [id(p)],
            |r| r.get(0),
        )
        .map_err(err)?;
    let targets:i64=db.query_row("SELECT count(DISTINCT target_account_id) FROM publications WHERE draft_id=?1 AND legacy=0 AND state!='cancelled'",[draft],|r|r.get(0)).map_err(err)?;
    Ok(enabled > 1 || targets > 1)
}
fn file_key(path: &Path) -> Result<serde_json::Value, String> {
    let m = std::fs::metadata(path)
        .map_err(|_| format!("Файл для выходного варианта не найден: {}", path.display()))?;
    let stamp = m
        .modified()
        .ok()
        .and_then(|s| s.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().to_string());
    Ok(serde_json::json!([path, m.len(), stamp]))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub url: String,
    pub poster_url: String,
    pub duration: f64,
    pub bytes: u64,
    pub video_kbps: u32,
    pub part_removed: bool,
    pub truncated: bool,
    pub max_seconds: Option<f64>,
}
async fn verified(
    path: &Path,
    caps: Caps,
    max: f64,
    hard_max: Option<f64>,
) -> Result<crate::media::VideoInfo, String> {
    let info = crate::media::probe(path).await.map_err(err)?;
    if !info.duration.is_finite()
        || info.duration <= 0.0
        || (info.duration - max).abs() > 0.12
        || info.width != 1080
        || info.height != 1920
    {
        return Err("Выходной вариант не прошёл проверку длительности/размера кадра".into());
    }
    if hard_max
        .or(caps.duration_max)
        .is_some_and(|max| info.duration > max + 0.001)
    {
        return Err("Выходной вариант длиннее заданного предела площадки".into());
    }
    if info.codec != "h264" || info.audio_codec.as_deref() != Some("aac") {
        return Err("Выходной вариант должен содержать H.264 и AAC".into());
    }
    if caps
        .file_max
        .is_some_and(|max| std::fs::metadata(path).map_or(true, |m| m.len() > max))
    {
        return Err("Выходной вариант превышает лимит файла площадки".into());
    }
    Ok(info)
}
pub(crate) async fn prepare(
    app: &AppHandle,
    draft: i64,
    p: PlatformId,
    publication: Option<i64>,
) -> Result<(PathBuf, Variant), String> {
    let state = app.state::<AppState>();
    let platform_caps = caps(p)?;
    let _source_use = state.source_use.read().await;
    let (profile, remove, final_path) = {
        let db = state.db.0.lock().unwrap();
        crate::factory::rights::require_clip(&db, draft)?;
        let d = drafts::get(&db, draft)?;
        let final_path = d
            .final_path
            .filter(|_| d.status == "final_done")
            .ok_or("Нужен готовый финал")?;
        file_key(Path::new(&final_path))?;
        let remove = remove_part(&db, draft, p)?;
        (
            publication_profile(&db, publication, p)?,
            remove,
            final_path,
        )
    };
    let mut job = drafts::output_job(app, draft, Path::new("variant.mp4"), remove)
        .await
        .map_err(err)?;
    let limits = budget(profile.limits, job.out_len(), platform_caps.file_max)?;
    let mut files = vec![
        file_key(&job.source.path)?,
        file_key(&job.gameplay.path)?,
        file_key(Path::new(&final_path))?,
    ];
    if let Some(b) = &job.banner {
        files.push(file_key(&b.path)?);
    }
    if let Some(m) = &job.music {
        files.push(file_key(&m.path)?);
    }
    let hash=hex::encode(Sha256::digest(serde_json::to_vec(&serde_json::json!({"version":3,"job":job,"limits":limits,"files":files,"removePart":remove})).map_err(err)?));
    let key = format!("draft-{draft}-{}-{}", id(p), &hash[..20]);
    let gate = state
        .output_prepares
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .clone();
    let _turn = gate.lock().await;
    let out = state
        .data_dir
        .join("output-variants")
        .join(format!("{key}.mp4"));
    let max = limits
        .max_seconds
        .unwrap_or(job.out_len())
        .min(job.out_len());
    let cached = if out.is_file() {
        verified(&out, platform_caps, max, limits.max_seconds)
            .await
            .ok()
    } else {
        None
    };
    let info = match cached {
        Some(info) => info,
        None => {
            let _permit = state.final_slots.clone().acquire_owned().await;
            crate::factory::rights::require_clip(&state.db.0.lock().unwrap(), draft)?;
            let reporter = app.clone();
            job.out = out.clone();
            let (_tx, cancel) = tokio::sync::watch::channel(false);
            crate::render::ffmpeg::render_limited(
                &job,
                &state.data_dir.join("tmp"),
                Some(&state.fonts_dir()),
                cancel,
                Some(&limits),
                move |fraction| {
                    let _ = reporter.emit(
                        "output-progress",
                        serde_json::json!({"draftId":draft,"platform":p,"fraction":fraction}),
                    );
                },
            )
            .await
            .map_err(err)?;
            match verified(&out, platform_caps, max, limits.max_seconds).await {
                Ok(info) => info,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&out).await;
                    return Err(e);
                }
            }
        }
    };
    crate::factory::rights::require_clip(&state.db.0.lock().unwrap(), draft)?;
    let poster = out.with_extension("jpg");
    let fresh = std::fs::metadata(&poster)
        .and_then(|m| m.modified())
        .ok()
        .zip(std::fs::metadata(&out).and_then(|m| m.modified()).ok())
        .is_some_and(|(p, v)| p >= v);
    if !fresh {
        crate::media::make_thumb(&out, &poster, 0.5_f64.min(info.duration / 2.0))
            .await
            .map_err(err)?;
    }
    let result = Variant {
        url: state.media.register(out.clone()),
        poster_url: state.media.register(poster),
        duration: info.duration,
        bytes: std::fs::metadata(&out).map_err(err)?.len(),
        video_kbps: limits.video_kbps,
        part_removed: remove,
        truncated: job.out_len() > max + 0.05,
        max_seconds: limits.max_seconds,
    };
    Ok((out, result))
}

/// Импортированный готовый файл: исходных слоёв нет, поэтому запечённую «Часть» убрать нельзя.
/// Применяем тот же профиль и проверку файла, сохраняя весь исходный кадр с полями.
pub(crate) async fn prepare_ready(
    app: &AppHandle,
    ready: i64,
    path: &Path,
    p: PlatformId,
    publication: Option<i64>,
) -> Result<PathBuf, String> {
    let state = app.state::<AppState>();
    let platform_caps = caps(p)?;
    let _source_use = state.source_use.read().await;
    let (profile, project) = {
        let db = state.db.0.lock().unwrap();
        let project: i64 = db
            .query_row(
                "SELECT project_id FROM ready_videos WHERE id=?1 AND path=?2",
                rusqlite::params![ready, path.to_string_lossy()],
                |r| r.get(0),
            )
            .map_err(err)?;
        crate::factory::rights::require_project(&db, project)?;
        (publication_profile(&db, publication, p)?, project)
    };
    let info = crate::media::probe(path).await.map_err(err)?;
    let limits = budget(profile.limits, info.duration, platform_caps.file_max)?;
    let hash = hex::encode(Sha256::digest(
        serde_json::to_vec(&serde_json::json!([1, file_key(path)?, limits])).map_err(err)?,
    ));
    let key = format!("ready-{ready}-{}-{}", id(p), &hash[..20]);
    let gate = state
        .output_prepares
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .clone();
    let _turn = gate.lock().await;
    let out = state
        .data_dir
        .join("output-variants")
        .join(format!("{key}.mp4"));
    let max = limits
        .max_seconds
        .unwrap_or(info.duration)
        .min(info.duration);
    if out.is_file()
        && verified(&out, platform_caps, max, limits.max_seconds)
            .await
            .is_ok()
    {
        crate::factory::rights::require_project(&state.db.0.lock().unwrap(), project)?;
        return Ok(out);
    }
    let _permit = state.final_slots.clone().acquire_owned().await;
    tokio::fs::create_dir_all(out.parent().unwrap())
        .await
        .map_err(err)?;
    let part = out.with_extension("part.mp4");
    let mut args = vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        path.to_string_lossy().into_owned(),
    ];
    if !info.has_audio {
        args.extend(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"].map(str::to_string));
    }
    args.extend(["-map","0:v:0","-map",if info.has_audio {"0:a:0"} else {"1:a:0"},"-vf","scale=1080:1920:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1080:1920:(ow-iw)/2:(oh-ih)/2,setsar=1","-r","30","-c:v","libx264","-preset","fast","-threads","4","-pix_fmt","yuv420p","-c:a","aac","-ar","48000","-ac","2","-af","aresample=48000,asetpts=PTS-STARTPTS","-movflags","+faststart"].map(str::to_string));
    args.push(part.to_string_lossy().into_owned());
    let mut plan = crate::render::filter::Plan {
        args,
        duration: info.duration,
        filter: String::new(),
    };
    crate::render::ffmpeg::apply_limits(&mut plan, &limits).map_err(err)?;
    let result = crate::platform::command("ffmpeg")
        .args(plan.args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(err)?;
    if !result.status.success() {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!(
            "Не удалось подготовить готовый файл: {}",
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(1500)
                .collect::<String>()
        ));
    }
    if let Err(e) = verified(&part, platform_caps, max, limits.max_seconds).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, &out).await.map_err(err)?;
    crate::factory::rights::require_project(&state.db.0.lock().unwrap(), project)?;
    Ok(out)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn output_profiles(state: State<'_, AppState>) -> Result<Vec<OutputProfile>, String> {
    [
        PlatformId::Tiktok,
        PlatformId::Telegram,
        PlatformId::Youtube,
        PlatformId::Instagram,
    ]
    .into_iter()
    .map(|p| load(&state.db.0.lock().unwrap(), p))
    .collect()
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn output_profile_save(
    state: State<'_, AppState>,
    profile: OutputProfile,
) -> Result<OutputProfile, String> {
    save(&state.db.0.lock().unwrap(), profile)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn output_preview(
    app: AppHandle,
    draft_id: i64,
    platform: PlatformId,
) -> Result<Variant, String> {
    prepare(&app, draft_id, platform, None)
        .await
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db
    }
    #[test]
    fn size_budget_caps_bitrate_and_duration_without_mutating_stored_profile() {
        let p = default_profile(PlatformId::Telegram).unwrap();
        let b = budget(p.limits.clone(), 180.0, Some(50_000_000)).unwrap();
        assert_eq!(b.video_kbps, 1960);
        assert_eq!(p.limits.video_kbps, 6000);
        let b = budget(
            OutputLimits {
                max_seconds: Some(10.0),
                ..p.limits.clone()
            },
            180.0,
            Some(50_000_000),
        )
        .unwrap();
        assert_eq!(b.video_kbps, 6000);
        assert!(budget(p.limits, 10000.0, Some(50_000_000)).is_err());
    }
    #[test]
    fn slot_profile_is_frozen_and_invalid_changes_leave_previous_settings() {
        let db = db();
        let p = default_profile(PlatformId::Youtube).unwrap();
        freeze(&db, 42, p.platform).unwrap();
        let new = OutputProfile {
            limits: OutputLimits {
                max_seconds: Some(60.0),
                video_kbps: 3000,
                audio_kbps: 96,
            },
            ..p.clone()
        };
        save(&db, new).unwrap();
        assert_eq!(load(&db, p.platform).unwrap().limits.video_kbps, 3000);
        assert_eq!(
            publication_profile(&db, Some(42), p.platform)
                .unwrap()
                .limits,
            p.limits
        );
        assert!(save(
            &db,
            OutputProfile {
                limits: OutputLimits {
                    max_seconds: None,
                    ..p.limits.clone()
                },
                ..p.clone()
            }
        )
        .is_err());
        assert!(save(
            &db,
            OutputProfile {
                limits: OutputLimits {
                    video_kbps: 1,
                    ..p.limits.clone()
                },
                ..p.clone()
            }
        )
        .is_err());
        assert_eq!(load(&db, p.platform).unwrap().limits.video_kbps, 3000);
        assert!(publication_profile(&db, Some(42), PlatformId::Telegram).is_err());
    }
    #[test]
    fn multiple_enabled_accounts_request_part_removal() {
        let db = db();
        db.execute_batch("INSERT INTO accounts(label,platform,external_ref)VALUES('TG1','telegram','@one'),('TG2','telegram','@two')").unwrap();
        assert!(remove_part(&db, 1, PlatformId::Telegram).unwrap());
        db.execute(
            "UPDATE accounts SET enabled=0 WHERE external_ref='@two'",
            [],
        )
        .unwrap();
        assert!(!remove_part(&db, 1, PlatformId::Telegram).unwrap());
    }
}
