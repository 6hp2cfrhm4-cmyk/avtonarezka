//! Самодостаточный отчёт: реальные назначения и строки общей очереди.
use crate::campaigns::{self, Campaign};
#[cfg(feature = "desktop")]
use crate::host::Manager;
use crate::host::State;
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;
type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub draft_id: i64,
    pub campaign: String,
    pub advertiser: String,
    pub banner: String,
    pub title: String,
    pub platform: Option<String>,
    pub account: Option<String>,
    pub state: Option<String>,
    pub sent_at: Option<i64>,
    pub url: Option<String>,
    pub caption: Option<String>,
    pub poster: Option<String>,
    pub views: Option<i64>,
    pub measured_at: Option<i64>,
    pub metric_source: Option<String>,
}
pub(crate) fn rows(db: &Connection, id: i64) -> Result<Vec<Row>> {
    let mut q=db.prepare("SELECT cc.draft_id,cc.snapshot,a.name,COALESCE(NULLIF(m.title,''),p.title),j.platform,ac.label,j.state,j.sent_at,j.url,j.caption,d.poster_path,mt.views,mt.observed_at,mt.source FROM clip_campaigns cc JOIN drafts d ON d.id=cc.draft_id JOIN moments m ON m.id=d.moment_id JOIN projects p ON p.id=m.project_id JOIN assets a ON a.id=cc.banner_id LEFT JOIN publications j ON j.draft_id=d.id LEFT JOIN accounts ac ON ac.id=COALESCE(j.target_account_id,j.account_id) LEFT JOIN metrics mt ON mt.id=(SELECT mm.id FROM metrics mm WHERE mm.publication_id=j.id AND mm.views IS NOT NULL ORDER BY mm.observed_at DESC,mm.source DESC,mm.id DESC LIMIT 1) WHERE cc.campaign_id=?1 ORDER BY cc.draft_id,j.id").map_err(err)?;
    let mapped = q
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(1)?,
                Row {
                    draft_id: r.get(0)?,
                    campaign: String::new(),
                    advertiser: String::new(),
                    banner: r.get(2)?,
                    title: r.get(3)?,
                    platform: r.get(4)?,
                    account: r.get(5)?,
                    state: r.get(6)?,
                    sent_at: r.get(7)?,
                    url: r.get(8)?,
                    caption: r.get(9)?,
                    poster: r.get(10)?,
                    views: r.get(11)?,
                    measured_at: r.get(12)?,
                    metric_source: r.get(13)?,
                },
            ))
        })
        .map_err(err)?;
    mapped
        .map(|r| {
            let (s, mut row) = r.map_err(err)?;
            let c: Campaign = serde_json::from_str(&s).map_err(err)?;
            row.campaign = c.name;
            row.advertiser = c.advertiser;
            Ok(row)
        })
        .collect()
}
fn html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn csv_cell(s: &str) -> String {
    let prefix = if s.trim_start().starts_with(['=', '+', '-', '@']) {
        "'"
    } else {
        ""
    };
    format!("\"{prefix}{}\"", s.replace('"', "\"\""))
}
fn date(t: Option<i64>) -> String {
    t.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%d.%m.%Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}
fn state(s: Option<&str>) -> &str {
    match s {
        Some("done") => "Опубликовано",
        Some("needs_check") => "Нужна проверка",
        Some("failed") => "Ошибка",
        Some("cancelled") => "Отменено",
        Some("scheduled") => "В календаре",
        Some("sending") => "Отправляется",
        Some(_) => "В работе",
        None => "Без публикации",
    }
}
fn base64(bytes: &[u8]) -> String {
    const C: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        out.push(C[((n >> 18) & 63) as usize] as char);
        out.push(C[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            C[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            C[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
fn image(path: Option<&str>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    if meta.len() > 1_000_000 {
        return String::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let mime = if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else {
        return String::new();
    };
    format!(
        "<img alt=\"Кадр ролика\" src=\"data:{mime};base64,{}\">",
        base64(&bytes)
    )
}
pub(crate) fn render(c: &Campaign, rows: &[Row]) -> (String, String) {
    let mut csv="\u{feff}Ролик;Кампания;Рекламодатель;Баннер;Название;Площадка;Аккаунт;Состояние;Дата публикации;Ссылка;Просмотры;Подпись;Дата замера;Источник метрик\r\n".to_string();
    let mut body = String::new();
    for r in rows {
        let platform = r.platform.as_deref().unwrap_or("—");
        let account = r.account.as_deref().unwrap_or("—");
        let status = state(r.state.as_deref());
        let at = date(r.sent_at);
        let url = r.url.as_deref().unwrap_or("");
        let link =
            if reqwest::Url::parse(url).is_ok_and(|u| ["http", "https"].contains(&u.scheme())) {
                format!("<a href=\"{}\">Открыть публикацию</a>", html(url))
            } else {
                "—".into()
            };
        let values = [
            r.draft_id.to_string(),
            r.campaign.clone(),
            r.advertiser.clone(),
            r.banner.clone(),
            r.title.clone(),
            platform.into(),
            account.into(),
            status.into(),
            at.clone(),
            url.into(),
            r.views.map(|n| n.to_string()).unwrap_or_default(),
            r.caption.clone().unwrap_or_default(),
            r.measured_at.map(|t| date(Some(t))).unwrap_or_default(),
            r.metric_source.clone().unwrap_or_default(),
        ];
        csv.push_str(
            &values
                .iter()
                .map(|s| csv_cell(s))
                .collect::<Vec<_>>()
                .join(";"),
        );
        csv.push_str("\r\n");
        let views = r.views.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
        let metric_date = r
            .measured_at
            .map(|t| {
                format!(
                    "Замер: {} · {}",
                    date(Some(t)),
                    if r.metric_source.as_deref() == Some("manual") {
                        "вручную"
                    } else {
                        "API"
                    }
                )
            })
            .unwrap_or_default();
        body.push_str(&format!("<tr><td>{}<strong>#{}</strong></td><td><strong>{}</strong><small>{} · {}</small><small>{}</small></td><td>{}<small>{}</small></td><td>{}<small>{}</small></td><td>{}<small>Просмотры: {views}</small><small>{}</small></td></tr>",image(r.poster.as_deref()),r.draft_id,html(&r.title),html(&r.campaign),html(&r.advertiser),html(&r.banner),html(platform),html(account),html(status),html(&at),link,html(&metric_date)));
    }
    if body.is_empty() {
        body = "<tr><td colspan=5>В этой кампании пока нет созданных роликов.</td></tr>".into();
    }
    let clips = rows
        .iter()
        .map(|r| r.draft_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let published = rows
        .iter()
        .filter(|r| r.state.as_deref() == Some("done"))
        .count();
    let html = format!(
        r#"<!doctype html><html lang="ru"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Отчёт · {}</title><style>body{{margin:0;background:#f7f7f7;color:#171717;font:14px/1.5 system-ui,sans-serif}}main{{max-width:1180px;margin:32px auto;padding:24px}}h1{{font-size:26px;margin:0 0 8px}}h2{{font-size:18px}}p,small{{color:#555}}small{{display:block;font-size:13px}}table{{width:100%;border-collapse:collapse;background:white}}td,th{{padding:14px 16px;text-align:left;border-bottom:1px solid #eee;vertical-align:top}}th{{font-weight:500;font-size:13px;color:#555}}img{{display:block;width:64px;height:100px;object-fit:cover;margin-bottom:6px}}a{{color:#235b40}}.print{{border:1px solid #ccc;background:white;color:#171717;padding:8px 14px;border-radius:6px;font:inherit;cursor:pointer}}.totals{{display:flex;gap:32px;padding:16px 0;font-variant-numeric:tabular-nums}}@media print{{.print{{display:none}}body{{background:white}}main{{margin:0;padding:0}}tr{{break-inside:avoid}}}}@media(max-width:700px){{main{{padding:12px}}table{{display:block;overflow:auto}}}}</style><main><button class="print" type="button" onclick="window.print()">Печать / PDF</button><h1>{}</h1><p>Рекламодатель: {} · Автонарезка · {}</p><div class="totals"><span>Создано роликов: <strong>{clips}</strong></span><span>Подтверждённых публикаций: <strong>{published}</strong></span></div><p>Данные из назначений баннеров и журнала публикаций. Просмотры — последний доступный замер для каждой публикации, с датой и источником. Отсутствие метрик не означает ноль. Кнопка «Печать / PDF» открывает системный диалог печати.</p><table><thead><tr><th>Ролик</th><th>Название и реклама</th><th>Площадка / аккаунт</th><th>Состояние / дата</th><th>Публикация</th></tr></thead><tbody>{body}</tbody></table></main></html>"#,
        html(&c.name),
        html(&c.name),
        html(&c.advertiser),
        date(Some(chrono::Utc::now().timestamp()))
    );
    (html, csv)
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn campaign_report_rows(
    state: State<'_, crate::state::AppState>,
    campaign_id: i64,
) -> Result<Vec<Row>> {
    rows(&state.db.0.lock().unwrap(), campaign_id)
}
#[derive(Serialize)]
pub struct Export {
    pub html: String,
    pub csv: String,
}
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn campaign_report_export(
    state: State<'_, crate::state::AppState>,
    campaign_id: i64,
    path: String,
) -> Result<Export> {
    let (report, csv) = {
        let db = state.db.0.lock().unwrap();
        let campaign = campaigns::list(&db)?
            .into_iter()
            .find(|c| c.id == campaign_id)
            .ok_or("Кампания не найдена")?;
        render(&campaign, &rows(&db, campaign_id)?)
    };
    let path = Path::new(&path);
    if path
        .extension()
        .and_then(|s| s.to_str())
        .is_none_or(|s| !s.eq_ignore_ascii_case("html"))
    {
        return Err("Выбери файл отчёта с расширением .html".into());
    }
    let csv_path = path.with_extension("csv");
    let temporary = path.with_extension("html.tmp");
    let csv_temp = path.with_extension("csv.tmp");
    tokio::fs::write(&temporary, report).await.map_err(err)?;
    tokio::fs::write(&csv_temp, csv).await.map_err(err)?;
    tokio::fs::rename(&csv_temp, &csv_path).await.map_err(err)?;
    tokio::fs::rename(&temporary, path).await.map_err(err)?;
    Ok(Export {
        html: path.display().to_string(),
        csv: csv_path.display().to_string(),
    })
}
/// Отчёт работает в отдельном webview без прав IPC главного окна.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn campaign_report_open(
    app: crate::host::AppHandle,
    state: State<'_, crate::state::AppState>,
    campaign_id: i64,
) -> Result<String> {
    let folder = state.data_dir.join("ad-reports");
    tokio::fs::create_dir_all(&folder).await.map_err(err)?;
    let path = folder.join(format!("campaign-{campaign_id}.html"));
    campaign_report_export(state.clone(), campaign_id, path.display().to_string()).await?;
    let url = format!(
        "{}&v={}",
        state.media.register(path),
        chrono::Utc::now().timestamp_millis()
    );
    let url = url.parse().map_err(err)?;
    let label = format!("campaign-report-{campaign_id}");
    if let Some(window) = app.get_webview_window(&label) {
        window.navigate(url).map_err(err)?;
        window.set_focus().map_err(err)?;
    } else {
        tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::External(url))
            .title("Автонарезка · отчёт кампании")
            .inner_size(1200.0, 850.0)
            .build()
            .map_err(err)?;
    }
    Ok(label)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_escapes_delimiters_quotes_formulas_and_html_escapes_data() {
        assert_eq!(csv_cell("=SUM(A1;A2)"), "\"'=SUM(A1;A2)\"");
        assert_eq!(csv_cell("a\";b"), "\"a\"\";b\"");
        assert_eq!(html("<script>\"&"), "&lt;script&gt;&quot;&amp;");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }
    #[test]
    fn totals_are_unique_clips_and_confirmed_publications_and_no_fake_metrics() {
        let c = Campaign {
            name: "<Кампания>".into(),
            advertiser: "Реклама".into(),
            ..Default::default()
        };
        let mut rows = vec![];
        for state in ["done", "needs_check"] {
            rows.push(Row {
                draft_id: 1,
                campaign: c.name.clone(),
                advertiser: c.advertiser.clone(),
                banner: "Баннер".into(),
                title: "<Название>".into(),
                platform: Some("telegram".into()),
                account: Some("Аккаунт".into()),
                state: Some(state.into()),
                sent_at: None,
                url: Some("javascript:alert(1)".into()),
                caption: None,
                poster: None,
                views: None,
                measured_at: None,
                metric_source: None,
            });
        }
        let (h, csv) = render(&c, &rows);
        assert!(
            h.contains("Создано роликов: <strong>1")
                && h.contains("Подтверждённых публикаций: <strong>1")
        );
        assert!(h.contains("&lt;Название&gt;"));
        assert!(!h.contains("href=\"javascript:"));
        assert_eq!(csv.lines().count(), 3);
        assert!(!h.contains("Просмотры: 0"));
    }
}
