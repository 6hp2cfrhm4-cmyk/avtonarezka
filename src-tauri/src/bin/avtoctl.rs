//! Серверная сторона канала «ПК ↔ сервер». ssh запускает её через ограниченный ключ
//! (`command="…/avtoctl serve"`): на stdin приходят строки JSON, на stdout уходят ответы.
use std::io::{BufRead, Read, Write};

use avtonarezka_lib::node::{handle_line, NodeCtx, MAX_LINE};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => {}
        _ => anyhow::bail!("использование: avtoctl serve"),
    }
    let ctx = NodeCtx {
        data_dir: avtonarezka_lib::daemon_data_dir(),
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut reader = stdin.lock().take(MAX_LINE as u64 + 2);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let resp = handle_line(&ctx, line.trim_end());
        serde_json::to_writer(&mut out, &resp)?;
        out.write_all(b"\n")?;
        out.flush()?;
        reader.set_limit(MAX_LINE as u64 + 2);
    }
    Ok(())
}
