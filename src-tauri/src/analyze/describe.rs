//! Длинное описание ролика для TikTok: текст по расшифровке от языковой модели, хэштеги и точная подгонка длины.
//!
//! Модель пишет по секциям (о чём, контекст, вопросы зрителям, итог) с запасом. Дальше чистая функция [`fit_description`]
//! убирает лишние предложения так, чтобы текст вместе с хэштегами занял ровно 3990–3999 символов: модель считать
//! символы не умеет, поэтому длину обеспечивает код, а не промпт.
//!
//! Всё, что попадает в текст, — буквы, цифры и простая пунктуация из BMP. Тогда длина в символах, в единицах UTF-16 и в
//! графемах совпадает, и неважно, как именно считает счётчик TikTok.

use anyhow::{anyhow, bail};

use super::llm::{LlmClient, Sampling, MAX_TRANSCRIPT_CHARS};
use crate::tiktok::api::{len_utf16, make_caption};

/// Окно длины: подпись вместе с хэштегами, в единицах UTF-16.
pub const MIN_LEN: usize = 3990;
pub const MAX_LEN: usize = 3999;
pub const MAX_TAGS: usize = 5;

const MIN_SENTENCE: usize = 12;
const MAX_SENTENCE: usize = 300;
const POOL_CAP: usize = 200;
/// Сколько знаков речи из ролика кладём в описание.
const SPEECH_CAP: usize = 1800;
/// Сколько токенов даём на одну секцию (≈2000 русских символов). Запрос с расшифровкой занимает до 1700 токенов, контекст 4096.
const SECTION_TOKENS: u32 = 800;
/// Запас на «растяжку» знаков конца предложений при подгонке: три «.»→«...» = +6.
const TUNE: i64 = 6;

/// Текст описания: только разрешённые символы, без `#`, эмодзи и переводов строки.
pub const TEXT_GRAMMAR: &str = "root ::= char+\nchar ::= [а-яА-ЯёЁa-zA-Z0-9 ,.!?:;()«»—–…%-]\n";

/// От 5 до 8 хэштегов в JSON-массиве: лишние нужны подгонке длины. Каждый — одно русское слово строчными буквами:
/// без подчёркиваний, цифр и латиницы. С разрешённой латиницей модель выдала английские теги не про ролик (`#useful #cute`).
pub const TAGS_GRAMMAR: &str = "root ::= \"[\" tag (\",\" ws tag){4,7} \"]\"\ntag ::= \"\\\"\" [а-яё]{3,20} \"\\\"\"\nws ::= \" \"?\n";

const SECTIONS: [&str; 4] = [
    "Перескажи, о чём этот ролик: что происходит, кто участвует и чем всё заканчивается. Пиши как обычный рассказ, без заголовков и списков.",
    "Расскажи подробнее о происходящем: что было до этого, о чём говорят герои, почему это забавно, остро или важно. Без заголовков и списков.",
    "Задай зрителям 4–5 вопросов по теме ролика и позови обсудить в комментариях, у кого как бывает.",
    "Подведи итог одной мыслью и позови досмотреть до конца, поделиться и подписаться, чтобы не пропустить продолжение.",
];

/// Если после четырёх секций текста мало — просим подробностей. Каждая такая секция даёт ещё около 1500 символов.
const EXTRA: &str = "Добавь ещё подробностей и наблюдений о происходящем в ролике, не повторяя уже написанное. Без заголовков и списков.";
const MAX_EXTRA: usize = 3;

// ---------------------------------------------------------------- чистка и предложения

fn allowed(c: char) -> bool {
    matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё' | 'a'..='z' | 'A'..='Z' | '0'..='9' | ' ' | ',' | '.' | '!' | '?' | ':' | ';' | '(' | ')' | '«' | '»' | '—' | '–' | '…' | '%' | '-')
}

fn terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}

/// Оставляет только разрешённые символы; переводы строки и табуляции становятся пробелами, пробелы схлопываются.
pub fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut space = true;
    for c in raw.chars() {
        let c = if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c };
        if !allowed(c) {
            continue;
        }
        if c == ' ' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out.trim().to_string()
}

/// Конец предложения к единому виду: один знак. Многоточие (три точки и больше или «…») — «…», иначе первый знак серии.
fn finalize(s: &str) -> Option<String> {
    let s = s.trim();
    let core = s.trim_end_matches(terminal).trim_end();
    let run = &s[core.len()..].trim_start();
    let first = run.chars().next()?;
    let mark = if run.contains('…') || run.chars().filter(|c| *c == '.').count() >= 3 { '…' } else { first };
    let n = len_utf16(core);
    if n + 1 < MIN_SENTENCE || n + 1 > MAX_SENTENCE || !core.chars().any(char::is_alphabetic) {
        return None;
    }
    Some(format!("{core}{mark}"))
}

/// Законченные предложения. Хвост без знака конца (обрыв по лимиту токенов) отбрасывается, повторы тоже.
pub fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        cur.push(c);
        if terminal(c) {
            while i + 1 < chars.len() && terminal(chars[i + 1]) {
                i += 1;
                cur.push(chars[i]);
            }
            // «3.5» и «т.е.» не конец: граница только перед пробелом или в конце текста
            if i + 1 >= chars.len() || chars[i + 1] == ' ' {
                if let Some(s) = finalize(&cur) {
                    if !out.iter().any(|o| o.to_lowercase() == s.to_lowercase()) {
                        out.push(s);
                    }
                }
                cur.clear();
            }
        }
        i += 1;
    }
    out
}

/// Хэштеги из ответа модели: JSON-массив слов → `#слово`, без повторов. Только буквы и цифры одного алфавита:
/// слово со смесью кириллицы и латиницы почти наверняка испорчено моделью.
pub fn parse_tags(raw: &str) -> Vec<String> {
    let words: Vec<String> = serde_json::from_str(raw.trim()).unwrap_or_default();
    let mut kept: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for w in &words {
        let w: String = w.trim().trim_start_matches('#').chars().filter(|c| c.is_alphanumeric()).collect();
        let cyr = w.chars().any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'));
        let lat = w.chars().any(|c| c.is_ascii_alphabetic());
        let lower = w.to_lowercase();
        // «#котнаплите» и «#котнаплитечайник» — один тег с хвостом: второй не нужен
        let variant = kept.iter().any(|k| lower.starts_with(k.as_str()) || k.starts_with(&lower));
        if (3..=24).contains(&w.chars().count()) && w.chars().all(allowed) && !(cyr && lat) && !variant {
            kept.push(lower);
            out.push(format!("#{w}"));
        }
    }
    out
}

/// Предложения из настоящей речи ролика, не больше `SPEECH_CAP` знаков. Это связный текст по делу, в отличие от сочинённого
/// маленькой моделью: на нём описание держится, а модель пишет только вступление, вопросы зрителям и призыв.
pub fn speech_sentences(transcript: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut total = 0;
    for s in split_sentences(&clean(transcript)) {
        total += len_utf16(&s) + 1;
        if total > SPEECH_CAP {
            break;
        }
        out.push(s);
    }
    out
}

// ---------------------------------------------------------------- подгонка длины

#[derive(Debug, PartialEq, Eq)]
pub enum FitError {
    /// Всего текста меньше нужного: нужно написать ещё.
    TooShort { have: usize },
    /// Текста хватает, но попасть в окно не вышло ни с одним набором хэштегов.
    NoFit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Description {
    pub caption: String,
    pub hashtags: Vec<String>,
    /// Длина подписи с хэштегами, как её соберёт `make_caption`.
    pub len: usize,
}

/// Сочетания по `k` из `n` в лексикографическом порядке, не больше `cap` штук.
fn combinations(n: usize, k: usize, cap: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    if k == 0 || k > n {
        return out;
    }
    let mut idx: Vec<usize> = (0..k).collect();
    loop {
        out.push(idx.clone());
        if out.len() >= cap {
            break;
        }
        let mut i = k;
        while i > 0 && idx[i - 1] == n - k + i - 1 {
            i -= 1;
        }
        if i == 0 {
            break;
        }
        idx[i - 1] += 1;
        for j in i..k {
            idx[j] = idx[j - 1] + 1;
        }
    }
    out
}

/// Какие предложения (кроме первого и последнего) убрать, чтобы оставшаяся стоимость попала в `[exact_lo - tune, hi]`.
/// Убираем как можно меньше предложений и предпочитаем попадание без «растяжки». `None` — не выходит.
fn choose_drops(costs: &[usize], exact_lo: i64, tune: i64, hi: i64) -> Option<Vec<usize>> {
    let total: usize = costs.iter().sum();
    let d_min = (total as i64 - hi).max(0) as usize;
    let d_max_i = total as i64 - (exact_lo - tune);
    if d_max_i < d_min as i64 {
        return None;
    }
    let d_max = (d_max_i as usize).min(total);
    if d_max < d_min {
        return None;
    }
    let items: Vec<usize> = if costs.len() > 2 { (1..costs.len() - 1).collect() } else { vec![] };
    const INF: u16 = u16::MAX;
    let mut dp = vec![INF; d_max + 1];
    dp[0] = 0;
    let mut changed = vec![vec![false; d_max + 1]; items.len()];
    for (ii, &i) in items.iter().enumerate() {
        let c = costs[i];
        if c > d_max {
            continue;
        }
        for d in (c..=d_max).rev() {
            if dp[d - c] != INF && dp[d - c] + 1 < dp[d] {
                dp[d] = dp[d - c] + 1;
                changed[ii][d] = true;
            }
        }
    }
    let best = (d_min..=d_max)
        .filter(|&d| dp[d] != INF)
        .min_by_key(|&d| (((total - d) as i64) < exact_lo, dp[d], d))?;
    let mut dropped = Vec::new();
    let mut d = best;
    for ii in (0..items.len()).rev() {
        if changed[ii][d] {
            dropped.push(items[ii]);
            d -= costs[items[ii]];
        }
    }
    dropped.reverse();
    Some(dropped)
}

/// Удлиняет знаки конца на `need` единиц: «.»→«...» (+2), «!»→«!!» и «?»→«??» (+1). `None`, если ровно не вышло.
fn inflate(sentences: &mut [String], mut need: usize) -> Option<()> {
    for s in sentences.iter_mut() {
        if need == 0 {
            break;
        }
        match s.chars().last() {
            Some('.') if need >= 2 => {
                s.pop();
                s.push_str("...");
                need -= 2;
            }
            Some(m @ ('!' | '?')) => {
                s.push(m);
                need -= 1;
            }
            _ => {}
        }
    }
    (need == 0).then_some(())
}

/// Собирает описание из секций (в каждой предложения по порядку) и набора хэштегов так, чтобы подпись с хэштегами,
/// какой её соберёт [`make_caption`], заняла [`MIN_LEN`]..=[`MAX_LEN`]. Предложения идут в исходном порядке,
/// первое и последнее остаются всегда, секции — абзацы. Берётся до [`MAX_TAGS`] хэштегов.
pub fn fit_description(sections: &[Vec<String>], tags: &[String]) -> Result<Description, FitError> {
    let flat: Vec<(usize, &String)> = sections.iter().enumerate().flat_map(|(si, v)| v.iter().map(move |s| (si, s))).take(POOL_CAP).collect();
    // каждое предложение стоит свою длину и один разделитель; ещё один разделитель стоит перед хэштегами
    let costs: Vec<usize> = flat.iter().map(|(_, s)| len_utf16(s) + 1).collect();
    let total: usize = costs.iter().sum();
    let tags: Vec<String> = crate::tiktok::api::normalize_hashtags(tags);
    if flat.len() < 2 {
        return Err(FitError::TooShort { have: total });
    }
    if tags.is_empty() {
        return Err(FitError::NoFit);
    }

    let mut longest_possible = 0;
    for k in (1..=tags.len().min(MAX_TAGS)).rev() {
        for pick in combinations(tags.len(), k, 60) {
            let chosen: Vec<String> = pick.iter().map(|&i| tags[i].clone()).collect();
            let tags_len: usize = chosen.iter().map(|t| len_utf16(t)).sum::<usize>() + chosen.len() - 1;
            longest_possible = longest_possible.max(total + tags_len);
            for tune in [0, TUNE] {
                let Some(dropped) = choose_drops(&costs, MIN_LEN as i64 - tags_len as i64, tune, MAX_LEN as i64 - tags_len as i64) else { continue };
                let mut kept: Vec<(usize, String)> = flat.iter().enumerate().filter(|(i, _)| !dropped.contains(i)).map(|(_, (si, s))| (*si, (*s).clone())).collect();
                let render = |kept: &[(usize, String)]| {
                    let mut body = String::new();
                    for (n, (si, s)) in kept.iter().enumerate() {
                        if n > 0 {
                            body.push(if kept[n - 1].0 == *si { ' ' } else { '\n' });
                        }
                        body.push_str(s);
                    }
                    body
                };
                let mut len = len_utf16(&make_caption(&render(&kept), &chosen, usize::MAX));
                if len < MIN_LEN {
                    let mut texts: Vec<String> = kept.iter().map(|(_, s)| s.clone()).collect();
                    if inflate(&mut texts, MIN_LEN - len).is_none() {
                        continue;
                    }
                    for (slot, t) in kept.iter_mut().zip(texts) {
                        slot.1 = t;
                    }
                    len = len_utf16(&make_caption(&render(&kept), &chosen, usize::MAX));
                }
                if (MIN_LEN..=MAX_LEN).contains(&len) {
                    return Ok(Description { caption: render(&kept), hashtags: chosen, len });
                }
            }
        }
    }
    if longest_possible + TUNE as usize * 2 < MIN_LEN {
        Err(FitError::TooShort { have: longest_possible })
    } else {
        Err(FitError::NoFit)
    }
}

// ---------------------------------------------------------------- сходство текстов

fn trigrams(text: &str) -> std::collections::HashSet<(String, String, String)> {
    let words: Vec<String> = text.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect();
    words.windows(3).map(|w| (w[0].clone(), w[1].clone(), w[2].clone())).collect()
}

/// Сходство двух текстов по словесным триграммам, 0..1. Больше половины — ролики получили почти одно и то же описание.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (trigrams(a), trigrams(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(&b).count() as f64 / a.union(&b).count() as f64
}

// ---------------------------------------------------------------- запросы к модели

fn chat(system: &str, user: &str) -> String {
    format!("<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n")
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn about(title: &str, transcript: &str) -> String {
    let t = transcript.trim();
    if t.is_empty() {
        format!("Видео называется «{}». Расшифровки речи нет, опирайся на название.", title.trim())
    } else {
        format!("Видео называется «{}». Расшифровка речи:\n{}", title.trim(), clip(t, MAX_TRANSCRIPT_CHARS))
    }
}

/// Запрос на одну секцию описания. `written` — уже написанное, чтобы модель не повторялась.
pub fn section_prompt(title: &str, transcript: &str, instruction: &str, written: &str) -> String {
    let mut user = format!(
        "{}\n\nЗадача: {instruction}\nПиши связным текстом, законченными предложениями по 8–25 слов, примерно 1200–1500 символов. Только обычные слова и знаки препинания.",
        about(title, transcript)
    );
    let w = written.trim();
    if !w.is_empty() {
        let tail: String = w.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
        user.push_str(&format!("\n\nУже написано (не повторяй): …{tail}"));
    }
    chat("Ты автор описаний к коротким видео в TikTok. Пишешь по-русски, грамотно, живо и по делу, без эмодзи и хэштегов. Опираешься только на то, что есть в расшифровке и названии, ничего не выдумываешь. Если пол героя неизвестен, пишешь «герой» или «человек». Предложения простые и законченные, слова согласованы. Отвечай только текстом описания.", &user)
}

pub fn tags_prompt(title: &str, transcript: &str) -> String {
    chat(
        "Ты помогаешь продвигать видео в TikTok. Отвечай только JSON-массивом строк.",
        &format!(
            "{}\n\nПридумай от 5 до 8 хэштегов, по которым это видео находят в TikTok: одно русское слово строчными буквами, от 3 до 20 букв, без символа #, пробелов и подчёркиваний, по теме именно этого видео. Верни JSON-массив строк.",
            about(title, transcript)
        ),
    )
}

fn creative(seed: u32) -> Sampling {
    Sampling { temperature: 0.55, seed: Some(seed), repeat_penalty: Some(1.12) }
}

/// Пишет описание для одного ролика. `seed` разный у разных роликов и попыток: так тексты не совпадают.
/// `on` получает короткие сообщения о ходе работы.
pub async fn describe(client: &mut LlmClient, title: &str, transcript: &str, seed: u32, on: &mut (dyn FnMut(&str) + Send)) -> anyhow::Result<Description> {
    on("Подбираю хэштеги");
    let tags_raw = client.complete_with(&tags_prompt(title, transcript), TAGS_GRAMMAR, 120, Sampling { temperature: 0.5, seed: Some(seed), repeat_penalty: None }).await?;
    let tags = parse_tags(&tags_raw);
    if tags.len() < 3 {
        bail!("модель не придумала хэштеги: {}", clip(&tags_raw, 120));
    }

    let mut sections: Vec<Vec<String>> = Vec::new();
    let mut written = String::new();
    let mut last_err = FitError::NoFit;
    let speech = speech_sentences(transcript);
    for round in 0..SECTIONS.len() + MAX_EXTRA {
        let (instruction, label) = match SECTIONS.get(round) {
            Some(s) => (*s, format!("Пишу описание, часть {} из {}", round + 1, SECTIONS.len())),
            None => (EXTRA, format!("Дописываю подробности ({})", round + 1 - SECTIONS.len())),
        };
        on(&label);
        let raw = client.complete_with(&section_prompt(title, transcript, instruction, &written), TEXT_GRAMMAR, SECTION_TOKENS, creative(seed.wrapping_add(round as u32 * 7 + 1))).await?;
        let sentences = split_sentences(&clean(&raw));
        // повтор уже написанного пропускаем целиком
        let fresh: Vec<String> = sentences.into_iter().filter(|s| !sections.iter().flatten().any(|o| o.to_lowercase() == s.to_lowercase())).collect();
        if !fresh.is_empty() {
            written.push(' ');
            written.push_str(&fresh.join(" "));
            // дописанное идёт в последний абзац перед итогом, чтобы призыв остался в конце
            if round >= SECTIONS.len() {
                let at = sections.len().saturating_sub(1);
                sections.insert(at, fresh);
            } else {
                sections.push(fresh);
            }
        }
        if round == 0 && !speech.is_empty() {
            // сразу после вступления — то, что на самом деле сказано в ролике
            sections.push(speech.clone());
        }
        if round + 1 < SECTIONS.len() || sections.iter().map(Vec::len).sum::<usize>() < 2 {
            continue;
        }
        match fit_description(&sections, &tags) {
            Ok(d) => return Ok(d),
            Err(e) => last_err = e,
        }
    }
    match last_err {
        FitError::TooShort { have } => Err(anyhow!("модель написала слишком мало текста ({have} символов), нужно {MIN_LEN}–{MAX_LEN}")),
        FitError::NoFit => Err(anyhow!("не удалось уложить описание в {MIN_LEN}–{MAX_LEN} символов")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Простой детерминированный «случайный» ряд: тесты не зависят от внешних крейтов.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, lo: usize, hi: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            lo + ((self.0 >> 33) as usize) % (hi - lo + 1)
        }
    }

    fn sentence(r: &mut Lcg, id: usize) -> String {
        let words = r.next(4, 24);
        let mut s = format!("Фраза{id}");
        for _ in 0..words {
            s.push(' ');
            for _ in 0..r.next(2, 9) {
                s.push(['а', 'о', 'е', 'и', 'н', 'т', 'р', 'с'][r.next(0, 7)]);
            }
        }
        s.push(['.', '.', '.', '!', '?', '…'][r.next(0, 5)]);
        s
    }

    fn pool(seed: u64, target: usize) -> Vec<Vec<String>> {
        let mut r = Lcg(seed);
        let mut sections = vec![Vec::new(); 4];
        let (mut id, mut sum) = (0, 0);
        while sum < target {
            let s = sentence(&mut r, id);
            sum += len_utf16(&s) + 1;
            sections[(id * 4 / 40).min(3)].push(s);
            id += 1;
        }
        sections
    }

    fn tag_set(seed: u64) -> Vec<String> {
        let mut r = Lcg(seed);
        (0..8).map(|i| format!("#тег{}{}", i, "а".repeat(r.next(2, 14)))).collect()
    }

    #[test]
    fn clean_drops_everything_outside_the_allowed_alphabet() {
        assert_eq!(clean("Привет,\n\n мир!  #хэш 😀 «да» \"x\" ’y’"), "Привет, мир! хэш «да» x y");
        let c = clean("а😀б\u{200d}в");
        assert_eq!(c, "абв");
        assert!(clean("ёЁ—–…%()").chars().all(allowed));
    }

    #[test]
    fn sentences_are_split_normalised_and_cut_off_tails_dropped() {
        let s = split_sentences("Первая фраза тут!!! Вторая фраза здесь... Третья фраза в 3.5 раза длиннее? Обрыв без знака");
        assert_eq!(s, vec!["Первая фраза тут!", "Вторая фраза здесь…", "Третья фраза в 3.5 раза длиннее?"]);
        assert!(split_sentences("Коротко. Ещё.").is_empty(), "слишком короткие не берём");
        assert_eq!(split_sentences("Одна и та же фраза. Одна и та же фраза.").len(), 1, "повторы убираем");
    }

    #[test]
    fn tags_come_out_unique_hash_prefixed_and_one_alphabet_only() {
        let t = parse_tags(r##"["#Юмор","юмор","смех шоу","пр","сериал_30","a b","тiktok","tiktok","кот_на_плите","котнаплитечайник"]"##);
        assert_eq!(t, vec!["#Юмор", "#смехшоу", "#сериал30", "#tiktok", "#котнаплите"], "подчёркивания и пробелы убраны, смесь алфавитов, повторы и варианты с хвостом отброшены");
        assert!(parse_tags("не json").is_empty());
    }

    #[test]
    fn speech_goes_in_as_clean_sentences_up_to_the_cap() {
        let s = speech_sentences("Привет всем, это наш новый выпуск! А вот «цитата» — с эмодзи 😀 и #хэштегом. Да.");
        assert_eq!(s, vec!["Привет всем, это наш новый выпуск!", "А вот «цитата» — с эмодзи и хэштегом."]);
        let long = "Это ещё одна довольно длинная фраза из речи героя. ".repeat(80);
        let cut = speech_sentences(&long);
        assert!(!cut.is_empty() && cut.iter().map(|x| len_utf16(x) + 1).sum::<usize>() <= SPEECH_CAP);
        assert!(speech_sentences("").is_empty());
    }

    #[test]
    fn combinations_are_complete_ordered_and_capped() {
        assert_eq!(combinations(4, 2, 100), vec![vec![0, 1], vec![0, 2], vec![0, 3], vec![1, 2], vec![1, 3], vec![2, 3]]);
        assert_eq!(combinations(8, 5, 100).len(), 56);
        assert_eq!(combinations(8, 5, 10).len(), 10);
        assert!(combinations(2, 3, 10).is_empty());
    }

    #[test]
    fn fitted_description_always_lands_in_the_window() {
        let mut inflated = 0;
        for seed in 0..300u64 {
            let sections = pool(seed, 4300 + (seed as usize % 7) * 400);
            let d = fit_description(&sections, &tag_set(seed)).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
            let full = make_caption(&d.caption, &d.hashtags, usize::MAX);
            assert_eq!(len_utf16(&full), d.len, "seed {seed}");
            assert!((MIN_LEN..=MAX_LEN).contains(&d.len), "seed {seed}: {}", d.len);
            assert_eq!(full.chars().count(), d.len, "BMP: длина в символах и в UTF-16 совпадает");
            assert!(d.hashtags.len() <= MAX_TAGS && !d.hashtags.is_empty(), "seed {seed}");
            assert!(!d.caption.contains('#'), "хэштеги только в конце");
            assert!(d.caption.chars().all(|c| allowed(c) || c == '\n' || c == '…' || c == '.'), "seed {seed}");
            assert!(d.caption.starts_with(&sections[0][0]), "первое предложение остаётся");
            if d.caption.contains("...") || d.caption.contains("!!") || d.caption.contains("??") {
                inflated += 1;
            }
        }
        assert!(inflated <= 15, "подгонка растяжкой — запасной путь, а не обычный, но понадобилась в {inflated} из 300");
    }

    #[test]
    fn last_sentence_and_order_survive_dropping() {
        let core = |s: &str| s.trim_end_matches(['.', '!', '?', '…']).to_string();
        let sections = pool(7, 6000);
        let d = fit_description(&sections, &tag_set(7)).unwrap();
        assert!(d.caption.contains(&core(sections[3].last().unwrap())), "итоговое предложение остаётся");
        // оставшиеся предложения идут в исходном порядке
        let (mut at, mut found) = (0, 0);
        for s in sections.iter().flatten() {
            if let Some(p) = d.caption[at..].find(&core(s)) {
                at += p + core(s).len();
                found += 1;
            }
        }
        assert!(found >= 20, "нашлось только {found} предложений по порядку");
        assert!(found < sections.iter().map(Vec::len).sum::<usize>(), "пул больше окна: часть предложений убрана");
    }

    #[test]
    fn a_pool_that_is_too_short_asks_for_more_text() {
        let sections = pool(1, 1500);
        match fit_description(&sections, &tag_set(1)) {
            Err(FitError::TooShort { have }) => assert!(have < MIN_LEN, "{have}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(fit_description(&[vec!["Одна единственная фраза тут.".into()]], &tag_set(1)), Err(FitError::TooShort { .. })));
    }

    #[test]
    fn tags_are_capped_at_five_and_normalised() {
        let tags: Vec<String> = (0..8).map(|i| format!("тег{i}аб")).collect();
        let d = fit_description(&pool(3, 5000), &tags).unwrap();
        assert!(d.hashtags.len() <= 5);
        assert!(d.hashtags.iter().all(|t| t.starts_with('#') && !t[1..].contains('#')));
    }

    #[test]
    fn similarity_tells_copies_from_different_texts() {
        let a = "это длинное описание про то как герой пришёл домой и увидел сюрприз на столе";
        assert!(similarity(a, a) > 0.99);
        assert!(similarity(a, "совсем другой текст про погоду в горах и про то как идёт дождь весь день") < 0.1);
        assert_eq!(similarity("", a), 0.0);
        let near = "это длинное описание про то как герой пришёл домой и увидел подарок на столе";
        assert!(similarity(a, near) > 0.5);
    }

    #[test]
    fn prompts_carry_the_title_the_clipped_transcript_and_an_empty_thinking_block() {
        let long = "слово ".repeat(1500);
        let p = section_prompt("Он не ожидал", &long, SECTIONS[0], "");
        assert!(p.starts_with("<|im_start|>system") && p.ends_with("<think>\n\n</think>\n\n"));
        assert!(p.contains("«Он не ожидал»"));
        assert!(p.chars().count() < MAX_TRANSCRIPT_CHARS + 1200, "расшифровка обрезана до лимита");
        assert!(!p.contains("Уже написано"));
        let p2 = section_prompt("t", "", SECTIONS[1], "Написанный ранее кусок.");
        assert!(p2.contains("Расшифровки речи нет") && p2.contains("Уже написано") && p2.contains("Написанный ранее кусок."));
        assert!(tags_prompt("т", "текст").contains("JSON-массив"));
    }

    #[test]
    fn the_text_grammar_alphabet_matches_what_clean_allows() {
        // сам разбор грамматики делает llama.cpp в воркере (проверяется тестом с настоящей моделью);
        // здесь убеждаемся, что алфавит грамматики и фильтр `clean` не разошлись
        let class = TEXT_GRAMMAR.split('[').nth(1).unwrap().split(']').next().unwrap();
        for c in class.chars() {
            assert!(allowed(c), "символ {c:?} есть в грамматике, но clean его не пропустит");
        }
        for c in "яЯёЁzZ09 ,.!?:;()«»—–…%-".chars() {
            assert!(allowed(c));
        }
        assert!(!TEXT_GRAMMAR.contains('#') && !allowed('#') && !allowed('\n') && !allowed('😀'));
        assert!(TAGS_GRAMMAR.starts_with("root ::= \"[\""));
        assert!(TAGS_GRAMMAR.contains("[а-яё]{3,20}") && !TAGS_GRAMMAR.contains("a-z") && !TAGS_GRAMMAR.contains('_'), "теги: одно русское слово, без латиницы и подчёркиваний");
    }
}
