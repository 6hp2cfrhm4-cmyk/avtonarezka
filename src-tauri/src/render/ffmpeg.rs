//! Запуск ffmpeg: прогресс, отмена, понятная ошибка.

use std::{collections::VecDeque, path::Path, process::Stdio};

use anyhow::{bail, Context};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::watch,
};

use super::{ass, filter, types::RenderJob};

/// Имя файла субтитров только из ASCII: путь с кириллицей libass на Windows читает ненадёжно.
fn ass_name(stem: &str) -> String {
    use sha2::{Digest, Sha256};
    if !stem.is_empty() && stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        return format!("{stem}.ass");
    }
    format!("job-{}.ass", hex::encode(&Sha256::digest(stem.as_bytes())[..6]))
}

/// Папка со шрифтами для `ass` — рядом с исходниками: `assets/fonts` или копия в данных приложения.
pub async fn render(
    job: &RenderJob,
    work_dir: &Path,
    fonts_dir: Option<&Path>,
    mut cancel: watch::Receiver<bool>,
    mut on_progress: impl FnMut(f64),
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(work_dir).await?;
    if let Some(parent) = job.out.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let stem = job.out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "job".into());
    let ass_path = work_dir.join(ass_name(&stem));
    let needs_ass = (job.style.subtitles.enabled && !job.words.is_empty())
        || (job.style.part_label.enabled && job.part.is_some());
    if needs_ass {
        tokio::fs::write(&ass_path, ass::build(job)).await.context("не удалось записать субтитры")?;
    }

    // Пишем во временный файл и переименовываем в конце: недорендеренный ролик не выглядит готовым.
    let final_out = job.out.clone();
    let part_path = final_out.with_extension("part.mp4");
    let mut job = job.clone();
    job.out = part_path.clone();
    let plan = filter::build(&job, needs_ass.then_some(ass_path.as_path()), fonts_dir);

    let mut child = crate::platform::command("ffmpeg")
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("не удалось запустить ffmpeg")?;

    // stderr нужно вычитывать, иначе процесс встанет на заполненном буфере.
    let stderr = child.stderr.take().context("нет stderr")?;
    let tail = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut buf: VecDeque<String> = VecDeque::with_capacity(40);
        while let Ok(Some(l)) = lines.next_line().await {
            if buf.len() == 40 {
                buf.pop_front();
            }
            buf.push_back(l);
        }
        buf.into_iter().collect::<Vec<_>>().join("\n")
    });

    let mut lines = BufReader::new(child.stdout.take().context("нет stdout")?).lines();
    let mut cancelled = false;
    let mut cancel_open = true;
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(l) => {
                    // out_time_us, а не out_time_ms: имя историческое, значение в микросекундах в обоих
                    if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<i64>().ok()) {
                        on_progress((us as f64 / 1e6 / plan.duration).clamp(0.0, 1.0));
                    }
                }
                None => break,
            },
            res = cancel.changed(), if cancel_open => {
                if res.is_err() {
                    // отправитель закрыт — отменять уже некому
                    cancel_open = false;
                } else if *cancel.borrow() {
                    cancelled = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }
    }

    let status = child.wait().await?;
    let stderr_tail = tail.await.unwrap_or_default();
    if needs_ass {
        let _ = tokio::fs::remove_file(&ass_path).await;
    }
    if cancelled {
        let _ = tokio::fs::remove_file(&part_path).await;
        bail!("рендер отменён");
    }
    if !status.success() {
        let _ = tokio::fs::remove_file(&part_path).await;
        bail!("ffmpeg завершился с ошибкой ({status}):\n{stderr_tail}");
    }
    tokio::fs::rename(&part_path, &final_out).await.context("не удалось переименовать готовый файл")?;
    on_progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ass_name;

    #[test]
    fn ascii_names_are_kept() {
        assert_eq!(ass_name("clip-3_final"), "clip-3_final.ass");
    }

    #[test]
    fn other_names_become_ascii_and_stay_distinct() {
        let (a, b) = (ass_name("Смешной момент"), ass_name("Смешной момент 2"));
        assert!(a.is_ascii() && b.is_ascii());
        assert!(a.starts_with("job-") && a.ends_with(".ass"));
        assert_ne!(a, b);
        assert_eq!(a, ass_name("Смешной момент"));
    }
}
