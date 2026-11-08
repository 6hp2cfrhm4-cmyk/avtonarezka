//! Воркер LLM. Протокол: по строке JSON на запрос в stdin, по строке JSON на ответ в stdout.
//!
//!   → {"id":1,"prompt":"<|im_start|>…","grammar":"root ::= …","maxTokens":300,"temperature":0.2}
//!   ← {"id":1,"text":"{…}","promptTokens":812,"generated":97}
//!   ← {"id":2,"error":"…"}            (воркер при этом продолжает жить)
//!
//! При запуске печатает {"ready":true}. Завершается, когда закрыт stdin.
//! Аргументы: --model <gguf> [--threads N] [--gpu-layers N] [--ctx N]

use std::{
    io::{BufRead, Write},
    num::NonZeroU32,
};

// Один и тот же код у приложения и воркера: выбор vulkan-1.dll на Windows (системный или запасной из установки)
#[path = "../../src/vulkan_loader.rs"]
mod vulkan_loader;

use anyhow::{bail, Context, Result};
use llama_cpp_2::{
    context::params::LlamaContextParams,
    list_llama_ggml_backend_devices,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, LlamaModel},
    LlamaBackendDeviceType,
    sampling::LlamaSampler,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    id: u64,
    prompt: String,
    grammar: Option<String>,
    max_tokens: u32,
    #[serde(default)]
    temperature: f32,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct Response {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct Settings {
    model: String,
    threads: i32,
    gpu_layers: u32,
    ctx: u32,
}

fn parse_args() -> Result<Settings> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    Ok(Settings {
        model: get("--model").context("нужен --model <файл.gguf>")?,
        threads: get("--threads").and_then(|v| v.parse().ok()).unwrap_or(8),
        gpu_layers: get("--gpu-layers").and_then(|v| v.parse().ok()).unwrap_or(0),
        ctx: get("--ctx").and_then(|v| v.parse().ok()).unwrap_or(4096),
    })
}

/// Видеокарта для модели: номер из `AVTO_GPU_DEVICE`, иначе дискретная с наибольшей памятью,
/// иначе встроенная. Без выбора llama.cpp раскидал бы слои по обеим картам, и работало бы медленнее.
fn pick_gpu() -> Option<(usize, String)> {
    let devices = list_llama_ggml_backend_devices();
    if let Some(i) = std::env::var("AVTO_GPU_DEVICE").ok().and_then(|v| v.parse::<usize>().ok()) {
        return devices.iter().find(|d| d.index == i).map(|d| (d.index, d.description.clone()));
    }
    devices
        .iter()
        .filter(|d| d.device_type == LlamaBackendDeviceType::Gpu)
        .max_by_key(|d| d.memory_total)
        .or_else(|| devices.iter().find(|d| d.device_type == LlamaBackendDeviceType::IntegratedGpu))
        .map(|d| (d.index, d.description.clone()))
}

fn generate(backend: &LlamaBackend, model: &LlamaModel, s: &Settings, req: &Request) -> Result<(String, usize, usize)> {
    const N_BATCH: usize = 512;
    let vocab = model.vocab();
    let tokens = vocab.tokenize(req.prompt.as_bytes(), true, true);
    if tokens.is_empty() {
        bail!("пустой запрос");
    }
    if tokens.len() + req.max_tokens as usize > s.ctx as usize {
        bail!("запрос не помещается в контекст: {} + {} > {}", tokens.len(), req.max_tokens, s.ctx);
    }

    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(s.ctx))
        .with_n_batch(N_BATCH as u32)
        .with_n_threads(s.threads)
        .with_n_threads_batch(s.threads);
    let mut ctx = model.new_context(backend, params).context("не удалось создать контекст")?;

    // Запрос читается пачками; логиты нужны только для последнего токена.
    let mut batch = LlamaBatch::new(N_BATCH, 1);
    let last = tokens.len() - 1;
    for start in (0..tokens.len()).step_by(N_BATCH) {
        batch.clear();
        for (i, t) in tokens[start..(start + N_BATCH).min(tokens.len())].iter().enumerate() {
            let pos = start + i;
            batch.add(*t, pos as i32, &[0], pos == last)?;
        }
        ctx.decode(&mut batch).context("ошибка чтения запроса")?;
    }

    let mut chain = Vec::new();
    if let Some(g) = &req.grammar {
        chain.push(LlamaSampler::grammar(model, g, "root").context("неверная грамматика")?);
    }
    if req.temperature > 0.0 {
        chain.extend([LlamaSampler::top_k(20), LlamaSampler::temp(req.temperature), LlamaSampler::dist(1234)]);
    } else {
        chain.push(LlamaSampler::greedy());
    }
    let mut sampler = LlamaSampler::chain_simple(chain);

    let mut out = Vec::<u8>::new();
    let mut pos = tokens.len();
    let mut idx = batch.n_tokens() - 1;
    let mut generated = 0;
    while generated < req.max_tokens as usize {
        // `sample` сам принимает токен, повторный `accept` испортил бы состояние грамматики
        let token = sampler.sample(&ctx, idx);
        if vocab.is_eog(token) {
            break;
        }
        out.extend(vocab.token_to_piece(token, false, None));
        generated += 1;
        batch.clear();
        batch.add(token, pos as i32, &[0], true)?;
        pos += 1;
        idx = 0;
        ctx.decode(&mut batch).context("ошибка генерации")?;
    }
    Ok((String::from_utf8_lossy(&out).into_owned(), tokens.len(), generated))
}

fn main() -> Result<()> {
    let s = parse_args()?;
    // До первого обращения к llama.cpp: он при запуске подключает Vulkan
    match vulkan_loader::prepare() {
        vulkan_loader::Source::System => {}
        vulkan_loader::Source::Bundled => eprintln!("llm-worker: системного vulkan-1.dll нет, взят запасной загрузчик из установки"),
        vulkan_loader::Source::Missing => eprintln!("llm-worker: нет ни системного, ни запасного vulkan-1.dll (папка vk рядом с программой)"),
    }
    let mut backend = LlamaBackend::init().context("не удалось запустить llama.cpp")?;
    backend.void_logs();
    let mut params = LlamaModelParams::default().with_n_gpu_layers(s.gpu_layers);
    if s.gpu_layers > 0 {
        match pick_gpu() {
            Some((index, name)) => {
                eprintln!("llm-worker: видеокарта {name} (устройство {index}), слоёв на ней: {}", s.gpu_layers);
                params = params.with_devices(&[index]).context("не удалось выбрать видеокарту")?;
            }
            None => eprintln!("llm-worker: видеокарт нет, модель работает на процессоре"),
        }
    }
    let model = LlamaModel::load_from_file(&backend, &s.model, &params)
        .with_context(|| format!("не удалось загрузить модель {}", s.model))?;

    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{{\"ready\":true}}")?;
    stdout.flush()?;

    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => match generate(&backend, &model, &s, &req) {
                Ok((text, prompt_tokens, generated)) => Response {
                    id: req.id,
                    text: Some(text),
                    prompt_tokens: Some(prompt_tokens),
                    generated: Some(generated),
                    ..Default::default()
                },
                Err(e) => Response { id: req.id, error: Some(format!("{e:#}")), ..Default::default() },
            },
            Err(e) => Response { error: Some(format!("неверный запрос: {e}")), ..Default::default() },
        };
        writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
        stdout.flush()?;
    }
    Ok(())
}
