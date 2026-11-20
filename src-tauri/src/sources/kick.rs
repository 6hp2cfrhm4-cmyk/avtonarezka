//! Публичный архив Kick. yt-dlp поддерживает VOD, но его KickIE не перечисляет архив канала.
use super::{Result, SourceItem, SourceRef};
use std::{collections::HashSet, time::Duration};

pub struct Archive {
    pub base: String,
    pub client: reqwest::Client,
}
impl Default for Archive {
    fn default() -> Self {
        Self {
            base: "https://kick.com/api/v2".into(),
            client: reqwest::Client::new(),
        }
    }
}
impl Archive {
    pub async fn list(&self, source: &SourceRef) -> Result<Vec<SourceItem>> {
        let url = reqwest::Url::parse(&source.url).map_err(|e| e.to_string())?;
        let name = url
            .path_segments()
            .and_then(|mut p| p.next())
            .filter(|s| !s.is_empty())
            .ok_or("Не указан канал Kick")?;
        if source.kind != "kick"
            || !source.collection
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("Нужен публичный архив канала Kick".into());
        }
        let response = self
            .client
            .get(format!("{}/channels/{name}/videos", self.base))
            .header(
                reqwest::header::USER_AGENT,
                "Autonarezka/0.3.0 (local content factory)",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("Архив Kick: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Архив Kick: HTTP {}. Проверь доступность публичных записей",
                response.status().as_u16()
            ));
        }
        if response.content_length().is_some_and(|n| n > 2_000_000) {
            return Err("Слишком большой ответ архива Kick".into());
        }
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        if bytes.len() > 2_000_000 {
            return Err("Слишком большой ответ архива Kick".into());
        }
        parse(name, &bytes)
    }
}
pub fn parse(name: &str, bytes: &[u8]) -> Result<Vec<SourceItem>> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("Архив Kick: {e}"))?;
    let entries = v
        .as_array()
        .ok_or("Kick изменил формат публичного архива")?;
    let mut ids = HashSet::new();
    let mut out = vec![];
    for e in entries.iter().take(200) {
        let video = &e["video"];
        if e["is_live"].as_bool() != Some(false)
            || video["is_private"].as_bool() == Some(true)
            || video["deleted_at"].as_str().is_some()
            || video["is_pruned"].as_bool() == Some(true)
            || video["status"].as_str().is_some_and(|s| s != "public")
        {
            continue;
        }
        let Some(id) = video["uuid"]
            .as_str()
            .filter(|s| s.len() == 36 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
        else {
            continue;
        };
        if !ids.insert(id) {
            continue;
        }
        let source = super::classify("kick", &format!("https://kick.com/{name}/videos/{id}"))?;
        let published_at = video["created_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.timestamp());
        out.push(SourceItem {
            external_id: id.into(),
            source: source.url,
            title: e["session_title"].as_str().unwrap_or("Запись Kick").into(),
            duration: e["duration"]
                .as_f64()
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(|ms| ms / 1000.0),
            published_at,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_completed_vods_only_consistent_ids_and_milliseconds() {
        let entry = serde_json::json!({"is_live":false,"session_title":"Реакция","duration":12345000,"video":{"uuid":"100ca507-87a7-4745-b31a-5193e9feb4af","is_private":false,"status":"public","created_at":"2026-11-13T22:04:01Z"}});
        let mut private = entry.clone();
        private["video"]["is_private"] = true.into();
        let mut live = entry.clone();
        live["is_live"] = true.into();
        let mut deleted = entry.clone();
        deleted["video"]["deleted_at"] = "2026-11-14".into();
        let mut pruned = entry.clone();
        pruned["video"]["is_pruned"] = true.into();
        let bytes = serde_json::to_vec(&vec![private, live, deleted, pruned, entry.clone(), entry])
            .unwrap();
        let list = parse("xqc", &bytes).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].duration, Some(12345.0));
        assert_eq!(list[0].published_at, Some(1794607441));
        assert_eq!(list[0].external_id, "100ca507-87a7-4745-b31a-5193e9feb4af");
        assert_eq!(
            list[0].source,
            "https://kick.com/xqc/videos/100ca507-87a7-4745-b31a-5193e9feb4af"
        );
        assert!(parse("xqc", b"{}").is_err());
        assert!(parse("xqc", b"[]").unwrap().is_empty());
    }
}
