//! Библиотека ассетов: баннеры (видео на зелёном фоне), фоны (геймплей), музыка.
//! Файлы остаются где лежат, в базе хранятся путь и сведения о них.

use std::path::PathBuf;

use crate::host::State;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::{media, state::AppState};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];
const AUDIO_EXT: &[&str] = &["mp3", "wav", "m4a", "aac", "ogg", "flac", "opus"];

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Meta {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    pub thumb: Option<String>,
    pub alpha: bool,
    pub green_corners: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AssetDto {
    pub id: i64,
    pub kind: String,
    pub path: String,
    pub name: String,
    #[serde(flatten)]
    pub meta: Meta,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<AssetDto> {
    Ok(AssetDto {
        id: r.get(0)?,
        kind: r.get(1)?,
        path: r.get(2)?,
        name: r.get(3)?,
        meta: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
    })
}

const COLS: &str = "id, kind, path, name, meta_json";

pub(crate) fn load_asset(db: &Connection, id: i64) -> Option<AssetDto> {
    db.query_row(
        &format!("SELECT {COLS} FROM assets WHERE id = ?1"),
        [id],
        from_row,
    )
    .ok()
}

pub(crate) fn list_assets(db: &Connection) -> rusqlite::Result<Vec<AssetDto>> {
    let mut st = db.prepare(&format!(
        "SELECT {COLS} FROM assets ORDER BY kind, created_at DESC, id DESC"
    ))?;
    let rows = st.query_map([], from_row)?;
    rows.collect()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn assets_list(state: State<'_, AppState>) -> CmdResult<Vec<AssetDto>> {
    list_assets(&state.db.0.lock().unwrap()).map_err(err)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn asset_add(
    state: State<'_, AppState>,
    kind: String,
    path: String,
) -> CmdResult<AssetDto> {
    add(&state, &kind, &path).await
}

pub(crate) async fn add(state: &AppState, kind: &str, path: &str) -> CmdResult<AssetDto> {
    if !["banner", "background", "music"].contains(&kind) {
        return Err(format!("неизвестный вид ассета: {kind}"));
    }
    let p = PathBuf::from(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let allowed = if kind == "music" {
        AUDIO_EXT
    } else if kind == "banner" && ext == "png" {
        &["png"]
    } else {
        VIDEO_EXT
    };
    if !p.is_file() || !allowed.contains(&ext.as_str()) {
        return Err(if kind == "music" {
            "Для музыки подойдут mp3, wav, m4a, ogg, flac".into()
        } else {
            if kind == "banner" {
                "Для баннера подойдут mp4, mov, mkv, webm или PNG".into()
            } else {
                "Это не видеофайл: подойдут mp4, mov, mkv, webm".into()
            }
        });
    }

    let meta = if kind == "music" {
        Meta {
            duration: media::probe_audio(&p).await.map_err(err)?,
            has_audio: true,
            ..Default::default()
        }
    } else if kind == "banner" {
        inspect_banner(&p, &state.data_dir).await?
    } else {
        let info = media::probe(&p).await.map_err(err)?;
        let hash = media::quick_hash(&p).await.map_err(err)?;
        let thumb = state.data_dir.join("thumbs").join(format!("{hash}.jpg"));
        // кадр с баннером лучше брать ближе к середине, где он уже не прозрачный
        let at = if kind == "banner" {
            info.duration * 0.4
        } else {
            info.duration.min(10.0) * 0.3
        };
        let thumb = media::make_thumb(&p, &thumb, at)
            .await
            .ok()
            .map(|_| thumb.display().to_string());
        Meta {
            duration: info.duration,
            width: info.width,
            height: info.height,
            has_audio: info.has_audio,
            thumb,
            ..Default::default()
        }
    };

    let name = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Без названия".into());
    let db = state.db.0.lock().unwrap();
    db.execute(
        "INSERT INTO assets (kind, path, name, meta_json) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET kind = excluded.kind, name = excluded.name, meta_json = excluded.meta_json",
        params![kind, path, name, serde_json::to_string(&meta).unwrap_or_default()],
    )
    .map_err(err)?;
    db.query_row(
        &format!("SELECT {COLS} FROM assets WHERE path = ?1"),
        [path],
        from_row,
    )
    .map_err(err)
}

/// Проверяем длительность, прозрачность и зелёные углы на настоящем кадре.
pub(crate) async fn inspect_banner(
    path: &std::path::Path,
    data: &std::path::Path,
) -> CmdResult<Meta> {
    let probe = crate::platform::command("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(err)?;
    if !probe.status.success() {
        return Err("Не удалось прочитать баннер через ffprobe".into());
    }
    let json: serde_json::Value = serde_json::from_slice(&probe.stdout).map_err(err)?;
    let streams = json["streams"]
        .as_array()
        .ok_or("Баннер не содержит дорожек")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or("Баннер не содержит видео или изображения")?;
    let png = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let duration = if png {
        6.0
    } else {
        json["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("Не удалось определить длительность баннера")?
    };
    if !duration.is_finite() || duration <= 0.0 || duration > 300.0 {
        return Err("Баннер должен длиться от 0 до 300 секунд".into());
    }
    let alpha_tag = video["tags"]["alpha_mode"].as_str() == Some("1")
        || video["tags"]["ALPHA_MODE"].as_str() == Some("1");
    if alpha_tag && video["codec_name"] == "vp8" {
        return Err("Для прозрачного WebM используй VP9. Также поддерживается PNG с альфой".into());
    }
    let mut command = crate::platform::command("ffmpeg");
    command.args(["-v", "error", "-threads", "1"]);
    if alpha_tag {
        command.args([
            "-c:v",
            if video["codec_name"] == "vp8" {
                "libvpx"
            } else {
                "libvpx-vp9"
            },
        ]);
    }
    if !png {
        command.args(["-ss", &format!("{:.3}", duration * 0.4)]);
    }
    let frame = command
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=64:64,format=rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(err)?;
    if !frame.status.success() || frame.stdout.len() != 64 * 64 * 4 {
        return Err("Не удалось проверить кадр баннера".into());
    }
    let mut green = 0;
    let mut corners = 0;
    for y in 0..64 {
        for x in 0..64 {
            if !(8..56).contains(&x) && !(8..56).contains(&y) {
                let p = &frame.stdout[(y * 64 + x) * 4..][..4];
                corners += 1;
                if p[3] > 128
                    && (p[1] as f64) > (p[0] as f64) * 1.3
                    && (p[1] as f64) > (p[2] as f64) * 1.3
                    && p[1] > 100
                {
                    green += 1;
                }
            }
        }
    }
    let alpha = alpha_tag || frame.stdout.chunks_exact(4).any(|p| p[3] < 250);
    let hash = media::quick_hash(path).await.map_err(err)?;
    let thumb = data.join("thumbs").join(format!("{hash}.jpg"));
    let thumb = media::make_thumb(path, &thumb, if png { 0.0 } else { duration * 0.4 })
        .await
        .ok()
        .map(|_| thumb.display().to_string());
    Ok(Meta {
        duration,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
        thumb,
        alpha,
        green_corners: Some(green as f64 / corners as f64),
    })
}

/// Убирает ассет из библиотеки (файл на диске остаётся).
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn asset_delete(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state
        .db
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM assets WHERE id = ?1", [id])
        .map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn banner_probe_reads_green_corners_alpha_png_and_vp9() {
        let dir = std::env::temp_dir().join(format!("avto-banner-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("alpha.png");
        let webm = dir.join("alpha.webm");
        let chroma = dir.join("key.mp4");
        let run = |args: &[&str]| {
            assert!(std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args(args)
                .status()
                .unwrap()
                .success())
        };
        run(&["-f","lavfi","-i","nullsrc=s=128x96,format=rgba,geq=r=255:g=0:b=0:a='if(between(X,40,88)*between(Y,30,66),255,0)'","-frames:v","1","-threads","1",png.to_str().unwrap()]);
        run(&[
            "-loop",
            "1",
            "-i",
            png.to_str().unwrap(),
            "-t",
            "1",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-auto-alt-ref",
            "0",
            "-threads",
            "1",
            webm.to_str().unwrap(),
        ]);
        run(&[
            "-f",
            "lavfi",
            "-i",
            "color=green:s=128x96:d=1",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            chroma.to_str().unwrap(),
        ]);
        for p in [&png, &webm] {
            let meta = inspect_banner(p, &dir).await.unwrap();
            assert!(meta.alpha);
            assert!(meta.green_corners.unwrap() < 0.1);
            assert!(meta.thumb.is_some());
            assert_eq!((meta.width, meta.height), (128, 96));
        }
        let key = inspect_banner(&chroma, &dir).await.unwrap();
        assert!(!key.alpha);
        assert!(key.green_corners.unwrap() > 0.9);
        assert!((key.duration - 1.0).abs() < 0.1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn meta_serialises_flat_and_camel_cased() {
        let a = AssetDto {
            id: 1,
            kind: "banner".into(),
            path: "/b.mp4".into(),
            name: "b".into(),
            meta: Meta {
                duration: 3.0,
                width: 640,
                height: 360,
                has_audio: true,
                thumb: None,
                ..Default::default()
            },
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["duration"], 3.0);
        assert_eq!(v["hasAudio"], true);
        assert!(v.get("meta").is_none());
    }

    #[test]
    fn broken_meta_does_not_break_listing() {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute("INSERT INTO assets (kind, path, name, meta_json) VALUES ('music', '/m.mp3', 'm', 'не json')", []).unwrap();
        let list = list_assets(&c).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].meta.duration, 0.0);
    }
}
