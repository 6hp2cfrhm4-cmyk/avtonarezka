//! Импорт сохранённого чата. Текст/имена не попадают в БД: остаются только кривые активности.
use super::{audio, HOP};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, path::Path};

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MESSAGES: usize = 1_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFeatures {
    pub format: String,
    pub messages: usize,
    pub ignored: usize,
    pub activity: Vec<f32>,
    pub emotes: Vec<f32>,
    pub authors: Vec<f32>,
    pub curve: Vec<f32>,
}
struct Message {
    at: f64,
    author: String,
    text: String,
    emotes: usize,
}

pub fn load(path: &Path, duration: f64) -> anyhow::Result<ChatFeatures> {
    let meta = std::fs::metadata(path).context("Файл чата недоступен")?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        bail!("Нужен файл чата не более 128 МиБ");
    }
    let bytes = std::fs::read(path).context("Не удалось прочитать чат")?;
    parse(&bytes, duration)
}

pub fn parse(bytes: &[u8], duration: f64) -> anyhow::Result<ChatFeatures> {
    if bytes.len() as u64 > MAX_BYTES
        || !duration.is_finite()
        || duration <= 0.0
        || duration > 86400.0
    {
        bail!("Некорректный размер чата или длительность записи");
    }
    let mut items = Vec::new();
    let mut ignored = 0;
    let format;
    // TwitchDownloader JSON (и явный best-effort Kick export).
    if let Ok(v) = serde_json::from_slice::<Value>(bytes) {
        if let Some(comments) = v.get("comments").and_then(Value::as_array) {
            format = "twitch";
            if comments.len() > MAX_MESSAGES {
                bail!("В чате больше миллиона сообщений");
            }
            for c in comments {
                let at = c.get("content_offset_seconds").and_then(Value::as_f64);
                let author = c
                    .pointer("/commenter/_id")
                    .or_else(|| c.pointer("/commenter/id"))
                    .or_else(|| c.pointer("/commenter/display_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let text = c
                    .pointer("/message/body")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let emotes = c
                    .pointer("/message/fragments")
                    .and_then(Value::as_array)
                    .map_or(0, |f| {
                        f.iter()
                            .filter(|x| x.get("emoticon").is_some_and(|x| !x.is_null()))
                            .count()
                    });
                push(&mut items, &mut ignored, at, author, text, emotes, duration);
            }
        } else if let Some(messages) = v.get("messages").and_then(Value::as_array) {
            format = "kick-export";
            if messages.len() > MAX_MESSAGES {
                bail!("В чате больше миллиона сообщений");
            }
            for c in messages {
                // Только явное время от начала VOD. Абсолютные created_at нельзя угадывать.
                let at = c
                    .get("offset_seconds")
                    .or_else(|| c.get("content_offset_seconds"))
                    .and_then(Value::as_f64);
                let author = c
                    .pointer("/sender/id")
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default();
                push(
                    &mut items,
                    &mut ignored,
                    at,
                    &author,
                    c.get("content").and_then(Value::as_str).unwrap_or(""),
                    0,
                    duration,
                );
            }
        } else if v.get("replayChatItemAction").is_some() {
            format = "youtube";
            youtube(&v, &mut items, &mut ignored, duration);
        } else {
            bail!("Неизвестный JSON чата: нужен TwitchDownloader, YouTube live_chat или Kick export с offset_seconds");
        }
    } else {
        format = "youtube";
        let text = std::str::from_utf8(bytes).context("Чат должен быть UTF-8 JSON/JSONL")?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value =
                serde_json::from_str(line).context("Повреждённая строка YouTube live_chat")?;
            youtube(&v, &mut items, &mut ignored, duration);
            if items.len() > MAX_MESSAGES {
                bail!("В чате больше миллиона сообщений");
            }
        }
    }
    if items.is_empty() {
        bail!("В файле нет сообщений с таймкодами внутри записи");
    }
    Ok(features(format, items, ignored, duration))
}

fn push(
    items: &mut Vec<Message>,
    ignored: &mut usize,
    at: Option<f64>,
    author: &str,
    text: &str,
    emotes: usize,
    duration: f64,
) {
    if let Some(at) = at.filter(|v| v.is_finite() && *v >= 0.0 && *v < duration) {
        // Не сохраняем гигантские строки/декоративные данные из внешнего файла.
        items.push(Message {
            at,
            author: author.chars().take(128).collect(),
            text: text.chars().take(4000).collect(),
            emotes,
        });
    } else {
        *ignored += 1;
    }
}
fn youtube(v: &Value, items: &mut Vec<Message>, ignored: &mut usize, duration: f64) {
    let Some(replay) = v.get("replayChatItemAction") else {
        *ignored += 1;
        return;
    };
    let offset = replay
        .get("videoOffsetTimeMsec")
        .and_then(|v| {
            v.as_str()
                .and_then(|v| v.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .map(|v| v / 1000.0);
    let Some(actions) = replay.get("actions").and_then(Value::as_array) else {
        return;
    };
    for action in actions {
        let Some(item) = action.pointer("/addChatItemAction/item") else {
            continue;
        };
        let Some(r) = item
            .get("liveChatTextMessageRenderer")
            .or_else(|| item.get("liveChatPaidMessageRenderer"))
        else {
            continue;
        };
        let author = r
            .get("authorExternalChannelId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let runs = r.pointer("/message/runs").and_then(Value::as_array);
        let text = runs
            .map(|runs| {
                runs.iter()
                    .filter_map(|v| v.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
            .unwrap_or_default();
        let emotes = runs.map_or(0, |runs| {
            runs.iter().filter(|v| v.get("emoji").is_some()).count()
        });
        push(items, ignored, offset, author, &text, emotes, duration);
    }
}
fn emote_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| {
            matches!(
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_ascii_lowercase()
                    .as_str(),
                "kekw"
                    | "lul"
                    | "lulw"
                    | "omegalul"
                    | "pog"
                    | "pogchamp"
                    | "poggers"
                    | "pepelaugh"
                    | "kappa"
                    | "хаха"
                    | "ахаха"
            )
        })
        .count()
}
fn features(format: &str, items: Vec<Message>, ignored: usize, duration: f64) -> ChatFeatures {
    // Пять секунд для подсчёта уникальных авторов; затем одна общая сетка HOP.
    let n = (duration / 5.0).ceil() as usize;
    let mut counts = vec![0.0f32; n];
    let mut emotes = counts.clone();
    let mut authors = vec![HashSet::new(); n];
    let messages = items.len();
    for m in items {
        let i = (m.at / 5.0) as usize;
        counts[i] += 1.0;
        emotes[i] += m.emotes.max(emote_words(&m.text)) as f32;
        if !m.author.is_empty() {
            authors[i].insert(m.author);
        }
    }
    let unique: Vec<_> = authors.iter().map(|a| a.len() as f32).collect();
    let expanded = |v: &[f32]| {
        let z = audio::robust_z(v);
        (0..(duration / HOP).ceil() as usize)
            .map(|i| z[((i as f64 * HOP) / 5.0) as usize])
            .collect::<Vec<_>>()
    };
    let activity = expanded(&counts);
    let emotes = expanded(&emotes);
    let authors = expanded(&unique);
    let curve = activity
        .iter()
        .zip(&emotes)
        .zip(&authors)
        .map(|((a, e), u)| 0.5 * a.max(0.0) + 0.25 * e.max(0.0) + 0.25 * u.max(0.0))
        .collect();
    ChatFeatures {
        format: format.into(),
        messages,
        ignored,
        activity,
        emotes,
        authors,
        curve,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn twitch_offsets_emotes_and_authors_peak() {
        let comments: Vec<_> = (0..40).map(|i| serde_json::json!({"content_offset_seconds":if i<5 {i as f64*10.0} else {120.0},"commenter":{"_id":i.to_string()},"message":{"body":"KEKW LUL","fragments":[]}})).collect();
        let f = parse(
            serde_json::json!({"comments":comments})
                .to_string()
                .as_bytes(),
            300.0,
        )
        .unwrap();
        assert_eq!(f.messages, 40);
        assert!(f.curve[240] > 4.0);
        assert!(f.authors[240] > 4.0);
        assert!(f.curve[100] < 0.01);
    }
    #[test]
    fn youtube_replay_uses_video_offset_not_wall_clock() {
        let line = serde_json::json!({"replayChatItemAction":{"videoOffsetTimeMsec":"12345","actions":[{"addChatItemAction":{"item":{"liveChatTextMessageRenderer":{"timestampUsec":"999999999999","authorExternalChannelId":"u","message":{"runs":[{"text":"Pog"},{"emoji":{"emojiId":"x"}}]}}}}}]}}).to_string();
        let f = parse(format!("{line}\n{line}\n").as_bytes(), 30.0).unwrap();
        assert_eq!((f.format.as_str(), f.messages), ("youtube", 2));
        assert!(f.curve[24] > 0.0);
        assert_eq!(f.curve[0], 0.0);
    }
    #[test]
    fn malformed_and_absolute_kick_times_are_rejected() {
        assert!(parse(b"{}", 30.0).is_err());
        assert!(parse(b"not json", 30.0).is_err());
        assert!(parse(
            br#"{"messages":[{"created_at":"2026-01-01","content":"hello"}]}"#,
            30.0
        )
        .is_err());
        let f = parse(br#"{"messages":[{"offset_seconds":2,"sender":{"id":7},"content":"Pog"},{"offset_seconds":-1}]}"#,30.0).unwrap();
        assert_eq!((f.messages, f.ignored), (1, 1));
    }
}
