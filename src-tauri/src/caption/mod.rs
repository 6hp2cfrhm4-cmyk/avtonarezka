//! Подпись состоит из локального текста и обязательных полей профиля/кампании.
//! При сокращении теряется только тело; кредит, ссылка и промокод не обрезаются.
pub mod commands;
pub mod generate;
use crate::{
    publish::{PlatformId, INSTAGRAM, TELEGRAM, TIKTOK, YOUTUBE},
    tiktok::api::{len_utf16, normalize_hashtags},
};
use serde::{Deserialize, Serialize};
pub const TIKTOK_MIN: usize = 0;
pub const TIKTOK_MAX: usize = 3999;
pub const TIKTOK_TAGS: usize = 5;
pub const DEFAULT_TEMPLATE: &str = "{тело}\n\n{кредит}\n{ссылка}\n{промокод}\n{хэштеги}";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionSpec {
    pub platform: PlatformId,
    pub template: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Fields {
    pub title: String,
    pub body: String,
    pub credit: String,
    pub link: String,
    pub promo: String,
    pub hashtags: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Composed {
    pub text: String,
    pub len: usize,
    pub max: usize,
    pub min: usize,
    pub body_trimmed: bool,
}

fn render(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    // Однопроходная подстановка: скобки из текста модели не становятся placeholders.
    while let Some(pos) = rest.find('{') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        if let Some((end, value)) = values
            .iter()
            .find_map(|(key, v)| rest.starts_with(key).then_some((key.len(), v)))
        {
            out.push_str(value);
            rest = &rest[end..];
        } else {
            out.push('{');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    let mut lines = Vec::new();
    let mut blank = false;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !blank && !lines.is_empty() {
                lines.push("");
            }
            blank = true;
        } else {
            lines.push(line);
            blank = false;
        }
    }
    lines.join("\n").trim().to_string()
}
pub fn compose(spec: &CaptionSpec, fields: &Fields) -> Result<Composed, String> {
    compose_with_min(spec, fields, true)
}
pub(crate) fn compose_with_min(
    spec: &CaptionSpec,
    fields: &Fields,
    enforce_min: bool,
) -> Result<Composed, String> {
    let caps = match spec.platform {
        PlatformId::Tiktok => TIKTOK,
        PlatformId::Telegram => TELEGRAM,
        PlatformId::Youtube => YOUTUBE,
        PlatformId::Instagram => INSTAGRAM,
        _ => return Err("Профиль подписи этой площадки ещё не подключён".into()),
    };
    let min = if spec.platform == PlatformId::Tiktok {
        TIKTOK_MIN
    } else {
        0
    };
    if spec.platform == PlatformId::Youtube
        && (fields.title.trim().is_empty()
            || len_utf16(fields.title.trim()) > caps.title_max
            || fields.title.contains(['<', '>']))
    {
        return Err("Заголовок YouTube: 1–100 знаков, без угловых скобок".into());
    }
    let template = if spec.template.trim().is_empty() {
        DEFAULT_TEMPLATE
    } else {
        spec.template.trim()
    };
    let credit = fields.credit.trim();
    let credit = if credit.is_empty() || credit.starts_with("Источник:") {
        credit.to_string()
    } else {
        format!("Источник: {credit}")
    };
    let promo = fields.promo.trim();
    let promo = if promo.is_empty() {
        String::new()
    } else {
        format!("Промокод: {promo}")
    };
    let link = fields.link.trim().to_string();
    if !link.is_empty() {
        let url =
            reqwest::Url::parse(&link).map_err(|_| "Укажи полную рекламную ссылку".to_string())?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Рекламная ссылка должна начинаться с http:// или https://".into());
        }
    }
    let tags = normalize_hashtags(&fields.hashtags)
        .into_iter()
        .take(caps.hashtags_max)
        .collect::<Vec<_>>()
        .join(" ");
    let mut values = vec![
        ("{название}", fields.title.trim().into()),
        ("{тело}", String::new()),
        ("{кредит}", credit),
        ("{ссылка}", link),
        ("{промокод}", promo),
        ("{хэштеги}", tags),
    ];
    for (key, value) in &values {
        if *key != "{название}"
            && (!value.is_empty() || *key == "{тело}")
            && !template.contains(key)
        {
            return Err(format!("В шаблоне отсутствует обязательное поле {key}"));
        }
        if template.matches(key).count() > 1 {
            return Err(format!("Поле {key} повторяется в шаблоне"));
        }
    }
    let re = regex::Regex::new(r"\{[^{}]*\}").unwrap();
    for item in re.find_iter(template) {
        if !values.iter().any(|(key, _)| *key == item.as_str()) {
            return Err(format!("Неизвестное поле {}", item.as_str()));
        }
    }
    let base = render(template, &values);
    if len_utf16(&base) > caps.caption_max {
        return Err(
            "Название, кредит и реклама не помещаются в лимит подписи. Сократи обязательные поля"
                .into(),
        );
    }
    let body = fields.body.trim();
    // Длина зависит от пустых строк вокруг тела; ищем максимальный префикс без разрыва Unicode.
    let ends: Vec<usize> = std::iter::once(0)
        .chain(body.char_indices().map(|(i, c)| i + c.len_utf8()))
        .collect();
    let (mut lo, mut hi) = (0, ends.len());
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        values[1].1 = body[..ends[mid]].trim_end().into();
        if len_utf16(&render(template, &values)) <= caps.caption_max {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    values[1].1 = body[..ends[lo]].trim_end().into();
    let text = render(template, &values);
    let len = len_utf16(&text);
    if spec.platform == PlatformId::Youtube && text.contains(['<', '>']) {
        return Err("Описание YouTube не принимает угловые скобки".into());
    }
    if enforce_min && len < min {
        return Err(format!(
            "Для TikTok нужно ещё {} знаков текста: целевая длина {min}–{}",
            min - len,
            caps.caption_max
        ));
    }
    Ok(Composed {
        text,
        len,
        max: caps.caption_max,
        min,
        body_trimmed: ends[lo] < body.len(),
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn caption_preview(spec: CaptionSpec, fields: Fields) -> Result<Composed, String> {
    compose(&spec, &fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(p: PlatformId) -> CaptionSpec {
        CaptionSpec {
            platform: p,
            template: DEFAULT_TEMPLATE.into(),
        }
    }
    #[test]
    fn reserves_credit_url_promo_and_tags_when_body_is_long() {
        let f = Fields {
            title: "Кульминация".into(),
            body: "🙂Событие. ".repeat(1000),
            credit: "Автор".into(),
            link: "https://example.com/?utm_content=42".into(),
            promo: "ЗИМА20".into(),
            hashtags: vec!["тест".into()],
        };
        let c = compose(&spec(PlatformId::Telegram), &f).unwrap();
        assert!(c.len <= 1024 && c.body_trimmed);
        assert!(c.text.contains("Источник: Автор"));
        assert!(c.text.contains(&f.link));
        assert!(c.text.contains("Промокод: ЗИМА20"));
        assert!(c.text.ends_with("#тест"));
        assert!(serde_json::to_string(&c).is_ok());
    }
    #[test]
    fn tiktok_allows_empty_or_short_text_and_only_enforces_the_upper_limit() {
        let f = Fields {
            body: "Русское содержание. ".repeat(600),
            credit: "стрим".into(),
            ..Default::default()
        };
        let c = compose(&spec(PlatformId::Tiktok), &f).unwrap();
        assert!(c.len <= 3999);
        assert!(c.text.ends_with("Источник: стрим"));
        assert_eq!(
            compose(&spec(PlatformId::Tiktok), &Fields::default())
                .unwrap()
                .text,
            ""
        );
        assert_eq!(
            compose(
                &spec(PlatformId::Tiktok),
                &Fields {
                    body: "Мой текст".into(),
                    ..Default::default()
                }
            )
            .unwrap()
            .text,
            "Мой текст"
        );
    }
    #[test]
    fn metadata_is_not_reinterpreted_or_silently_lost() {
        let f = Fields {
            body: "{ссылка} остаётся словами модели".into(),
            credit: "Автор".into(),
            ..Default::default()
        };
        let c = compose(&spec(PlatformId::Telegram), &f).unwrap();
        assert!(c.text.contains("{ссылка} остаётся"));
        assert!(compose(
            &CaptionSpec {
                template: "{тело}".into(),
                ..spec(PlatformId::Telegram)
            },
            &f
        )
        .is_err());
        assert!(compose(
            &CaptionSpec {
                template: "{тело} {ошибка}".into(),
                ..spec(PlatformId::Telegram)
            },
            &Fields::default()
        )
        .is_err());
        assert!(compose(
            &spec(PlatformId::Telegram),
            &Fields {
                link: "javascript:alert(1)".into(),
                ..Default::default()
            }
        )
        .is_err());
        assert!(compose(
            &spec(PlatformId::Telegram),
            &Fields {
                credit: "X".repeat(1100),
                ..Default::default()
            }
        )
        .is_err());
    }
    #[test]
    fn youtube_limits_title_and_description_without_losing_credit() {
        let fields = Fields {
            title: "Короткий ролик".into(),
            body: "🙂Содержание. ".repeat(1000),
            credit: "Автор".into(),
            ..Default::default()
        };
        let result = compose(&spec(PlatformId::Youtube), &fields).unwrap();
        assert!(result.len <= 5000 && result.body_trimmed);
        assert!(result.text.contains("Источник: Автор"));
        assert!(compose(
            &spec(PlatformId::Youtube),
            &Fields {
                title: "я".repeat(101),
                ..fields.clone()
            }
        )
        .is_err());
        assert!(compose(
            &spec(PlatformId::Youtube),
            &Fields {
                body: "<текст>".into(),
                ..fields
            }
        )
        .is_err());
    }
    #[test]
    fn instagram_reserves_credit_ad_and_thirty_hashtags() {
        let fields = Fields {
            body: "🙂Текст. ".repeat(1000),
            credit: "Автор".into(),
            link: "https://example.com/reels".into(),
            promo: "РЕАКЦИЯ".into(),
            hashtags: (0..40).map(|i| format!("тег{i}")).collect(),
            ..Default::default()
        };
        let c = compose(&spec(PlatformId::Instagram), &fields).unwrap();
        assert!(c.len <= 2200 && c.body_trimmed);
        assert!(c.text.contains("Источник: Автор"));
        assert!(c.text.contains(&fields.link));
        assert!(c.text.contains("Промокод: РЕАКЦИЯ"));
        assert_eq!(c.text.matches('#').count(), 30);
    }
}
