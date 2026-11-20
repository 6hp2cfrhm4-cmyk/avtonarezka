//! Упаковка задачи для передачи на другой узел и приём такой упаковки.
//!
//! Передаётся задача, которая ещё не начата (`queued`, `fetching`): источник, снимок профиля канала и шаблона,
//! баннеры/геймплей/музыка, на которые ссылается снимок, и кампании. Идентификаторы ресурсов на другом узле
//! другие, поэтому снимок профиля при приёме переписывается (`remap_profile`). Приём идемпотентен по `pack_id`:
//! повторная отправка той же упаковки ничего не удваивает.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Суммарный потолок комплекта ресурсов и исходного файла одной упаковки (место на сервере ограничено, G4).
pub const MAX_PACK_BYTES: u64 = 1_500_000_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BlobRef {
    pub sha256: String,
    pub size: u64,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PackAsset {
    pub pc_id: i64,
    pub kind: String,
    pub name: String,
    pub meta_json: String,
    pub blob: BlobRef,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PackCampaign {
    pub pc_id: i64,
    pub name: String,
    pub advertiser: String,
    pub config: String,
    pub enabled: bool,
    /// Идентификаторы баннеров на исходном узле (из `assets`).
    pub banners: Vec<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pack {
    pub pack_id: String,
    pub kind: String,
    pub source: String,
    pub title: String,
    pub preset: String,
    pub precut: bool,
    pub priority: i64,
    /// Для источника-файла: сам файл (путь на принимающем узле подставляется при приёме).
    pub source_blob: Option<BlobRef>,
    pub profile_json: String,
    pub assets: Vec<PackAsset>,
    pub campaigns: Vec<PackCampaign>,
}

impl Pack {
    /// Все файлы, которые должны лежать на приёмной стороне до `submit`.
    pub fn blobs(&self) -> Vec<BlobRef> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for b in self
            .source_blob
            .iter()
            .chain(self.assets.iter().map(|a| &a.blob))
        {
            if seen.insert(b.sha256.clone()) {
                out.push(b.clone());
            }
        }
        out
    }
    pub fn total_bytes(&self) -> u64 {
        self.blobs().iter().map(|b| b.size).sum()
    }
}

/// Можно ли передать задачу сейчас. `Err` — причина по-русски для окна.
pub fn movability(stage: &str, owner: &str, paused: bool) -> Result<(), String> {
    if owner != "local" {
        return Err("Уже передана на другой узел".into());
    }
    if paused {
        return Err("Стоит на паузе".into());
    }
    match stage {
        "queued" | "fetching" => Ok(()),
        "scouting" | "selecting" => Err("Идёт анализ: дождитесь конца или остановите".into()),
        "awaiting_selection" | "awaiting_approval" => {
            Err("Ждёт вашего решения, это решение принимается на ПК".into())
        }
        "completed" | "failed" | "cancelled" | "needs_check" => Err("Уже завершена".into()),
        _ => Err("Эта стадия нельзя переносить посреди работы, остаётся на ПК".into()),
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(h.finalize()), total))
}

pub fn blob_path(blob_dir: &Path, sha: &str) -> PathBuf {
    blob_dir.join(sha)
}

fn safe_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let n = n.trim_matches('.').to_string();
    if n.is_empty() {
        "file".into()
    } else {
        n.chars().take(80).collect()
    }
}

// ------------------------------------------------------------------ профиль

#[derive(Debug, Default, PartialEq)]
pub struct Refs {
    pub assets: BTreeSet<i64>,
    pub campaigns: BTreeSet<i64>,
}

/// На какие ресурсы и кампании ссылается снимок профиля.
pub fn profile_refs(profile: &Value) -> Refs {
    let mut r = Refs::default();
    let cfg = &profile["template"]["config"];
    if let Some(id) = cfg["bannerId"].as_i64() {
        r.assets.insert(id);
    }
    if let Some(id) = cfg["musicId"].as_i64() {
        r.assets.insert(id);
    }
    for id in cfg["backgroundIds"].as_array().into_iter().flatten() {
        if let Some(id) = id.as_i64() {
            r.assets.insert(id);
        }
    }
    for id in profile["channel"]["campaignIds"].as_array().into_iter().flatten() {
        if let Some(id) = id.as_i64() {
            r.campaigns.insert(id);
        }
    }
    r
}

/// Переписывает снимок под принимающий узел: чужие ресурсы заменяются своими, аккаунты публикации
/// убираются (сервер в этой версии только готовит ролики, публикует ПК), режим канала — `auto`,
/// иначе серверная задача ждала бы одобрения, которое некому дать.
pub fn remap_profile(
    profile: &mut Value,
    assets: &BTreeMap<i64, i64>,
    campaigns: &BTreeMap<i64, i64>,
) {
    let map = |v: &Value, m: &BTreeMap<i64, i64>| v.as_i64().and_then(|id| m.get(&id).copied());
    let cfg = &mut profile["template"]["config"];
    for key in ["bannerId", "musicId"] {
        let new = map(&cfg[key], assets);
        cfg[key] = new.map_or(Value::Null, |v| json!(v));
    }
    if let Some(list) = cfg["backgroundIds"].as_array() {
        let mapped: Vec<i64> = list.iter().filter_map(|v| map(v, assets)).collect();
        cfg["backgroundIds"] = json!(mapped);
    }
    let ch = &mut profile["channel"];
    if let Some(list) = ch["campaignIds"].as_array() {
        let mapped: Vec<i64> = list.iter().filter_map(|v| map(v, campaigns)).collect();
        ch["campaignIds"] = json!(mapped);
    }
    ch["targets"] = json!([]);
    ch["mode"] = json!("auto");
    ch["id"] = json!(0);
    ch["templateId"] = Value::Null;
    ch["lastCheckedAt"] = Value::Null;
    ch["nextCheckAt"] = Value::Null;
}

// ------------------------------------------------------------------ сборка (исходный узел)

struct Row {
    kind: String,
    source: String,
    title: String,
    preset: String,
    precut: bool,
    priority: i64,
    stage: String,
    owner: String,
    paused: bool,
    profile: Option<String>,
}

fn read_item(db: &Connection, id: i64) -> Result<Row, String> {
    db.query_row(
        "SELECT kind,source,title,preset,precut,priority,stage,owner_node,paused,profile_json FROM factory_items WHERE id=?1",
        [id],
        |r| {
            Ok(Row {
                kind: r.get(0)?,
                source: r.get(1)?,
                title: r.get(2)?,
                preset: r.get(3)?,
                precut: r.get::<_, i64>(4)? != 0,
                priority: r.get(5)?,
                stage: r.get(6)?,
                owner: r.get(7)?,
                paused: r.get::<_, i64>(8)? != 0,
                profile: r.get(9)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "Задача не найдена".to_string())
}

/// Собирает упаковку. Ничего не меняет в базе.
pub fn build(db: &Connection, item_id: i64) -> Result<Pack, String> {
    build_with_paths(db, item_id).map(|(p, _)| p)
}

/// То же, но ещё отдаёт пути файлов по хэшу: их нужно загрузить на принимающий узел.
pub fn build_with_paths(
    db: &Connection,
    item_id: i64,
) -> Result<(Pack, BTreeMap<String, PathBuf>), String> {
    let mut paths = BTreeMap::new();
    let row = read_item(db, item_id)?;
    movability(&row.stage, &row.owner, row.paused)?;
    let profile_json = match row.profile {
        Some(p) => p,
        None => crate::factory::profiles::legacy_snapshot(db)?,
    };
    let profile: Value = serde_json::from_str(&profile_json).map_err(|e| e.to_string())?;
    let refs = profile_refs(&profile);

    let mut assets = Vec::new();
    for id in &refs.assets {
        let (kind, path, name, meta): (String, String, String, String) = db
            .query_row(
                "SELECT kind,path,name,meta_json FROM assets WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(|_| format!("Ресурс {id} из шаблона не найден"))?;
        let (sha, size) = sha256_file(Path::new(&path))
            .map_err(|e| format!("Файл «{name}» не читается: {e}"))?;
        paths.insert(sha.clone(), PathBuf::from(&path));
        assets.push(PackAsset {
            pc_id: *id,
            kind,
            name: name.clone(),
            meta_json: meta,
            blob: BlobRef {
                sha256: sha,
                size,
                name: safe_name(&name),
            },
        });
    }
    let mut campaigns = Vec::new();
    for id in &refs.campaigns {
        let (name, advertiser, config, enabled): (String, String, String, bool) = db
            .query_row(
                "SELECT name,advertiser,config,enabled FROM campaigns WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0)),
            )
            .map_err(|_| format!("Кампания {id} не найдена"))?;
        let mut st = db
            .prepare("SELECT asset_id FROM campaign_banners WHERE campaign_id=?1 ORDER BY asset_id")
            .map_err(|e| e.to_string())?;
        let banners: Vec<i64> = st
            .query_map([id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        for b in &banners {
            if assets.iter().any(|a| a.pc_id == *b) {
                continue;
            }
            let (kind, path, name, meta): (String, String, String, String) = db
                .query_row(
                    "SELECT kind,path,name,meta_json FROM assets WHERE id=?1",
                    [b],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .map_err(|_| format!("Баннер {b} не найден"))?;
            let (sha, size) = sha256_file(Path::new(&path))
                .map_err(|e| format!("Баннер «{name}» не читается: {e}"))?;
            paths.insert(sha.clone(), PathBuf::from(&path));
            assets.push(PackAsset {
                pc_id: *b,
                kind,
                name: name.clone(),
                meta_json: meta,
                blob: BlobRef {
                    sha256: sha,
                    size,
                    name: safe_name(&name),
                },
            });
        }
        campaigns.push(PackCampaign {
            pc_id: *id,
            name,
            advertiser,
            config,
            enabled,
            banners,
        });
    }
    let source_blob = if row.kind == "file" {
        let p = Path::new(&row.source);
        let (sha, size) = sha256_file(p).map_err(|e| format!("Файл источника не читается: {e}"))?;
        paths.insert(sha.clone(), p.to_path_buf());
        Some(BlobRef {
            sha256: sha,
            size,
            name: safe_name(&p.file_name().map_or("source".into(), |n| n.to_string_lossy().into_owned())),
        })
    } else {
        None
    };
    let id_seed = format!("{item_id}|{}|{profile_json}", row.source);
    let pack_id = hex::encode(&Sha256::digest(id_seed.as_bytes())[..12]);
    let pack = Pack {
        pack_id,
        kind: row.kind,
        source: row.source,
        title: row.title,
        preset: row.preset,
        precut: row.precut,
        priority: row.priority,
        source_blob,
        profile_json,
        assets,
        campaigns,
    };
    if pack.total_bytes() > MAX_PACK_BYTES {
        return Err(format!(
            "Слишком много данных для передачи: {} МБ (потолок {} МБ)",
            pack.total_bytes() / 1_000_000,
            MAX_PACK_BYTES / 1_000_000
        ));
    }
    Ok((pack, paths))
}

/// Запоминает передачу и отдаёт задачу серверу: здесь она перестаёт обрабатываться.
pub fn mark_handed_over(
    db: &Connection,
    item_id: i64,
    pack: &Pack,
    remote_id: i64,
    node: &str,
) -> Result<(), String> {
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO transfers(pack_id,direction,kind,local_id,remote_id,state) VALUES(?1,'out','item',?2,?3,'accepted')
         ON CONFLICT(pack_id) DO UPDATE SET remote_id=?3,state='accepted',updated_at=unixepoch()",
        params![pack.pack_id, item_id, remote_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE factory_items SET owner_node=?2,origin_pack=?3,updated_at=unixepoch() WHERE id=?1 AND owner_node='local'",
        params![item_id, node, pack.pack_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'stage','queued',?2)",
        params![item_id, format!("Передана на узел «{node}»")],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Забирает задачу обратно (если на сервере ещё не начата или отказ).
pub fn take_back(db: &Connection, item_id: i64) -> Result<(), String> {
    let n = db
        .execute(
            "UPDATE factory_items SET owner_node='local',updated_at=unixepoch() WHERE id=?1 AND owner_node<>'local'",
            [item_id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("Задача и так на этом узле".into());
    }
    db.execute(
        "UPDATE transfers SET state='returned',updated_at=unixepoch() WHERE direction='out' AND local_id=?1",
        [item_id],
    )
    .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'stage','queued','Задача возвращена на этот узел')",
        [item_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ------------------------------------------------------------------ приём (принимающий узел)

pub struct Dirs<'a> {
    pub blobs: &'a Path,
    pub kit: &'a Path,
    pub sources: &'a Path,
}

fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Ok(());
    }
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
}

/// Принимает упаковку. Все файлы должны уже лежать в `blobs/<sha256>`. Возвращает id задачи на этом узле.
pub fn apply(db: &Connection, pack: &Pack, dirs: &Dirs) -> Result<i64, String> {
    if let Some(id) = db
        .query_row(
            "SELECT local_id FROM transfers WHERE pack_id=?1 AND direction='in'",
            [&pack.pack_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        return id.ok_or_else(|| "Упаковка принята ранее без задачи".to_string());
    }
    let missing: Vec<String> = pack
        .blobs()
        .into_iter()
        .filter(|b| !blob_path(dirs.blobs, &b.sha256).is_file())
        .map(|b| b.name)
        .collect();
    if !missing.is_empty() {
        return Err(format!("Не хватает файлов: {}", missing.join(", ")));
    }
    let mut profile: Value = serde_json::from_str(&pack.profile_json).map_err(|e| e.to_string())?;

    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    // ресурсы: путь привязан к хэшу, поэтому тот же файл не задваивается
    let mut asset_map = BTreeMap::new();
    for a in &pack.assets {
        let path = dirs
            .kit
            .join(format!("{}-{}", &a.blob.sha256[..12], safe_name(&a.blob.name)));
        link_or_copy(&blob_path(dirs.blobs, &a.blob.sha256), &path).map_err(|e| e.to_string())?;
        let p = path.to_string_lossy().into_owned();
        tx.execute(
            "INSERT OR IGNORE INTO assets(kind,path,name,meta_json) VALUES(?1,?2,?3,?4)",
            params![a.kind, p, a.name, a.meta_json],
        )
        .map_err(|e| e.to_string())?;
        let id: i64 = tx
            .query_row("SELECT id FROM assets WHERE path=?1", [&p], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        asset_map.insert(a.pc_id, id);
    }
    let mut campaign_map = BTreeMap::new();
    for c in &pack.campaigns {
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM campaigns WHERE name=?1 AND advertiser=?2 AND config=?3",
                params![c.name, c.advertiser, c.config],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let id = match found {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO campaigns(name,advertiser,config,enabled) VALUES(?1,?2,?3,?4)",
                    params![c.name, c.advertiser, c.config, c.enabled],
                )
                .map_err(|e| e.to_string())?;
                tx.last_insert_rowid()
            }
        };
        for b in &c.banners {
            if let Some(local) = asset_map.get(b) {
                tx.execute(
                    "INSERT OR IGNORE INTO campaign_banners(campaign_id,asset_id) VALUES(?1,?2)",
                    params![id, local],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        campaign_map.insert(c.pc_id, id);
    }
    remap_profile(&mut profile, &asset_map, &campaign_map);
    let source = match &pack.source_blob {
        Some(b) => {
            let path = dirs
                .sources
                .join(format!("{}-{}", &b.sha256[..12], safe_name(&b.name)));
            link_or_copy(&blob_path(dirs.blobs, &b.sha256), &path).map_err(|e| e.to_string())?;
            path.to_string_lossy().into_owned()
        }
        None => pack.source.clone(),
    };
    let position: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(position),0)+1 FROM factory_items",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO factory_items(position,kind,source,title,preset,precut,stage,channel_id,priority,profile_json,owner_node,origin_pack)
         VALUES(?1,?2,?3,?4,?5,?6,'queued',NULL,?7,?8,'local',?9)",
        params![
            position,
            pack.kind,
            source,
            pack.title,
            pack.preset,
            pack.precut,
            pack.priority,
            serde_json::to_string(&profile).map_err(|e| e.to_string())?,
            pack.pack_id
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO events(item_id,kind,stage,message) VALUES(?1,'stage','queued','Принята с другого узла')",
        [id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO transfers(pack_id,direction,kind,local_id,state) VALUES(?1,'in','item',?2,'accepted')",
        params![pack.pack_id, id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{drafts::Defaults, factory::profiles::{Channel, Snapshot, Template}};

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("avto-pack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn snapshot(banner: i64, backs: &[i64], camp: i64) -> String {
        let mut config = Defaults::default();
        config.banner_id = Some(banner);
        config.background_ids = backs.to_vec();
        let channel = Channel {
            id: 9,
            name: "К".into(),
            campaign_ids: vec![camp],
            mode: "review".into(),
            targets: vec![crate::factory::profiles::Target {
                platform: "telegram".into(),
                account_id: Some(4),
            }],
            ..Default::default()
        };
        let template = Template {
            id: 3,
            name: "Ш".into(),
            layout: "gameplay_stack".into(),
            config,
            is_default: false,
            preview_url: None,
        };
        serde_json::to_string(&Snapshot { channel, template }).unwrap()
    }

    #[test]
    fn movability_explains_why_not() {
        assert!(movability("queued", "local", false).is_ok());
        assert!(movability("fetching", "local", false).is_ok());
        assert!(movability("queued", "server", false).unwrap_err().contains("Уже передана"));
        assert!(movability("queued", "local", true).unwrap_err().contains("паузе"));
        assert!(movability("awaiting_approval", "local", false).unwrap_err().contains("решения"));
        assert!(movability("rendering_final", "local", false).is_err());
    }

    #[test]
    fn remap_rewrites_ids_and_strips_publish_targets() {
        let json = snapshot(11, &[21, 22], 5);
        let mut v: Value = serde_json::from_str(&json).unwrap();
        let refs = profile_refs(&v);
        assert_eq!(refs.assets, BTreeSet::from([11, 21, 22]));
        assert_eq!(refs.campaigns, BTreeSet::from([5]));
        let assets = BTreeMap::from([(11, 101), (21, 201)]);
        let camps = BTreeMap::from([(5, 50)]);
        remap_profile(&mut v, &assets, &camps);
        // читается обратно типизированно
        let back: Snapshot = serde_json::from_value(v).unwrap();
        assert_eq!(back.template.config.banner_id, Some(101));
        assert_eq!(back.template.config.background_ids, vec![201]);
        assert_eq!(back.channel.campaign_ids, vec![50]);
        assert!(back.channel.targets.is_empty());
        assert_eq!(back.channel.mode, "auto");
    }

    #[test]
    fn pack_travels_between_two_databases_once() {
        let dir = tmp("travel");
        let pc = db();
        let srv = db();
        // ресурсы на «ПК»
        let banner = dir.join("banner.png");
        let back = dir.join("gameplay.mp4");
        std::fs::write(&banner, b"banner-bytes").unwrap();
        std::fs::write(&back, b"gameplay-bytes").unwrap();
        pc.execute("INSERT INTO assets(id,kind,path,name) VALUES(11,'banner',?1,'Баннер.png')", [banner.to_string_lossy()]).unwrap();
        pc.execute("INSERT INTO assets(id,kind,path,name) VALUES(21,'background',?1,'Геймплей.mp4')", [back.to_string_lossy()]).unwrap();
        pc.execute("INSERT INTO campaigns(id,name,advertiser,config) VALUES(5,'Кампания','Реклама','{}')", []).unwrap();
        pc.execute("INSERT INTO campaign_banners(campaign_id,asset_id) VALUES(5,11)", []).unwrap();
        pc.execute(
            "INSERT INTO factory_items(position,kind,source,title,preset,precut,stage,profile_json) VALUES(1,'link','https://youtu.be/x','Тест','1080',0,'queued',?1)",
            [snapshot(11, &[21], 5)],
        ).unwrap();
        let item = pc.last_insert_rowid();
        let pack = build(&pc, item).unwrap();
        assert_eq!(pack.assets.len(), 2);
        assert_eq!(pack.blobs().len(), 2);
        assert!(pack.source_blob.is_none());

        // «загрузка» файлов на сервер
        let sdir = tmp("travel-srv");
        let (blobs, kit, sources) = (sdir.join("blobs"), sdir.join("kit"), sdir.join("sources"));
        std::fs::create_dir_all(&blobs).unwrap();
        for a in &pack.assets {
            let from = if a.pc_id == 11 { &banner } else { &back };
            std::fs::copy(from, blob_path(&blobs, &a.blob.sha256)).unwrap();
        }
        let dirs = Dirs { blobs: &blobs, kit: &kit, sources: &sources };
        let id = apply(&srv, &pack, &dirs).unwrap();
        assert_eq!(apply(&srv, &pack, &dirs).unwrap(), id, "повторный приём ничего не удваивает");
        let n: i64 = srv.query_row("SELECT COUNT(*) FROM factory_items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let (owner, profile): (String, String) = srv
            .query_row("SELECT owner_node,profile_json FROM factory_items WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(owner, "local");
        let snap: Snapshot = serde_json::from_str(&profile).unwrap();
        let banner_local: i64 = srv.query_row("SELECT id FROM assets WHERE kind='banner'", [], |r| r.get(0)).unwrap();
        assert_eq!(snap.template.config.banner_id, Some(banner_local));
        let camp_local: i64 = srv.query_row("SELECT id FROM campaigns", [], |r| r.get(0)).unwrap();
        assert_eq!(snap.channel.campaign_ids, vec![camp_local]);
        let linked: i64 = srv.query_row("SELECT COUNT(*) FROM campaign_banners WHERE campaign_id=?1 AND asset_id=?2", [camp_local, banner_local], |r| r.get(0)).unwrap();
        assert_eq!(linked, 1);

        // исходный узел отдаёт задачу
        mark_handed_over(&pc, item, &pack, id, "server").unwrap();
        let o: String = pc.query_row("SELECT owner_node FROM factory_items WHERE id=?1", [item], |r| r.get(0)).unwrap();
        assert_eq!(o, "server");
        assert!(build(&pc, item).unwrap_err().contains("Уже передана"));
        take_back(&pc, item).unwrap();
        assert!(build(&pc, item).is_ok());
    }

    #[test]
    fn apply_refuses_when_files_are_missing() {
        let dir = tmp("missing");
        let pc = db();
        let f = dir.join("v.mp4");
        std::fs::write(&f, b"video").unwrap();
        pc.execute(
            "INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,'file',?1,'t','1080',0,'queued')",
            [f.to_string_lossy()],
        ).unwrap();
        let pack = build(&pc, pc.last_insert_rowid()).unwrap();
        assert!(pack.source_blob.is_some());
        let srv = db();
        let sdir = tmp("missing-srv");
        let dirs = Dirs { blobs: &sdir.join("b"), kit: &sdir.join("k"), sources: &sdir.join("s") };
        let err = apply(&srv, &pack, &dirs).unwrap_err();
        assert!(err.contains("Не хватает файлов"));
        let n: i64 = srv.query_row("SELECT COUNT(*) FROM factory_items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
