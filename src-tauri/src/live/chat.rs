//! Живой чат Twitch по IRC (только чтение, анонимный вход `justinfan…`): разбор строк и счётчик активности.
//! Тексты и имена не сохраняются: наружу уходят только числа за интервал.
use std::collections::HashSet;

/// Подсказки зрителей: слова, которые чаще пишут в момент, достойный клипа.
const HINTS: &[&str] = &["клип", "clip", "ахах", "хаха", "kekw", "lul", "pog", "pogchamp", "omegalul", "wtf", "ору", "лол", "ебать", "💀", "😂", "🤣"];

#[derive(Debug, PartialEq, Clone)]
pub enum Line {
    Ping(String),
    Message { author: String, text: String, emotes: usize },
    Other,
}

/// Одна строка IRC с тегами: `@emotes=25:0-4;... :nick!nick@nick.tmi.twitch.tv PRIVMSG #chan :текст`.
pub fn parse_line(raw: &str) -> Line {
    let raw = raw.trim_end_matches(['\r', '\n']);
    if let Some(rest) = raw.strip_prefix("PING") {
        return Line::Ping(rest.trim().trim_start_matches(':').to_string());
    }
    let (tags, rest) = match raw.strip_prefix('@') {
        Some(t) => t.split_once(' ').unwrap_or((t, "")),
        None => ("", raw),
    };
    let Some(rest) = rest.strip_prefix(':') else { return Line::Other };
    let Some((prefix, rest)) = rest.split_once(' ') else { return Line::Other };
    let Some(rest) = rest.strip_prefix("PRIVMSG ") else { return Line::Other };
    let Some((_, text)) = rest.split_once(" :") else { return Line::Other };
    let author = prefix.split('!').next().unwrap_or_default().to_string();
    let emotes = tags
        .split(';')
        .find_map(|kv| kv.strip_prefix("emotes="))
        .map(|v| if v.is_empty() { 0 } else { v.split('/').map(|e| e.split_once(':').map_or(0, |(_, r)| r.split(',').count())).sum() })
        .unwrap_or(0);
    Line::Message { author, text: text.to_string(), emotes }
}

pub fn has_hint(text: &str) -> bool {
    let t = text.to_lowercase();
    HINTS.iter().any(|h| t.contains(h))
}

/// Накопитель одного интервала.
#[derive(Default, Debug)]
pub struct Bucket {
    msgs: usize,
    with_emotes: usize,
    hints: usize,
    authors: HashSet<String>,
}

#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct Counts {
    pub msgs: f64,
    pub authors: f64,
    pub emotes: f64,
    pub hints: f64,
}

impl Bucket {
    pub fn add(&mut self, author: &str, text: &str, emotes: usize) {
        self.msgs += 1;
        if emotes > 0 {
            self.with_emotes += 1;
        }
        if has_hint(text) {
            self.hints += 1;
        }
        // имена нужны только для подсчёта разных людей и живут до конца интервала
        if self.authors.len() < 5000 {
            self.authors.insert(author.to_string());
        }
    }
    /// Снимает итог и обнуляет накопитель.
    pub fn take(&mut self) -> Counts {
        let c = Counts {
            msgs: self.msgs as f64,
            authors: self.authors.len() as f64,
            emotes: if self.msgs == 0 { 0.0 } else { self.with_emotes as f64 / self.msgs as f64 },
            hints: self.hints as f64,
        };
        *self = Bucket::default();
        c
    }
}

/// Строки входа в канал. Ник анонимный; возможностей `tags` достаточно для подсчёта смайлов.
pub fn handshake(channel: &str, nick_suffix: u32) -> Vec<String> {
    vec!["CAP REQ :twitch.tv/tags".into(), format!("NICK justinfan{nick_suffix}"), format!("JOIN #{}", channel.trim_start_matches('#').to_lowercase())]
}

/// Имя канала из ссылки `https://twitch.tv/name`.
pub fn twitch_channel(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    let host = u.host_str()?;
    if !(host == "twitch.tv" || host.ends_with(".twitch.tv")) {
        return None;
    }
    let name = u.path_segments()?.next()?.to_lowercase();
    (!name.is_empty() && name.len() <= 25 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !["videos", "directory", "clip"].contains(&name.as_str())).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_privmsg_with_tags_and_emotes() {
        let l = parse_line("@badges=;emotes=25:0-4,6-10/1902:12-16;id=1 :nick!nick@nick.tmi.twitch.tv PRIVMSG #chan :Kappa Kappa Keepo\r\n");
        assert_eq!(l, Line::Message { author: "nick".into(), text: "Kappa Kappa Keepo".into(), emotes: 3 });
        let none = parse_line("@emotes= :a!a@a.tmi.twitch.tv PRIVMSG #c :привет");
        assert_eq!(none, Line::Message { author: "a".into(), text: "привет".into(), emotes: 0 });
        // без тегов тоже работает, двоеточие внутри текста не ломает разбор
        assert_eq!(parse_line(":a!a@a PRIVMSG #c :время 12:30"), Line::Message { author: "a".into(), text: "время 12:30".into(), emotes: 0 });
    }

    #[test]
    fn ping_and_service_lines() {
        assert_eq!(parse_line("PING :tmi.twitch.tv\r\n"), Line::Ping("tmi.twitch.tv".into()));
        assert_eq!(parse_line(":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!"), Line::Other);
        assert_eq!(parse_line(":tmi.twitch.tv CAP * ACK :twitch.tv/tags"), Line::Other);
        assert_eq!(parse_line("мусор"), Line::Other);
        assert_eq!(parse_line(""), Line::Other);
    }

    #[test]
    fn bucket_counts_unique_authors_emotes_and_hints() {
        let mut b = Bucket::default();
        b.add("a", "ахах это КЛИП", 0);
        b.add("a", "ещё", 2);
        b.add("b", "KEKW", 1);
        b.add("c", "обычное", 0);
        let c = b.take();
        assert_eq!(c, Counts { msgs: 4.0, authors: 3.0, emotes: 0.5, hints: 2.0 });
        assert_eq!(b.take(), Counts::default(), "после снятия накопитель пуст");
    }

    #[test]
    fn handshake_is_anonymous_and_lowercase() {
        let h = handshake("#SomeStreamer", 4242);
        assert_eq!(h[1], "NICK justinfan4242");
        assert_eq!(h[2], "JOIN #somestreamer");
        assert!(h.iter().all(|l| !l.contains("PASS") && !l.contains("oauth")));
    }

    #[test]
    fn channel_from_url() {
        assert_eq!(twitch_channel("https://www.twitch.tv/ZXCursed").as_deref(), Some("zxcursed"));
        assert_eq!(twitch_channel("https://twitch.tv/videos/123"), None);
        assert_eq!(twitch_channel("https://evil.com/twitch.tv/x"), None);
        assert_eq!(twitch_channel("https://twitch.tv/"), None);
    }
}
