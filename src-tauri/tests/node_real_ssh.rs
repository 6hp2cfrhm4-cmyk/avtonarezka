//! Настоящий сервер по ssh (только руками): `AVTO_REAL_SSH=host:port:user:/путь/к/ключу cargo test --test node_real_ssh -- --ignored --nocapture`.
//! Проверяет транспорт: ограниченный ключ, forced command, передача небольшого файла, проверка у сервера,
//! возврат задачи. Ключ проекта и known_hosts пользователя читаются только для этого сервера.
#![cfg(not(feature = "desktop"))]

use std::sync::Mutex;

use avtonarezka_lib::node::{
    link::{recall, send_item, ssh_command, ProcWire, ServerConfig, Wire},
    open_database,
};
use serde_json::json;

/// `AVTO_REAL_SSH=хост:порт:пользователь:путь_к_ключу`
fn real_cfg() -> Option<(ServerConfig, String)> {
    let v = std::env::var("AVTO_REAL_SSH").ok()?;
    let p: Vec<&str> = v.splitn(4, ':').collect();
    if p.len() != 4 {
        return None;
    }
    let cfg = ServerConfig { host: p[0].into(), port: p[1].parse().ok()?, user: p[2].into(), key_path: p[3].into(), enabled: true };
    let entry = if cfg.port == 22 { cfg.host.clone() } else { format!("[{}]:{}", cfg.host, cfg.port) };
    Some((cfg, entry))
}

#[test]
#[ignore]
fn real_server_roundtrip() {
    let Some((cfg, entry)) = real_cfg() else {
        eprintln!("пропуск: нужен AVTO_REAL_SSH=хост:порт:пользователь:путь_к_ключу");
        return;
    };
    let dir = std::env::temp_dir().join(format!("avto-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // доверенный отпечаток — тот, что уже есть в known_hosts пользователя для этого адреса
    let out = std::process::Command::new("ssh-keygen").args(["-F", &entry]).output().unwrap();
    let known: String = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n");
    assert!(known.contains("ssh-ed25519"), "в known_hosts нет сервера");
    let kh = dir.join("known_hosts");
    std::fs::write(&kh, format!("{known}\n")).unwrap();

    let mut w = ProcWire::spawn(ssh_command(&cfg, &kh)).expect("ssh с ограниченным ключом");
    let ping = w.call("ping", json!({})).unwrap();
    println!("ping: {ping}");
    assert_eq!(ping["node"], "server");
    let st = w.call("status", json!({})).unwrap();
    println!("status: {st}");
    assert_eq!(st["db"], true);
    // произвольные глаголы сервер не принимает
    assert!(w.call("rm", json!({})).is_err());

    // небольшой файл-задача: сервер примет, проверим по items, затем вернём на ПК
    let pc = open_database(&dir.join("pc.db")).unwrap();
    let src = dir.join("probe.mp4");
    std::fs::write(&src, (0..300_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
    let item = {
        let g: &Mutex<_> = &pc;
        let g = g.lock().unwrap();
        g.execute("INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,'file',?1,'Проверка транспорта','1080',0,'queued')", [src.to_string_lossy()]).unwrap();
        g.last_insert_rowid()
    };
    send_item(&pc, &mut w, item, &mut |_, _| {}).expect("передача на настоящий сервер");
    let owner: String = pc.lock().unwrap().query_row("SELECT owner_node FROM factory_items WHERE id=?1", [item], |r| r.get(0)).unwrap();
    assert_eq!(owner, "server");
    let remote: i64 = pc.lock().unwrap().query_row("SELECT remote_id FROM transfers", [], |r| r.get(0)).unwrap();
    let items = w.call("items", json!({"ids": [remote]})).unwrap();
    println!("items: {items}");
    recall(&pc, &mut w, item).expect("возврат задачи");
    let owner: String = pc.lock().unwrap().query_row("SELECT owner_node FROM factory_items WHERE id=?1", [item], |r| r.get(0)).unwrap();
    assert_eq!(owner, "local");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Настоящая работа на сервере: короткое видео с «всплесками» уходит на сервер, там идёт весь конвейер
/// (звук, картинка, речь Whisper small, отбор, черновики, рендер x264), результат забираем обратно.
/// `AVTO_REAL_SSH=1 AVTO_REAL_PIPELINE=1 cargo test --test node_real_ssh real_pipeline_on_server -- --ignored --nocapture`
#[test]
#[ignore]
fn real_pipeline_on_server() {
    let (Some((cfg, entry)), Some(_)) = (real_cfg(), std::env::var_os("AVTO_REAL_PIPELINE")) else {
        eprintln!("пропуск: нужны AVTO_REAL_SSH=… и AVTO_REAL_PIPELINE=1");
        return;
    };
    let dir = std::env::temp_dir().join(format!("avto-pipe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = std::process::Command::new("ssh-keygen").args(["-F", &entry]).output().unwrap();
    let known: String = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n");
    let kh = dir.join("known_hosts");
    std::fs::write(&kh, format!("{known}\n")).unwrap();

    // 3 минуты, 720p; тихо, но с тремя громкими отрезками и сменой картинки — есть что искать
    let src = dir.join("real.mp4");
    let af = "volume='if(between(mod(t,60),20,28),1.0,0.03)':eval=frame";
    let st = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1280x720:r=30:d=180", "-f", "lavfi", "-i", "sine=f=330:r=44100:d=180", "-af", af])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "28", "-c:a", "aac", "-shortest"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(st.success());

    let pc = open_database(&dir.join("pc.db")).unwrap();
    let item = {
        let g = pc.lock().unwrap();
        g.execute("INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,'file',?1,'Проверка конвейера','1080',0,'queued')", [src.to_string_lossy()]).unwrap();
        g.last_insert_rowid()
    };
    let mut w = ProcWire::spawn(ssh_command(&cfg, &kh)).unwrap();
    send_item(&pc, &mut w, item, &mut |_, _| {}).expect("передача");
    let remote: i64 = pc.lock().unwrap().query_row("SELECT remote_id FROM transfers", [], |r| r.get(0)).unwrap();
    let started = std::time::Instant::now();
    let mut last = String::new();
    loop {
        let items = w.call("items", json!({"ids": [remote]})).unwrap();
        let it = &items["items"][0];
        let stage = it["stage"].as_str().unwrap_or("?").to_string();
        if stage != last {
            println!("[{:>4}с] стадия на сервере: {stage} {}", started.elapsed().as_secs(), it["error"].as_str().unwrap_or(""));
            last = stage.clone();
        }
        if ["completed", "failed", "needs_check", "awaiting_selection"].contains(&stage.as_str()) || started.elapsed().as_secs() > 1500 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
    let got = avtonarezka_lib::node::link::pull_results(&pc, &mut w, &dir).unwrap();
    println!("итог: {got:?}");
    let files: Vec<_> = std::fs::read_dir(dir.join("from-server")).ok().into_iter().flatten().flatten().collect();
    println!("папок результатов: {}", files.len());
    assert!(["completed", "awaiting_selection", "failed", "needs_check"].contains(&last.as_str()), "конвейер не дошёл до конца: {last}");
}
