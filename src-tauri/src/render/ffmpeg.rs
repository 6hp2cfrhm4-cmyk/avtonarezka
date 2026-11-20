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
    if !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return format!("{stem}.ass");
    }
    format!(
        "job-{}.ass",
        hex::encode(&Sha256::digest(stem.as_bytes())[..6])
    )
}

/// Папка со шрифтами для `ass` — рядом с исходниками: `assets/fonts` или копия в данных приложения.
pub async fn render(
    job: &RenderJob,
    work_dir: &Path,
    fonts_dir: Option<&Path>,
    cancel: watch::Receiver<bool>,
    on_progress: impl FnMut(f64),
) -> anyhow::Result<()> {
    render_limited(job, work_dir, fonts_dir, cancel, None, on_progress).await
}

/// Тот же граф, с ограничениями экспортного варианта; исходный финал не меняется.
pub async fn render_limited(
    job: &RenderJob,
    work_dir: &Path,
    fonts_dir: Option<&Path>,
    cancel: watch::Receiver<bool>,
    limits: Option<&super::types::OutputLimits>,
    mut on_progress: impl FnMut(f64),
) -> anyhow::Result<()> {
    use super::types::{Decode, Encoder};
    let mut job = job.clone();
    let mut gpu = if job.encoder == Encoder::Nvenc || job.decode == Decode::Nvidia {
        crate::operations::gpu::render().await
    } else {
        None
    };
    if gpu.is_none() {
        if job.encoder == Encoder::Nvenc || job.decode == Decode::Nvidia {
            tracing::info!("GPU занят локальным ИИ; рендер продолжит на CPU");
        }
        job.encoder = Encoder::X264;
        job.decode = Decode::Cpu;
    }
    loop {
        if *cancel.borrow() {
            bail!("рендер отменён");
        }
        match render_once(
            &job,
            work_dir,
            fonts_dir,
            cancel.clone(),
            limits,
            &mut on_progress,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) if !*cancel.borrow() && job.decode == Decode::Nvidia => {
                tracing::warn!("NVDEC не справился ({e:#}), повторяю с декодированием на CPU");
                job.decode = Decode::Cpu;
            }
            Err(e) if !*cancel.borrow() && job.encoder == Encoder::Nvenc => {
                tracing::warn!("NVENC не справился ({e:#}), повторяю на CPU");
                job.encoder = Encoder::X264;
                job.decode = Decode::Cpu;
                drop(gpu.take());
            }
            Err(e) => return Err(e),
        }
    }
}

async fn render_once(
    job: &RenderJob,
    work_dir: &Path,
    fonts_dir: Option<&Path>,
    mut cancel: watch::Receiver<bool>,
    limits: Option<&super::types::OutputLimits>,
    mut on_progress: impl FnMut(f64),
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(work_dir).await?;
    if let Some(parent) = job.out.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let stem = job
        .out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "job".into());
    let ass_path = work_dir.join(ass_name(&stem));
    let needs_ass = (job.style.subtitles.enabled && !job.words.is_empty())
        || (job.style.part_label.enabled && job.part.is_some())
        || (job.style.hook_enabled && job.hook.as_ref().is_some_and(|s| !s.trim().is_empty()));
    if needs_ass {
        tokio::fs::write(&ass_path, ass::build(job))
            .await
            .context("не удалось записать субтитры")?;
    }

    // Пишем во временный файл и переименовываем в конце: недорендеренный ролик не выглядит готовым.
    let final_out = job.out.clone();
    let part_path = final_out.with_extension("part.mp4");
    let mut job = job.clone();
    job.out = part_path.clone();
    let mut plan = filter::build(&job, needs_ass.then_some(ass_path.as_path()), fonts_dir);
    if let Some(limits) = limits {
        apply_limits(&mut plan, limits)?;
    }

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
    tokio::fs::rename(&part_path, &final_out)
        .await
        .context("не удалось переименовать готовый файл")?;
    on_progress(1.0);
    Ok(())
}

/// Опции кодирования располагаются перед выходным файлом. Старый CQ/CRF убирается,
/// чтобы заданный битрейт имел один смысл для CPU и NVENC.
pub fn apply_limits(
    plan: &mut filter::Plan,
    limits: &super::types::OutputLimits,
) -> anyhow::Result<()> {
    limits.validate().map_err(anyhow::Error::msg)?;
    let out = plan.args.pop().context("Нет выходного файла")?;
    let output_start = plan
        .args
        .iter()
        .rposition(|a| a == "-i")
        .map_or(0, |i| i + 2);
    let mut args: Vec<String> = plan.args.drain(..output_start).collect();
    let mut iter = plan.args.drain(..);
    while let Some(arg) = iter.next() {
        if ["-b:v", "-b:a", "-maxrate", "-bufsize", "-cq", "-crf", "-t"].contains(&arg.as_str()) {
            iter.next();
        } else {
            args.push(arg);
        }
    }
    drop(iter);
    if let Some(max) = limits.max_seconds {
        // Выход 30fps: дробный предел не должен добавлять кадр за границей лимита.
        if max <= plan.duration {
            plan.duration = (max * 30.0).floor() / 30.0;
        }
    }
    if let Some(at) = args.iter().position(|a| a == "-filter_complex") {
        let graph = args.get_mut(at + 1).context("Нет графа фильтров")?;
        // loudnorm работает на 192kHz. Явно возвращаем 48kHz и нулевой PTS до AAC,
        // иначе у стерео-клипа контейнер может оказаться на 10–20мс длиннее -t.
        if let Some(end) = graph.rfind("[aout]") {
            graph.insert_str(
                end,
                &format!(
                    ",aresample=48000,atrim=duration={:.6},asetpts=PTS-STARTPTS",
                    plan.duration
                ),
            );
            plan.filter = graph.clone();
        }
    }
    args.extend([
        "-b:v".into(),
        format!("{}k", limits.video_kbps),
        "-maxrate".into(),
        format!("{}k", limits.video_kbps),
        "-bufsize".into(),
        format!("{}k", limits.video_kbps * 2),
        "-b:a".into(),
        format!("{}k", limits.audio_kbps),
        "-t".into(),
        format!("{:.3}", plan.duration),
        out,
    ]);
    plan.args = args;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ass_name;
    #[test]
    fn output_rate_does_not_remove_input_duration_or_seek() {
        let mut p = super::filter::Plan {
            args: [
                "-ss",
                "7",
                "-t",
                "10",
                "-i",
                "source.mp4",
                "-crf",
                "19",
                "-b:v",
                "10M",
                "-maxrate",
                "14M",
                "-t",
                "15",
                "out.mp4",
            ]
            .map(str::to_string)
            .to_vec(),
            duration: 15.0,
            filter: String::new(),
        };
        let limits = super::super::types::OutputLimits {
            max_seconds: Some(8.0),
            video_kbps: 2000,
            audio_kbps: 128,
        };
        super::apply_limits(&mut p, &limits).unwrap();
        assert_eq!(&p.args[..6], &["-ss", "7", "-t", "10", "-i", "source.mp4"]);
        assert_eq!(p.args.iter().filter(|a| a.as_str() == "-t").count(), 2);
        assert!(!p.args.iter().any(|a| a == "-crf"));
        assert_eq!(p.duration, 8.0);
        assert_eq!(p.args.last().unwrap(), "out.mp4");
        assert!(p.args.windows(2).any(|a| a == ["-b:v", "2000k"]));
    }

    #[test]
    fn output_timeline_trims_after_loudnorm_and_rounds_down_partial_frames() {
        let graph = "[0:a]loudnorm=I=-14[aout]";
        let mut p = super::filter::Plan {
            args: [
                "-i",
                "a.mp4",
                "-filter_complex",
                graph,
                "-map",
                "[aout]",
                "out.mp4",
            ]
            .map(str::to_string)
            .to_vec(),
            duration: 10.0,
            filter: graph.into(),
        };
        super::apply_limits(
            &mut p,
            &super::super::types::OutputLimits {
                max_seconds: Some(3.01),
                video_kbps: 600,
                audio_kbps: 96,
            },
        )
        .unwrap();
        assert_eq!(p.duration, 3.0);
        assert!(p.filter.contains(
            "loudnorm=I=-14,aresample=48000,atrim=duration=3.000000,asetpts=PTS-STARTPTS[aout]"
        ));
        assert!(p
            .args
            .windows(2)
            .any(|w| w == ["-filter_complex", p.filter.as_str()]));
    }

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
