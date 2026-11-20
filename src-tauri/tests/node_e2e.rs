//! Передача задач между «ПК» и «сервером» на двух настоящих процессах: сервер — это `avtoctl serve`
//! со своей базой и своим каталогом, ПК — этот тест. Обрыв канала (убитый процесс) ничего не теряет.
#![cfg(not(feature = "desktop"))]

use std::{path::PathBuf, process::Command, sync::Mutex};

use avtonarezka_lib::node::{
    link::{pull_results, recall, send_item, ProcWire, Wire},
    open_database,
};
use rusqlite::Connection;
use serde_json::json;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("avto-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn server_process(dir: &PathBuf) -> ProcWire {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_avtoctl"));
    cmd.arg("serve").env("AVTO_DATA_DIR", dir);
    ProcWire::spawn(cmd).expect("avtoctl запустился")
}

fn add_item(db: &Mutex<Connection>, source: &str, kind: &str) -> i64 {
    let g = db.lock().unwrap();
    g.execute(
        "INSERT INTO factory_items(position,kind,source,title,preset,precut,stage) VALUES(1,?1,?2,'Тест','1080',0,'queued')",
        [kind, source],
    )
    .unwrap();
    g.last_insert_rowid()
}

#[test]
fn two_processes_hand_over_a_file_task_and_get_the_result_back() {
    let (pc_dir, srv_dir) = (tmp("pc"), tmp("srv"));
    let pc = open_database(&pc_dir.join("avtonarezka.db")).unwrap();
    let srv = open_database(&srv_dir.join("avtonarezka.db")).unwrap();
    // исходный файл крупнее одного куска, чтобы проверить разбиение и докачку
    let source = pc_dir.join("stream.mp4");
    std::fs::write(&source, (0..1_300_000u32).map(|i| (i % 253) as u8).collect::<Vec<_>>()).unwrap();
    let item = add_item(&pc, source.to_str().unwrap(), "file");

    {
        let mut w = server_process(&srv_dir);
        let st = w.call("status", json!({})).unwrap();
        assert_eq!(st["db"], true);
        // первый кусок уходит и процесс «обрывается»
        let data = std::fs::read(&source).unwrap();
        let sha = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(&data))
        };
        let chunk = &data[..avtonarezka_lib::node::server::CHUNK];
        use base64::Engine;
        w.call("put", json!({"sha256": sha, "total": data.len(), "offset": 0,
            "data": base64::engine::general_purpose::STANDARD.encode(chunk)})).unwrap();
    } // процесс убит

    // новый процесс продолжает с места обрыва и принимает задачу
    let mut w = server_process(&srv_dir);
    send_item(&pc, &mut w, item, &mut |_, _| {}).unwrap();
    drop(w);
    assert_eq!(
        pc.lock().unwrap().query_row("SELECT owner_node FROM factory_items WHERE id=?1", [item], |r| r.get::<_, String>(0)).unwrap(),
        "server"
    );
    let (remote_src, owner): (String, String) = srv
        .lock().unwrap()
        .query_row("SELECT source,owner_node FROM factory_items", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(owner, "local");
    assert_eq!(std::fs::read(&remote_src).unwrap(), std::fs::read(&source).unwrap(), "файл на сервере совпадает побайтно");

    // повторная отправка той же задачи не создаёт дубль на сервере
    assert!(send_item(&pc, &mut server_process(&srv_dir), item, &mut |_, _| {}).is_err());
    let n: i64 = srv.lock().unwrap().query_row("SELECT COUNT(*) FROM factory_items", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);

    // «сервер» работает и готовит ролик
    let clip = srv_dir.join("out.mp4");
    std::fs::write(&clip, vec![9u8; 700_000]).unwrap();
    {
        let g = srv.lock().unwrap();
        g.execute("INSERT INTO projects(id,path,hash,title,duration,width,height,fps,has_audio) VALUES(1,'p','h','t',10,1920,1080,30,1)", []).unwrap();
        g.execute("INSERT INTO moments(id,project_id,start,\"end\") VALUES(1,1,0,30)", []).unwrap();
        g.execute("INSERT INTO drafts(id,moment_id,status,final_path) VALUES(1,1,'final_done',?1)", [clip.to_string_lossy()]).unwrap();
        g.execute("UPDATE factory_items SET project_id=1,stage='completed'", []).unwrap();
    }

    // ПК забирает результат новым подключением (в конце выключенный ПК включается и забирает)
    let mut w = server_process(&srv_dir);
    let got = pull_results(&pc, &mut w, &pc_dir).unwrap();
    assert_eq!(got.finished, vec![item]);
    let pack: String = pc.lock().unwrap().query_row("SELECT pack_id FROM transfers", [], |r| r.get(0)).unwrap();
    assert_eq!(std::fs::read(pc_dir.join("from-server").join(pack).join("out.mp4")).unwrap(), vec![9u8; 700_000]);
    assert!(recall(&pc, &mut w, item).is_err(), "завершённую задачу вернуть нельзя");
}

#[test]
fn integration_settings_reach_the_server_store_and_unknown_names_are_refused() {
    let srv_dir = tmp("cfg");
    let _db = open_database(&srv_dir.join("avtonarezka.db")).unwrap();
    let key = srv_dir.join("secrets.key");
    std::fs::write(&key, "ab".repeat(32)).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_avtoctl"));
    cmd.arg("serve").env("AVTO_DATA_DIR", &srv_dir).env("AVTO_SECRETS_KEY_FILE", &key);
    let mut w = ProcWire::spawn(cmd).unwrap();

    let r = w.call("config_put", json!({"secrets": {"factory_twitch_app": "{\"client_id\":\"a\",\"client_secret\":\"b\"}", "notify_bot_token": "123:abc"}, "kv": {"notify_config": "{\"enabled\":true,\"chat\":\"42\"}"}})).unwrap();
    assert_eq!(r["applied"].as_array().unwrap().len(), 3);
    let g = w.call("config_get", json!({})).unwrap();
    assert_eq!(g["keyStore"], true);
    assert_eq!(g["secrets"]["factory_twitch_app"], true);
    assert_eq!(g["secrets"]["notify_bot_token"], true);
    assert_eq!(g["secrets"]["discovery_youtube_key"], false);
    assert!(g["kv"]["notify_config"].as_str().unwrap().contains("\"chat\":\"42\""));
    // значения секретов наружу не возвращаются
    assert!(!g.to_string().contains("123:abc"));
    // секрет зашифрован на диске
    let raw = std::fs::read(srv_dir.join("secrets.enc")).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("123:abc"));

    // чужие имена и кривой JSON отклоняются
    assert!(w.call("config_put", json!({"secrets": {"ssh_private_key": "x"}})).is_err());
    assert!(w.call("config_put", json!({"kv": {"scene_active": "1"}})).is_err());
    assert!(w.call("config_put", json!({"kv": {"notify_config": "не json"}})).is_err());
    // пустое значение удаляет секрет
    w.call("config_put", json!({"secrets": {"notify_bot_token": ""}})).unwrap();
    assert_eq!(w.call("config_get", json!({})).unwrap()["secrets"]["notify_bot_token"], false);
}
