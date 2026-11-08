//! Клиент LLM: промпт, грамматика JSON, разбор ответа и общение с процессом `llm-worker`.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

use super::Sentence;

/// Сколько символов расшифровки отдаём модели: дальше она не помещается в контекст.
pub const MAX_TRANSCRIPT_CHARS: usize = 3500;
pub const CONTEXT: u32 = 4096;
pub const MAX_TOKENS: u32 = 320;

/// Ответ модели. Поля идут в том же порядке, что и в грамматике.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Verdict {
    pub hook: u8,
    pub payoff: u8,
    pub standalone: u8,
    pub title: String,
    pub climax_t: f64,
    pub caption: String,
    pub hashtags: Vec<String>,
}

impl Verdict {
    /// Оценка 0..1: цепляющее начало, яркая кульминация и понятность без контекста.
    pub fn score(&self) -> f64 {
        (0.35 * self.hook as f64 + 0.35 * self.payoff as f64 + 0.30 * self.standalone as f64) / 10.0
    }
}

/// JSON строго нужной формы: модель физически не может ответить чем-то другим.
pub const GRAMMAR: &str = r#"root ::= "{" ws "\"hook\":" ws score "," ws "\"payoff\":" ws score "," ws "\"standalone\":" ws score "," ws "\"title\":" ws title "," ws "\"climax_t\":" ws num "," ws "\"caption\":" ws caption "," ws "\"hashtags\":" ws "[" ws tag ("," ws tag) ("," ws tag) ("," ws tag)? ("," ws tag)? ws "]" ws "}"
score ::= "10" | [1-9]
num ::= [0-9]+ ("." [0-9])?
char ::= [^"\\\n\r]
title ::= "\"" char{1,60} "\""
caption ::= "\"" char{1,160} "\""
tag ::= "\"" char{2,24} "\""
ws ::= [ \n]?
"#;

/// Запрос в формате чата Qwen3. Блок `<think>` оставлен пустым: рассуждения нам не нужны.
pub fn build_prompt(sentences: &[Sentence], clip_start: f64, clip_end: f64) -> String {
    let mut transcript = String::new();
    for s in sentences.iter().filter(|s| s.end > clip_start && s.start < clip_end) {
        let line = format!("[{:.1}] {}\n", (s.start - clip_start).max(0.0), s.text.trim());
        if transcript.chars().count() + line.chars().count() > MAX_TRANSCRIPT_CHARS {
            transcript.push_str("[…]\n");
            break;
        }
        transcript.push_str(&line);
    }
    format!(
        "<|im_start|>system\nТы редактор коротких вертикальных видео для TikTok. Отвечай только JSON.<|im_end|>\n\
         <|im_start|>user\nНиже расшифровка фрагмента длиной {:.0} с. В квадратных скобках указана секунда от начала фрагмента.\n\
         Оцени, получится ли из него удачный ролик, и верни JSON:\n\
         - hook — насколько цепляет начало, от 1 до 10;\n\
         - payoff — есть ли яркая кульминация, шутка или вывод, от 1 до 10;\n\
         - standalone — понятен ли фрагмент без остального видео, от 1 до 10;\n\
         - title — короткий заголовок до 60 символов;\n\
         - climax_t — секунда от начала фрагмента, где самый яркий момент (там покажем рекламный баннер);\n\
         - caption — подпись для TikTok до 160 символов;\n\
         - hashtags — от 3 до 5 хэштегов.\n\n\
         Фрагмент:\n{transcript}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n",
        clip_end - clip_start
    )
}

/// Разбор и проверка: оценки в 1..=10, кульминация внутри фрагмента, хэштеги с `#`.
pub fn parse_verdict(text: &str, clip_len: f64) -> anyhow::Result<Verdict> {
    let mut v: Verdict = serde_json::from_str(text.trim()).context("ответ модели не разобрался как JSON")?;
    for s in [&mut v.hook, &mut v.payoff, &mut v.standalone] {
        *s = (*s).clamp(1, 10);
    }
    v.climax_t = v.climax_t.clamp(0.0, clip_len);
    v.title = v.title.trim().to_string();
    v.caption = v.caption.trim().to_string();
    v.hashtags = v
        .hashtags
        .iter()
        .map(|t| t.trim().trim_start_matches('#').replace(' ', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("#{t}"))
        .collect();
    if v.title.is_empty() {
        bail!("модель вернула пустой заголовок");
    }
    Ok(v)
}

/// Воркер лежит рядом с исполняемым файлом приложения (а в тестах — на уровень выше `deps`).
pub fn find_worker() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "llm-worker.exe" } else { "llm-worker" };
    [dir.join(name), dir.parent()?.join(name)].into_iter().find(|p| p.is_file())
}

pub struct LlmClient {
    child: Child,
    stdin: ChildStdin,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    id: u64,
    text: Option<String>,
    error: Option<String>,
}

impl LlmClient {
    /// Запускает воркер и ждёт, пока модель загрузится.
    pub async fn start(model: &Path, threads: usize, gpu_layers: u32) -> anyhow::Result<Self> {
        let worker = find_worker().context("не найден llm-worker (соберите: cargo build -p llm-worker)")?;
        let mut child = crate::platform::command_at(worker)
            .args(["--model"])
            .arg(model)
            .args(["--threads", &threads.to_string(), "--gpu-layers", &gpu_layers.to_string(), "--ctx", &CONTEXT.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("не удалось запустить llm-worker")?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let first = stdout.next_line().await?.context("llm-worker завершился при загрузке модели")?;
        if !serde_json::from_str::<Reply>(&first).is_ok_and(|r| r.ready) {
            bail!("llm-worker не сообщил о готовности: {first}");
        }
        Ok(Self { child, stdin, stdout, next_id: 1 })
    }

    pub async fn complete(&mut self, prompt: &str, grammar: &str, max_tokens: u32) -> anyhow::Result<String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({ "id": id, "prompt": prompt, "grammar": grammar, "maxTokens": max_tokens, "temperature": 0.2 });
        self.stdin.write_all(format!("{req}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        let line = self.stdout.next_line().await?.context("llm-worker закрыл вывод")?;
        let reply: Reply = serde_json::from_str(&line).with_context(|| format!("непонятный ответ воркера: {line}"))?;
        if reply.id != id {
            bail!("ответ на другой запрос: {} вместо {id}", reply.id);
        }
        match (reply.text, reply.error) {
            (Some(t), _) => Ok(t),
            (_, Some(e)) => bail!("llm-worker: {e}"),
            _ => bail!("пустой ответ llm-worker"),
        }
    }

    pub async fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(start: f64, end: f64, text: &str) -> Sentence {
        Sentence { start, end, text: text.into() }
    }

    const GOOD: &str = r##"{"hook":8,"payoff":7,"standalone":9,"title":"Он не ожидал такого","climax_t":14.5,"caption":"Смотрите до конца","hashtags":["#юмор","смех","  лайфхак "]}"##;

    #[test]
    fn prompt_is_a_qwen_chat_with_empty_thinking_and_relative_times() {
        let p = build_prompt(&[sent(100.0, 104.0, "Привет."), sent(104.5, 108.0, "Это тест.")], 100.0, 110.0);
        assert!(p.starts_with("<|im_start|>system"));
        assert!(p.ends_with("<think>\n\n</think>\n\n"));
        assert!(p.contains("[0.0] Привет."));
        assert!(p.contains("[4.5] Это тест."));
        assert!(p.contains("длиной 10 с"));
    }

    #[test]
    fn long_transcripts_are_cut_to_fit_the_context() {
        let many: Vec<Sentence> = (0..400).map(|i| sent(i as f64, i as f64 + 0.9, &"слово ".repeat(12))).collect();
        let p = build_prompt(&many, 0.0, 400.0);
        assert!(p.contains("[…]"));
        assert!(p.chars().count() < MAX_TRANSCRIPT_CHARS + 1200);
    }

    #[test]
    fn sentences_outside_the_clip_are_ignored() {
        let p = build_prompt(&[sent(0.0, 5.0, "до"), sent(100.0, 105.0, "внутри"), sent(500.0, 505.0, "после")], 99.0, 110.0);
        assert!(p.contains("внутри") && !p.contains("до\n") && !p.contains("после"));
    }

    #[test]
    fn good_answer_is_parsed_and_tidied() {
        let v = parse_verdict(GOOD, 30.0).unwrap();
        assert_eq!((v.hook, v.payoff, v.standalone), (8, 7, 9));
        assert_eq!(v.hashtags, vec!["#юмор", "#смех", "#лайфхак"]);
        assert!((v.score() - (0.35 * 8.0 + 0.35 * 7.0 + 0.3 * 9.0) / 10.0).abs() < 1e-9);
    }

    #[test]
    fn climax_and_scores_are_clamped() {
        let v = parse_verdict(&GOOD.replace("14.5", "99.0").replace("\"hook\":8", "\"hook\":0"), 30.0).unwrap();
        assert_eq!(v.climax_t, 30.0);
        assert_eq!(v.hook, 1);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(parse_verdict("не json", 30.0).is_err());
        assert!(parse_verdict("{\"hook\":5}", 30.0).is_err());
        assert!(parse_verdict(&GOOD.replace("Он не ожидал такого", "  "), 30.0).is_err());
    }

    #[test]
    fn grammar_names_every_field_the_parser_needs() {
        for key in ["hook", "payoff", "standalone", "title", "climax_t", "caption", "hashtags"] {
            assert!(GRAMMAR.contains(&format!("\\\"{key}\\\"")), "в грамматике нет {key}");
        }
    }
}
