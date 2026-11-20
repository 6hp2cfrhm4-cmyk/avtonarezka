//! Прокси анализируется на исходной шкале времени; рендер берёт только выбранную секцию 1080p.
use super::{CmdResult, NewItem};
use crate::host::{AppHandle, Emitter, Manager};
use crate::{
    commands::{self, Project},
    sources::{self, DownloadMode, Source, SourceItem, Window, YtDlpSource},
    state::AppState,
};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use tokio::sync::watch;

fn record(
    db: &Connection,
    path: &Path,
    item: i64,
    project: Option<i64>,
    channel: Option<i64>,
    kind: &str,
) -> CmdResult<()> {
    db.execute("INSERT INTO source_files(path,item_id,project_id,channel_id,kind) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(path) DO UPDATE SET project_id=COALESCE(excluded.project_id,source_files.project_id)",params![path.to_string_lossy(),item,project,channel,kind]).map_err(|e|e.to_string())?;
    Ok(())
}
pub(crate) async fn download(app: &AppHandle, id: i64, item: &NewItem) -> CmdResult<Project> {
    let state = app.state::<AppState>();
    let _source_use = state.source_use.read().await;
    let source = sources::classify_link(&item.source)?;
    let snapshot = super::profile(&state.db.0.lock().unwrap(), id);
    let options = snapshot
        .as_ref()
        .map(|s| s.channel.monitor.clone())
        .unwrap_or_default();
    // Ручная Студия позволяет произвольные границы ещё до рендера: получает полный исходник.
    let proxy = item.channel_id.is_some()
        && options.two_pass
        && snapshot.as_ref().is_none_or(|s| s.channel.mode != "manual");
    let src = SourceItem {
        external_id: id.to_string(),
        source: source.url,
        title: item.title.clone(),
        duration: None,
        published_at: None,
    };
    let dir = state
        .data_dir
        .join("sources")
        .join(id.to_string())
        .join(if proxy { "proxy" } else { "original" });
    let (_tx, rx) = watch::channel(false);
    let _slot = state
        .youtube_slots
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    let downloaded = YtDlpSource::detected(&source.kind)?
        .download(
            &src,
            if proxy {
                DownloadMode::Proxy
            } else {
                DownloadMode::Full
            },
            &options,
            &dir,
            rx,
        )
        .await?;
    record(
        &state.db.0.lock().unwrap(),
        &downloaded.path,
        id,
        None,
        item.channel_id,
        if proxy { "proxy" } else { "original" },
    )?;
    commands::import_video(app.state(), downloaded.path.to_string_lossy().into_owned()).await
}

pub(crate) fn proxy_project(db: &Connection, project: i64) -> bool {
    db.query_row("SELECT EXISTS(SELECT 1 FROM projects p JOIN source_files f ON f.path=p.path WHERE p.id=?1 AND f.kind='proxy')",[project],|r|r.get(0)).unwrap_or(false)
}

/// Вызывается и при создании черновика, и после ручной правки границ, и у варианта под площадку.
pub(crate) async fn prepare_draft(
    app: &AppHandle,
    draft: i64,
    cancel: watch::Receiver<bool>,
) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let d = crate::drafts::get(&state.db.0.lock().unwrap(), draft)?;
    if !proxy_project(&state.db.0.lock().unwrap(), d.project_id) {
        return Ok(());
    }
    let gate = state
        .source_prepares
        .lock()
        .unwrap()
        .entry(d.project_id)
        .or_default()
        .clone();
    let _guard = gate.lock().await;
    // Другой черновик мог уже скачать полный fallback, пока этот ждал.
    if !proxy_project(&state.db.0.lock().unwrap(), d.project_id) {
        return Ok(());
    }
    if segment(&state.db.0.lock().unwrap(), d.project_id, d.start, d.end)?.is_some() {
        return Ok(());
    }
    let (id, url, channel, duration): (i64, String, Option<i64>, f64) = {
        let db = state.db.0.lock().unwrap();
        db.query_row("SELECT f.id,f.source,f.channel_id,p.duration FROM factory_items f JOIN projects p ON p.id=f.project_id WHERE f.project_id=?1 ORDER BY f.id DESC LIMIT 1",[d.project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e|e.to_string())?
    };
    let snap = super::profile(&state.db.0.lock().unwrap(), id)
        .ok_or("Нет сохранённого профиля источника")?;
    let source = sources::classify_link(&url)?;
    let driver = YtDlpSource::detected(&source.kind)?;
    let src = SourceItem {
        external_id: id.to_string(),
        source: source.url,
        title: d.title.clone(),
        duration: Some(duration),
        published_at: None,
    };
    // С небольшим запасом для ручной доводки. Точное обрезание остаётся прежнему ffmpeg renderer.
    let window = Window {
        start: (d.start - 2.0).max(0.0),
        end: (d.end + 2.0).min(duration),
    };
    let dir = state
        .data_dir
        .join("sources")
        .join(id.to_string())
        .join(format!(
            "section-{:.0}-{:.0}",
            window.start * 1000.0,
            window.end * 1000.0
        ));
    let _slot = state
        .youtube_slots
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    let mut file = driver
        .download(
            &src,
            DownloadMode::Section(window),
            &snap.channel.monitor,
            &dir,
            cancel.clone(),
        )
        .await?;
    let mut info = crate::media::probe(&file.path)
        .await
        .map_err(|e| e.to_string())?;
    if !file.fallback_full && info.duration + 0.25 < window.end - window.start {
        file = driver
            .download(
                &src,
                DownloadMode::Full,
                &snap.channel.monitor,
                &dir.join("fallback-short"),
                cancel,
            )
            .await?;
        file.fallback_full = true;
        info = crate::media::probe(&file.path)
            .await
            .map_err(|e| e.to_string())?;
    }
    if file.fallback_full && info.duration + 0.25 < d.end {
        return Err("Полный исходник короче выбранного момента".into());
    }
    let db = state.db.0.lock().unwrap();
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    record(
        &tx,
        &file.path,
        id,
        Some(d.project_id),
        channel,
        if file.fallback_full {
            "original"
        } else {
            "section"
        },
    )?;
    if file.fallback_full {
        tx.execute(
            "UPDATE projects SET path=?2,width=?3,height=?4,has_audio=?5 WHERE id=?1",
            params![
                d.project_id,
                file.path.to_string_lossy(),
                info.width,
                info.height,
                info.has_audio
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO events(item_id,channel_id,kind,message) VALUES(?1,?2,'source','Источник не выдал секцию: скачан полный файл 1080p')",params![id,channel]).map_err(|e|e.to_string())?;
    } else {
        tx.execute("INSERT INTO source_segments(project_id,start,\"end\",path,duration,width,height,has_audio) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(project_id,start,\"end\") DO UPDATE SET path=excluded.path,duration=excluded.duration,width=excluded.width,height=excluded.height,has_audio=excluded.has_audio",params![d.project_id,file.offset,window.end,file.path.to_string_lossy(),info.duration,info.width,info.height,info.has_audio]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    let _ = app.emit("factory-changed", ());
    Ok(())
}

pub(crate) struct Segment {
    pub path: PathBuf,
    pub offset: f64,
    pub duration: f64,
    pub width: f64,
    pub height: f64,
    pub has_audio: bool,
}
pub(crate) fn segment(
    db: &Connection,
    project: i64,
    start: f64,
    end: f64,
) -> CmdResult<Option<Segment>> {
    let mut st=db.prepare("SELECT path,start,duration,width,height,has_audio FROM source_segments WHERE project_id=?1 AND start<=?2+0.001 AND \"end\">=?3-0.001 ORDER BY \"end\"-start").map_err(|e|e.to_string())?;
    let rows = st
        .query_map(params![project, start, end], |r| {
            Ok(Segment {
                path: PathBuf::from(r.get::<_, String>(0)?),
                offset: r.get(1)?,
                duration: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                has_audio: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    for r in rows {
        let segment = r.map_err(|e| e.to_string())?;
        if segment.path.is_file() {
            return Ok(Some(segment));
        }
    }
    Ok(None)
}
