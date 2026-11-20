//! Калибровка камеры на локальном исходнике, без загрузки моделей или изменения видео.
use crate::host::Emitter;
use crate::host::State;
use crate::state::AppState;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub id: String,
    pub frames: usize,
    pub total: usize,
    pub stage: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub check: crate::analyze::face::detector::Check,
    pub suggested: Option<crate::analyze::face::Rect>,
}

struct ScanGuard<'a> {
    state: &'a AppState,
    id: String,
}
impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        let mut active = self.state.camera_scan.lock().unwrap();
        if active.as_ref().is_some_and(|(id, _)| id == &self.id) {
            *active = None;
        }
    }
}

/// Предлагает медианную рамку по всей записи. Изменение канала требует принятия в UI.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn camera_scan(
    app: crate::host::AppHandle,
    state: State<'_, AppState>,
    project_id: i64,
    id: String,
    camera: crate::analyze::face::Rect,
) -> Result<ScanResult, String> {
    let _source_use = state.source_use.read().await;
    camera.validate()?;
    if id.is_empty() || id.len() > 128 {
        return Err("Неверный идентификатор проверки".into());
    }
    if !crate::model_download::face_ready(&state.models_dir()) {
        return Err("Для проверки записи загрузи YuNet".into());
    }
    let path: String = {
        let db = state.db.0.lock().unwrap();
        db.query_row("SELECT path FROM projects WHERE id=?1", [project_id], |r| {
            r.get(0)
        })
        .map_err(|_| "Выбери импортированную запись")?
    };
    let (tx, mut cancel) = tokio::sync::watch::channel(false);
    {
        let mut active = state.camera_scan.lock().unwrap();
        if active.is_some() {
            return Err(
                "Проверка камеры уже выполняется. Дождись результата или останови её".into(),
            );
        }
        *active = Some((id.clone(), tx));
    }
    let _guard = ScanGuard {
        state: &state,
        id: id.clone(),
    };
    let info = crate::media::probe(Path::new(&path))
        .await
        .map_err(|e| e.to_string())?;
    let total = (info.duration * 2.0).floor() as usize;
    let report = |frames, stage: &str| {
        let _ = app.emit(
            "camera-scan-progress",
            ScanProgress {
                id: id.clone(),
                frames,
                total,
                stage: stage.into(),
            },
        );
    };
    report(0, "waiting");
    let _ml = tokio::select! {
        permit=state.ml_gate.acquire()=>permit.map_err(|e|e.to_string())?,
        _=cancel.changed()=>return Err("Проверка камеры остановлена".into()),
    };
    if *cancel.borrow() {
        return Err("Проверка камеры остановлена".into());
    }
    report(0, "scanning");
    let check = crate::analyze::face::detector::scan_clip(
        Path::new(&path),
        &state.models_dir().join(crate::model_download::FACE_REL),
        0.0,
        info.duration,
        info.width,
        info.height,
        camera,
        cancel,
        |frames| {
            if frames % 2 == 0 {
                report(frames, "scanning");
            }
        },
    )
    .await
    .map_err(|e| format!("Камера: {e:#}"))?;
    report(check.frames, "done");
    let suggested = check.median.map(|r| r.expanded(1.8));
    Ok(ScanResult { check, suggested })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn camera_scan_cancel(state: State<'_, AppState>, id: String) {
    if let Some((active, tx)) = state.camera_scan.lock().unwrap().as_ref() {
        if active == &id {
            let _ = tx.send(true);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub at: f64,
}
static FRAME_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutFrame {
    pub url: String,
    pub face_present: bool,
    pub note: String,
}

/// Кадр проходит тот же filter::build, что черновик и финал, с текущими ассетами.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn layout_preview(
    state: State<'_, AppState>,
    channel_id: i64,
    project_id: i64,
    at: f64,
    template: crate::factory::profiles::Template,
) -> Result<LayoutFrame, String> {
    let _source_use = state.source_use.read().await;
    use crate::render::*;
    let _guard = FRAME_GATE.lock().await;
    let (source, camera, gp, banner, words) = {
        let db = state.db.0.lock().unwrap();
        let source: String = db
            .query_row("SELECT path FROM projects WHERE id=?1", [project_id], |r| {
                r.get(0)
            })
            .map_err(|_| "Выбери импортированную запись")?;
        let channel = crate::factory::profiles::channels(&db)?
            .into_iter()
            .find(|c| c.id == channel_id)
            .ok_or("Выбери канал с профилем камеры")?;
        let camera = channel
            .cam_rect
            .and_then(|v| serde_json::from_value::<crate::analyze::face::Rect>(v).ok())
            .and_then(|r| r.validate().ok());
        let gp = template
            .config
            .background_ids
            .first()
            .and_then(|id| crate::assets_cmds::load_asset(&db, *id));
        let banner = template
            .config
            .banner_id
            .and_then(|id| crate::assets_cmds::load_asset(&db, id));
        let words: Vec<Word> = db
            .query_row(
                "SELECT words_json FROM analysis WHERE project_id=?1",
                [project_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        (source, camera, gp, banner, words)
    };
    let info = crate::media::probe(Path::new(&source))
        .await
        .map_err(|e| e.to_string())?;
    if !at.is_finite() || at < 0.0 || at + 0.5 >= info.duration {
        return Err("Выбери кадр не ближе 0,5 с к концу записи".into());
    }
    let mut st = template.config.style.clone();
    st.layout = match template.layout.as_str() {
        "reaction_pip" => Layout::ReactionPip,
        "reaction_stack" => Layout::ReactionStack,
        "gameplay_stack" => Layout::GameplayStack,
        _ => return Err("Неизвестная раскладка".into()),
    };
    // Превью не воспроизводит и не смешивает звук, но использует тот же видеограф.
    st.audio.main = 0.0;
    st.audio.gameplay = 0.0;
    st.audio.banner = 0.0;
    st.audio.music = 0.0;
    let mut cam_rect = None;
    let mut note = String::new();
    if st.layout != Layout::GameplayStack {
        if let Some(camera) = camera {
            if crate::model_download::face_ready(&state.models_dir()) {
                let _ml = state.ml_gate.acquire().await.map_err(|e| e.to_string())?;
                let frame = crate::analyze::face::detector::read_frame(Path::new(&source), at)
                    .await
                    .map_err(|e| e.to_string())?;
                let model = state.models_dir().join(crate::model_download::FACE_REL);
                let (w, h) = (info.width, info.height);
                let faces = tokio::task::spawn_blocking(move || {
                    crate::analyze::face::detector::Detector::load(&model)?.detect(&frame, w, h)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                if crate::analyze::face::detector::within_camera(&faces, camera).is_some() {
                    cam_rect = Some(camera);
                } else {
                    note = "На этом кадре лицо в области камеры не найдено".into();
                }
            } else {
                note = "YuNet не загружена; камера в превью скрыта".into();
            }
        } else {
            note = "Настрой область камеры в профиле канала".into();
        }
    }
    let make = |a: crate::assets_cmds::AssetDto| MediaRef {
        path: a.path.into(),
        duration: a.meta.duration,
        has_audio: false,
    };
    let needs_gp = st.layout == Layout::GameplayStack
        || (st.layout == Layout::ReactionPip && st.reaction.background);
    let gameplay = match gp {
        Some(a) => make(a),
        None if !needs_gp => MediaRef {
            path: source.clone().into(),
            duration: info.duration,
            has_audio: false,
        },
        None => return Err("Выбери геймплей для шаблона".into()),
    };
    let dir = state.data_dir.join("layout-previews");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let target = dir.join(format!("template-{}-{}.png", template.id, nonce));
    let banner = banner.map(make);
    let job = RenderJob {
        cam_rect,
        decode: Decode::Cpu,
        source: MediaRef {
            path: source.into(),
            duration: info.duration,
            has_audio: false,
        },
        source_aspect: info.width as f64 / info.height as f64,
        start: at,
        end: (at + 1.0).min(info.duration),
        banner_at: banner.as_ref().map(|_| 0.2),
        banner,
        gameplay,
        gameplay_offset: crate::drafts::GAMEPLAY_START,
        music: None,
        music_offset: 0.0,
        words,
        part: Some(1),
        hook: None,
        style: st,
        profile: Profile::Draft,
        encoder: Encoder::X264,
        out: target.clone(),
    };
    let ass = target.with_extension("ass");
    tokio::fs::write(&ass, crate::render::ass::build(&job))
        .await
        .map_err(|e| e.to_string())?;
    let mut args = crate::render::filter::build(&job, Some(&ass), Some(&state.fonts_dir())).args;
    let last = args
        .iter()
        .position(|v| v == "-filter_complex")
        .ok_or("Не собран видеограф")?
        + 2;
    args.truncate(last);
    args.extend([
        "-map".into(),
        "[vout]".into(),
        "-ss".into(),
        "0.4".into(),
        "-frames:v".into(),
        "1".into(),
        "-threads".into(),
        "1".into(),
        "-update".into(),
        "1".into(),
        target.to_string_lossy().into_owned(),
    ]);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        crate::platform::command("ffmpeg")
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Превью не готово за 45 секунд")?
    .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(ass).await;
    if !output.status.success() || !target.is_file() {
        return Err(format!(
            "Превью: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if template.id > 0 {
        let old = {
            let db = state.db.0.lock().unwrap();
            let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
            let key = format!("layout_preview:{}", template.id);
            let old = crate::drafts::kv_get(&tx, &key);
            crate::drafts::kv_set(&tx, &key, &target.to_string_lossy())
                .map_err(|e| e.to_string())?;
            crate::drafts::kv_set(
                &tx,
                &format!("layout_preview_config:{}", template.id),
                &crate::factory::profiles::preview_config(&template),
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            old
        };
        if let Some(old) = old {
            let old = std::path::PathBuf::from(old);
            if old.starts_with(&dir) && old != target {
                let _ = tokio::fs::remove_file(old).await;
            }
        }
    }
    Ok(LayoutFrame {
        url: state.media.register(target),
        face_present: cam_rect.is_some(),
        note,
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn face_model_ready(state: State<'_, AppState>) -> bool {
    crate::model_download::face_ready(&state.models_dir())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn camera_detect(
    state: State<'_, AppState>,
    project_id: i64,
    at: f64,
) -> Result<Vec<crate::analyze::face::detector::Detection>, String> {
    let _source_use = state.source_use.read().await;
    let (path, duration, width, height): (String, f64, u32, u32) = {
        let db = state.db.0.lock().unwrap();
        db.query_row(
            "SELECT path,duration,width,height FROM projects WHERE id=?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| "Выбери импортированную запись".to_string())?
    };
    if !at.is_finite() || at < 0.0 || at >= duration {
        return Err("Время кадра за пределами записи".into());
    }
    let model = state.models_dir().join(crate::model_download::FACE_REL);
    if !crate::model_download::face_ready(&state.models_dir()) {
        return Err("Для поиска лица сначала загрузи YuNet (230 КБ)".into());
    }
    let _ml = state.ml_gate.acquire().await.map_err(|e| e.to_string())?;
    let frame = crate::analyze::face::detector::read_frame(Path::new(&path), at)
        .await
        .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        crate::analyze::face::detector::Detector::load(&model)?.detect(&frame, width, height)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("YuNet: {e:#}"))
}

pub async fn extract_frame(source: &Path, target: &Path, at: f64) -> Result<(), String> {
    if !at.is_finite() || at < 0.0 {
        return Err("Время кадра должно быть неотрицательным".into());
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::platform::command("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-ss",
                &format!("{at:.3}"),
                "-i",
            ])
            .arg(source)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=w='min(1280,iw)':h=-2",
                "-threads",
                "1",
                "-q:v",
                "3",
            ])
            .arg(target)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Не удалось получить кадр за 30 секунд".to_string())?
    .map_err(|e| e.to_string())?;
    if !output.status.success() || !target.is_file() {
        return Err(format!(
            "Не удалось прочитать кадр: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn camera_frame(
    state: State<'_, AppState>,
    project_id: i64,
    at: f64,
) -> Result<Frame, String> {
    let _source_use = state.source_use.read().await;
    let _guard = FRAME_GATE.lock().await;
    let (path, duration, width, height): (String, f64, u32, u32) = {
        let db = state.db.0.lock().unwrap();
        db.query_row(
            "SELECT path,duration,width,height FROM projects WHERE id=?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| "Выбери импортированный проект".to_string())?
    };
    if !at.is_finite() || at < 0.0 || at >= duration {
        return Err("Время кадра за пределами записи".into());
    }
    let dir = state.data_dir.join("camera-previews");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = dir.join(format!(".{project_id}-{nonce}.jpg"));
    let target = dir.join(format!("{project_id}.jpg"));
    if let Err(e) = extract_frame(Path::new(&path), &temp, at).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(e);
    }
    // Windows не заменяет существующий файл через rename; удаляется лишь наш прежний preview.
    if target.exists() {
        tokio::fs::remove_file(&target)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(&temp, &target)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("{}&v={nonce}", state.media.register(target));
    Ok(Frame {
        url,
        width,
        height,
        duration,
        at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_frame_is_read_without_touching_source() {
        let dir = std::env::temp_dir().join(format!("avto-camera-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mkv");
        let target = dir.join("frame.jpg");
        assert!(std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=160x90:d=2:r=2",
                "-c:v",
                "ffv1"
            ])
            .arg(&source)
            .status()
            .unwrap()
            .success());
        let before = std::fs::read(&source).unwrap();
        extract_frame(&source, &target, 0.5).await.unwrap();
        assert!(std::fs::metadata(target).unwrap().len() > 100);
        assert_eq!(before, std::fs::read(&source).unwrap());
        assert!(extract_frame(&source, &dir.join("bad.jpg"), f64::NAN)
            .await
            .is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
