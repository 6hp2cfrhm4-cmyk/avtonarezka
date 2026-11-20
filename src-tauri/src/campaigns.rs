//! Кампания закрепляется при создании черновика вместе с его оформлением.
use crate::host::{AppHandle, Emitter, State};
use crate::{factory::profiles::Snapshot, state::AppState};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Campaign {
    pub id: i64,
    pub name: String,
    pub advertiser: String,
    pub enabled: bool,
    pub banner_ids: Vec<i64>,
    pub link: String,
    pub promo: String,
    pub utm: String,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub weight: u32,
    pub daily_limit: Option<u32>,
    pub total_limit: Option<u32>,
    pub channel_ids: Vec<i64>,
    pub platforms: Vec<String>,
}
impl Default for Campaign {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            advertiser: String::new(),
            enabled: true,
            banner_ids: vec![],
            link: String::new(),
            promo: String::new(),
            utm: "utm_source={platform}&utm_content={clip}".into(),
            starts_at: None,
            ends_at: None,
            weight: 1,
            daily_limit: None,
            total_limit: None,
            channel_ids: vec![],
            platforms: vec![],
        }
    }
}
#[derive(Clone, Default, Debug)]
pub struct Usage {
    pub campaign: i64,
    pub total: u64,
    pub today: u64,
    pub banners: Vec<(i64, u64)>,
}
pub struct Context<'a> {
    pub at: i64,
    pub day_start: i64,
    pub channel: i64,
    pub platforms: &'a [String],
    pub seed: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    pub campaign_id: i64,
    pub banner_id: i64,
}
fn mix(mut n: u64) -> u64 {
    n ^= n >> 30;
    n = n.wrapping_mul(0xbf58476d1ce4e5b9);
    n ^= n >> 27;
    n = n.wrapping_mul(0x94d049bb133111eb);
    n ^ (n >> 31)
}
/// Взвешенный круг: следующая доля (использований+1)/вес; seed разрешает равенство.
/// Никакой случайности/БД внутри решения. Лимиты считают зарезервированные черновики.
pub fn pick_banner(ctx: &Context<'_>, campaigns: &[Campaign], history: &[Usage]) -> Option<Pick> {
    let mut eligible: Vec<_> = campaigns
        .iter()
        .filter(|c| {
            c.enabled
                && c.weight > 0
                && !c.banner_ids.is_empty()
                && c.starts_at.is_none_or(|t| ctx.at >= t)
                && c.ends_at.is_none_or(|t| ctx.at < t)
                && (c.channel_ids.is_empty() || c.channel_ids.contains(&ctx.channel))
                && (c.platforms.is_empty()
                    || (!ctx.platforms.is_empty()
                        && ctx.platforms.iter().all(|p| c.platforms.contains(p))))
        })
        .filter_map(|c| {
            let u = history
                .iter()
                .find(|u| u.campaign == c.id)
                .cloned()
                .unwrap_or_default();
            (c.total_limit.is_none_or(|n| u.total < (n as u64))
                && c.daily_limit.is_none_or(|n| u.today < (n as u64)))
            .then_some((c, u))
        })
        .collect();
    eligible.sort_by(|(a, ua), (b, ub)| {
        ((ua.total + 1) * (b.weight as u64))
            .cmp(&((ub.total + 1) * (a.weight as u64)))
            .then_with(|| mix(ctx.seed ^ (a.id as u64)).cmp(&mix(ctx.seed ^ (b.id as u64))))
            .then(a.id.cmp(&b.id))
    });
    let (c, u) = eligible.first()?;
    let banner = c.banner_ids.iter().min_by_key(|id| {
        (
            u.banners
                .iter()
                .find(|(bid, _)| bid == *id)
                .map(|(_, n)| *n)
                .unwrap_or(0),
            mix(ctx.seed ^ (**id as u64)),
            **id,
        )
    })?;
    Some(Pick {
        campaign_id: c.id,
        banner_id: *banner,
    })
}
pub(crate) fn list(db: &Connection) -> Result<Vec<Campaign>> {
    let mut q = db
        .prepare("SELECT id,name,advertiser,enabled,config FROM campaigns ORDER BY id")
        .map_err(err)?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(err)?;
    rows.map(|row| {
        let (id, name, advertiser, enabled, raw) = row.map_err(err)?;
        let mut c: Campaign = serde_json::from_str(&raw).map_err(err)?;
        c.id = id;
        c.name = name;
        c.advertiser = advertiser;
        c.enabled = enabled;
        Ok(c)
    })
    .collect()
}
fn validate(c: &mut Campaign, db: &Connection) -> Result<()> {
    c.name = c.name.trim().into();
    c.advertiser = c.advertiser.trim().into();
    c.link = c.link.trim().into();
    c.promo = c.promo.trim().into();
    if c.name.is_empty() || c.advertiser.is_empty() {
        return Err("Укажи название кампании и рекламодателя".into());
    }
    if c.name.chars().count() > 150
        || c.advertiser.chars().count() > 150
        || c.promo.chars().count() > 150
    {
        return Err("Название, рекламодатель и промокод: до 150 знаков".into());
    }
    if !(1..=1000).contains(&c.weight) {
        return Err("Вес кампании: от 1 до 1000".into());
    }
    if c.starts_at.zip(c.ends_at).is_some_and(|(a, b)| a >= b) {
        return Err("Окончание кампании должно быть позже начала".into());
    }
    if c.daily_limit == Some(0) || c.total_limit == Some(0) {
        return Err("Лимит должен быть больше нуля или пустым".into());
    }
    c.banner_ids.sort();
    c.banner_ids.dedup();
    c.channel_ids.sort();
    c.channel_ids.dedup();
    c.platforms.sort();
    c.platforms.dedup();
    if c.banner_ids.is_empty() {
        return Err("Выбери хотя бы один баннер".into());
    }
    for id in &c.banner_ids {
        if !db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1 AND kind='banner')",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(err)?
        {
            return Err(format!("Баннер #{id} отсутствует в библиотеке"));
        }
    }
    for id in &c.channel_ids {
        if !db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM channels WHERE id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(err)?
        {
            return Err(format!("Канал #{id} не найден"));
        }
    }
    if c.platforms
        .iter()
        .any(|p| !["tiktok", "youtube", "instagram", "telegram", "vk"].contains(&p.as_str()))
    {
        return Err("Неизвестная площадка кампании".into());
    }
    tracking_link(c, "telegram", 1)?;
    Ok(())
}
pub(crate) fn save(db: &Connection, mut c: Campaign) -> Result<Campaign> {
    validate(&mut c, db)?;
    let tx = db.unchecked_transaction().map_err(err)?;
    if c.id==0 {tx.execute("INSERT INTO campaigns(name,advertiser,enabled,config)VALUES(?1,?2,?3,?4)",params![c.name,c.advertiser,c.enabled,serde_json::to_string(&c).map_err(err)?]).map_err(err)?;c.id=tx.last_insert_rowid();}
    else if tx.execute("UPDATE campaigns SET name=?2,advertiser=?3,enabled=?4,config=?5,updated_at=unixepoch() WHERE id=?1",params![c.id,c.name,c.advertiser,c.enabled,serde_json::to_string(&c).map_err(err)?]).map_err(err)?==0 {return Err("Кампания не найдена".into());}
    tx.execute("DELETE FROM campaign_banners WHERE campaign_id=?1", [c.id])
        .map_err(err)?;
    for id in &c.banner_ids {
        tx.execute(
            "INSERT INTO campaign_banners(campaign_id,asset_id)VALUES(?1,?2)",
            params![c.id, id],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    Ok(c)
}
fn usage(db: &Connection, day: i64) -> Result<Vec<Usage>> {
    let mut out = vec![];
    for c in list(db)? {
        let(total,today)=db.query_row("SELECT count(*),COALESCE(sum(assigned_at>=?2),0) FROM clip_campaigns WHERE campaign_id=?1",params![c.id,day],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64))).map_err(err)?;
        let mut q=db.prepare("SELECT banner_id,count(*) FROM clip_campaigns WHERE campaign_id=?1 GROUP BY banner_id").map_err(err)?;
        let banners = q
            .query_map([c.id], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))
            .map_err(err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err)?;
        out.push(Usage {
            campaign: c.id,
            total,
            today,
            banners,
        });
    }
    Ok(out)
}
fn day_start(at: i64) -> i64 {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(at, 0)
        .single()
        .and_then(|t| {
            t.date_naive()
                .and_hms_opt(0, 0, 0)
                .and_then(|d| Local.from_local_datetime(&d).earliest())
        })
        .map(|d| d.timestamp())
        .unwrap_or(at - at.rem_euclid(86400))
}
fn project_profile(db: &Connection, project: i64) -> Result<Option<Snapshot>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT profile_json FROM factory_items WHERE project_id=?1 ORDER BY id DESC LIMIT 1",
            [project],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?
        .flatten();
    raw.map(|r| serde_json::from_str(&r).map_err(err))
        .transpose()
}
pub(crate) fn assign(db: &Connection, project: i64, draft: i64, at: i64) -> Result<Option<Pick>> {
    if let Some(p) = db
        .query_row(
            "SELECT campaign_id,banner_id FROM clip_campaigns WHERE draft_id=?1",
            [draft],
            |r| {
                Ok(Pick {
                    campaign_id: r.get(0)?,
                    banner_id: r.get(1)?,
                })
            },
        )
        .optional()
        .map_err(err)?
    {
        return Ok(Some(p));
    }
    let Some(profile) = project_profile(db, project)? else {
        return Ok(None);
    };
    if profile.channel.campaign_ids.is_empty() {
        return Ok(None);
    }
    let campaigns: Vec<_> = list(db)?
        .into_iter()
        .filter(|c| profile.channel.campaign_ids.contains(&c.id))
        .collect();
    let platforms: Vec<_> = profile
        .channel
        .targets
        .iter()
        .map(|t| t.platform.clone())
        .collect();
    let ctx = Context {
        at,
        day_start: day_start(at),
        channel: profile.channel.id,
        platforms: &platforms,
        seed: draft as u64,
    };
    let pick = pick_banner(&ctx, &campaigns, &usage(db, ctx.day_start)?).ok_or(
        "Нет доступного баннера назначенных кампаний: проверь даты, лимиты и площадки в Кампаниях",
    )?;
    let campaign = campaigns.iter().find(|c| c.id == pick.campaign_id).unwrap();
    db.execute("INSERT INTO clip_campaigns(draft_id,campaign_id,banner_id,snapshot,assigned_at)VALUES(?1,?2,?3,?4,?5)",params![draft,pick.campaign_id,pick.banner_id,serde_json::to_string(campaign).map_err(err)?,at]).map_err(err)?;
    Ok(Some(pick))
}
pub fn tracking_link(c: &Campaign, platform: &str, clip: i64) -> Result<String> {
    if c.link.is_empty() {
        return Ok(String::new());
    }
    let mut url = reqwest::Url::parse(&c.link)
        .map_err(|_| "Рекламная ссылка должна быть полным HTTP(S) адресом")?;
    if !["https", "http"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Нужна HTTP(S) ссылка без логина и пароля".into());
    }
    let query = c
        .utm
        .replace("{platform}", platform)
        .replace("{clip}", &clip.to_string());
    if query.contains(['{', '}']) || query.contains('#') || query.len() > 2000 {
        return Err("Шаблон UTM: используй только {platform} и {clip}, до 2000 знаков".into());
    }
    if !query.is_empty() {
        let extra = reqwest::Url::parse(&format!(
            "https://utm.invalid/?{}",
            query.trim_start_matches('?')
        ))
        .map_err(err)?
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<Vec<_>>();
        if extra
            .iter()
            .any(|(k, v)| k.is_empty() || !k.starts_with("utm_") || v.is_empty())
        {
            return Err("Шаблон UTM должен содержать параметры utm_ с непустыми значениями".into());
        }
        let keep = url
            .query_pairs()
            .filter(|(k, _)| !extra.iter().any(|(x, _)| x == k))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect::<Vec<_>>();
        url.set_query(None);
        url.query_pairs_mut().extend_pairs(keep).extend_pairs(extra);
    }
    Ok(url.to_string())
}
pub(crate) fn caption_fields(
    db: &Connection,
    draft: i64,
    platform: &str,
) -> Result<Option<(String, String)>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT snapshot FROM clip_campaigns WHERE draft_id=?1",
            [draft],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    raw.map(|s| {
        let c: Campaign = serde_json::from_str(&s).map_err(err)?;
        if !c.platforms.is_empty() && !c.platforms.iter().any(|p| p == platform) {
            return Err("Кампания этого ролика не разрешает выбранную площадку".into());
        }
        Ok((tracking_link(&c, platform, draft)?, c.promo))
    })
    .transpose()
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub draft_id: i64,
    pub campaign_id: i64,
    pub campaign_name: String,
    pub advertiser: String,
    pub banner_id: i64,
    pub assigned_at: i64,
}
pub(crate) fn assignments(db: &Connection) -> Result<Vec<Assignment>> {
    let mut q=db.prepare("SELECT draft_id,campaign_id,banner_id,assigned_at,snapshot FROM clip_campaigns ORDER BY assigned_at DESC,draft_id DESC").map_err(err)?;
    let out = q
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(err)?
        .map(|r| {
            let (draft_id, campaign_id, banner_id, assigned_at, s) = r.map_err(err)?;
            let c: Campaign = serde_json::from_str(&s).map_err(err)?;
            Ok(Assignment {
                draft_id,
                campaign_id,
                banner_id,
                assigned_at,
                campaign_name: c.name,
                advertiser: c.advertiser,
            })
        })
        .collect();
    out
}
pub(crate) fn preview(db: &Connection, channel: i64, at: i64) -> Result<Vec<Option<Pick>>> {
    let channel = crate::factory::profiles::channels(db)?
        .into_iter()
        .find(|c| c.id == channel)
        .ok_or("Канал не найден")?;
    let campaigns: Vec<_> = list(db)?
        .into_iter()
        .filter(|c| channel.campaign_ids.contains(&c.id))
        .collect();
    let platforms: Vec<_> = channel.targets.iter().map(|t| t.platform.clone()).collect();
    let mut usage = usage(db, day_start(at))?;
    let mut out = vec![];
    let next: i64 = db
        .query_row("SELECT COALESCE(max(id),0)+1 FROM drafts", [], |r| r.get(0))
        .map_err(err)?;
    for i in 0..10 {
        let pick = pick_banner(
            &Context {
                at,
                day_start: day_start(at),
                channel: channel.id,
                platforms: &platforms,
                seed: (next + i) as u64,
            },
            &campaigns,
            &usage,
        );
        if let Some(p) = &pick {
            if !usage.iter().any(|u| u.campaign == p.campaign_id) {
                usage.push(Usage {
                    campaign: p.campaign_id,
                    ..Default::default()
                });
            }
            let u = usage
                .iter_mut()
                .find(|u| u.campaign == p.campaign_id)
                .unwrap();
            u.total += 1;
            u.today += 1;
            if let Some((_, n)) = u.banners.iter_mut().find(|(id, _)| *id == p.banner_id) {
                *n += 1
            } else {
                u.banners.push((p.banner_id, 1));
            }
        }
        out.push(pick);
    }
    Ok(out)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn campaigns_list(state: State<'_, AppState>) -> Result<Vec<Campaign>> {
    list(&state.db.0.lock().unwrap())
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn campaign_save(
    app: AppHandle,
    state: State<'_, AppState>,
    campaign: Campaign,
) -> Result<Campaign> {
    let c = save(&state.db.0.lock().unwrap(), campaign)?;
    let _ = app.emit("factory-changed", ());
    Ok(c)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn campaign_preview(state: State<'_, AppState>, channel_id: i64) -> Result<Vec<Option<Pick>>> {
    preview(
        &state.db.0.lock().unwrap(),
        channel_id,
        chrono::Utc::now().timestamp(),
    )
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn campaign_assignments(state: State<'_, AppState>) -> Result<Vec<Assignment>> {
    assignments(&state.db.0.lock().unwrap())
}

pub(crate) fn ensure_banner(
    db: &Connection,
    draft: i64,
    config: &crate::drafts::DraftConfig,
) -> Result<()> {
    let banner: Option<i64> = db
        .query_row(
            "SELECT banner_id FROM clip_campaigns WHERE draft_id=?1",
            [draft],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    if banner.is_some() && (config.banner_id != banner || !config.style.banner.enabled) {
        return Err(
            "Баннер закреплён кампанией. Для этого ролика нельзя заменить или выключить рекламу"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn campaign(id: i64, weight: u32) -> Campaign {
        Campaign {
            id,
            name: format!("Кампания {id}"),
            advertiser: "Рекламодатель".into(),
            weight,
            banner_ids: vec![id * 10, id * 10 + 1],
            ..Default::default()
        }
    }
    #[test]
    fn weighted_round_banner_balance_limits_expiry_and_seed() {
        let mut campaigns = vec![campaign(1, 3), campaign(2, 1)];
        let mut history: Vec<Usage> = vec![];
        for i in 0..100 {
            let ctx = Context {
                at: 100,
                day_start: 0,
                channel: 1,
                platforms: &[],
                seed: i,
            };
            let p = pick_banner(&ctx, &campaigns, &history).unwrap();
            assert_eq!(Some(p.clone()), pick_banner(&ctx, &campaigns, &history));
            if !history.iter().any(|u| u.campaign == p.campaign_id) {
                history.push(Usage {
                    campaign: p.campaign_id,
                    ..Default::default()
                });
            }
            let u = history
                .iter_mut()
                .find(|u| u.campaign == p.campaign_id)
                .unwrap();
            u.total += 1;
            u.today += 1;
            if let Some((_, n)) = u.banners.iter_mut().find(|(id, _)| *id == p.banner_id) {
                *n += 1
            } else {
                u.banners.push((p.banner_id, 1));
            }
        }
        assert_eq!(history.iter().find(|u| u.campaign == 1).unwrap().total, 75);
        for u in &history {
            assert!(u.banners[0].1.abs_diff(u.banners[1].1) <= 1);
        }
        campaigns[0].daily_limit = Some(75);
        campaigns[1].ends_at = Some(100);
        let ctx = Context {
            at: 100,
            day_start: 0,
            channel: 1,
            platforms: &[],
            seed: 3,
        };
        assert!(pick_banner(&ctx, &campaigns, &history).is_none());
        campaigns[0].daily_limit = None;
        campaigns[0].total_limit = Some(75);
        assert!(pick_banner(&ctx, &campaigns, &history).is_none());
        assert!(pick_banner(&ctx, &[], &[]).is_none());
    }
    #[test]
    fn filters_require_all_platforms_and_match_channel_dates() {
        let mut c = campaign(1, 1);
        c.channel_ids = vec![4];
        c.platforms = vec!["telegram".into()];
        c.starts_at = Some(50);
        c.ends_at = Some(60);
        let ps = vec!["telegram".into(), "youtube".into()];
        let mut ctx = Context {
            at: 50,
            day_start: 0,
            channel: 4,
            platforms: &ps,
            seed: 0,
        };
        assert!(pick_banner(&ctx, &[c.clone()], &[]).is_none());
        ctx.platforms = &ps[..1];
        assert!(pick_banner(&ctx, &[c.clone()], &[]).is_some());
        ctx.at = 60;
        assert!(pick_banner(&ctx, &[c.clone()], &[]).is_none());
        ctx.at = 49;
        assert!(pick_banner(&ctx, &[c.clone()], &[]).is_none());
        ctx.at = 50;
        ctx.channel = 5;
        assert!(pick_banner(&ctx, &[c], &[]).is_none());
    }
    #[test]
    fn tracking_preserves_query_fragment_and_replaces_utm_without_duplicates() {
        let mut c = campaign(1, 1);
        c.link = "https://example.com/path?ref=abc&utm_source=old#offer".into();
        let u = reqwest::Url::parse(&tracking_link(&c, "youtube", 42).unwrap()).unwrap();
        assert_eq!(u.fragment(), Some("offer"));
        let pairs = u.query_pairs().collect::<Vec<_>>();
        assert_eq!(pairs.iter().filter(|(k, _)| k == "utm_source").count(), 1);
        assert!(pairs.iter().any(|(k, v)| k == "utm_content" && v == "42"));
        assert!(pairs.iter().any(|(k, v)| k == "ref" && v == "abc"));
        c.link = "javascript:alert(1)".into();
        assert!(tracking_link(&c, "telegram", 1).is_err());
        c.link = "https://a.invalid".into();
        c.utm = "utm_source={unknown}".into();
        assert!(tracking_link(&c, "telegram", 1).is_err());
    }
    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute_batch("INSERT INTO assets(id,kind,path,name,meta_json)VALUES(10,'banner','/one.mp4','Первый','{}'),(11,'banner','/two.mp4','Второй','{}');INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio)VALUES(1,'/record.mp4','record','Запись',60,1280,720,30,1);INSERT INTO moments(id,project_id,start,\"end\",status,transcript)VALUES(1,1,0,20,'accepted','Содержательная речь участника.'),(2,1,20,40,'accepted','Участник исправил свою ошибку.');").unwrap();
        db
    }
    #[test]
    fn actual_drafts_reserve_once_snapshot_survives_edit_and_caption_protects_ad() {
        let db = setup();
        let mut c = campaign(0, 1);
        c.banner_ids = vec![10, 11];
        c.link = "https://example.com/offer?ref=author".into();
        c.promo = "ТЕСТ".into();
        let c = save(&db, c).unwrap();
        let channel = crate::factory::profiles::save_channel(
            &db,
            crate::factory::profiles::Channel {
                name: "Источник".into(),
                campaign_ids: vec![c.id],
                ..Default::default()
            },
        )
        .unwrap();
        db.execute("INSERT INTO factory_items(position,kind,source,project_id,channel_id,profile_json)VALUES(1,'file','/record.mp4',1,?1,?2)",params![channel.id,crate::factory::profiles::snapshot(&db,channel.id).unwrap()]).unwrap();
        assert_eq!(
            preview(&db, channel.id, chrono::Utc::now().timestamp())
                .unwrap()
                .len(),
            10
        );
        assert!(assignments(&db).unwrap().is_empty());
        assert_eq!(crate::drafts::generate(&db, 1).unwrap(), 2);
        assert_eq!(crate::drafts::generate(&db, 1).unwrap(), 0);
        let assigned = assignments(&db).unwrap();
        assert_eq!(assigned.len(), 2);
        assert_ne!(assigned[0].banner_id, assigned[1].banner_id);
        let id = assigned[0].draft_id;
        let mut cfg = crate::drafts::get(&db, id).unwrap().config;
        assert_eq!(cfg.banner_id, Some(assigned[0].banner_id));
        ensure_banner(&db, id, &cfg).unwrap();
        cfg.banner_id = None;
        assert!(ensure_banner(&db, id, &cfg).is_err());
        let mut edited = c.clone();
        edited.link = "https://new.invalid".into();
        edited.promo = "ДРУГОЙ".into();
        edited.name = "Новое имя".into();
        save(&db, edited).unwrap();
        let (link, promo) = caption_fields(&db, id, "telegram").unwrap().unwrap();
        assert!(link.starts_with("https://example.com/offer?"));
        assert_eq!(promo, "ТЕСТ");
        assert_ne!(assignments(&db).unwrap()[0].campaign_name, "Новое имя");
        let mut saved =
            crate::caption::commands::load(&db, id, crate::publish::PlatformId::Telegram).unwrap();
        assert_eq!(saved.fields.link, link);
        saved.fields.link.clear();
        saved.fields.promo.clear();
        crate::caption::commands::save_for_account(&db, id, saved, None).unwrap();
        let saved =
            crate::caption::commands::load(&db, id, crate::publish::PlatformId::Telegram).unwrap();
        assert_eq!(saved.fields.link, link);
        assert_eq!(saved.fields.promo, "ТЕСТ");
        assert!(db
            .execute("DELETE FROM assets WHERE id=?1", [assigned[0].banner_id])
            .is_err());
        assert!(db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none());
    }
    #[test]
    fn exhausted_campaign_rolls_back_drafts_and_assignments_as_one_transaction() {
        let db = setup();
        let mut c = campaign(0, 1);
        c.banner_ids = vec![10];
        c.total_limit = Some(1);
        let c = save(&db, c).unwrap();
        let channel = crate::factory::profiles::save_channel(
            &db,
            crate::factory::profiles::Channel {
                name: "Канал".into(),
                campaign_ids: vec![c.id],
                ..Default::default()
            },
        )
        .unwrap();
        db.execute("INSERT INTO factory_items(position,kind,source,project_id,profile_json)VALUES(1,'file','/record.mp4',1,?1)",[crate::factory::profiles::snapshot(&db,channel.id).unwrap()]).unwrap();
        assert!(crate::drafts::generate(&db, 1).is_err());
        assert!(assignments(&db).unwrap().is_empty());
        assert_eq!(
            db.query_row("SELECT count(*) FROM drafts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
