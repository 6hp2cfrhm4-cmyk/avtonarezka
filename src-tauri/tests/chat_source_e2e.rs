//! Replay-чаты получают только поддельные локальные утилиты; никаких установок/сети/cookies.
#[cfg(unix)]
#[tokio::test]
async fn replay_tools_args_and_chat_parsing_match_source_contract() {
    use avtonarezka_lib::{
        analyze::chat as parser,
        sources::{chat, MonitorOptions},
    };
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("avto-chat-source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("tool.py");
    std::fs::write(&fake,r##"#!/usr/bin/python3
import pathlib,sys,json
root=pathlib.Path(__file__).parent;a=sys.argv[1:]
with (root/'args.jsonl').open('a') as f:f.write(json.dumps(a)+'\n')
if a==['--version']:
 print('tool error' if (root/'version-error').exists() else 'TwitchDownloaderCLI 1.56.5+fixture');sys.exit(1)
if 'chatdownload' in a:
 p=pathlib.Path(a[a.index('--output')+1]);p.write_text(json.dumps({'comments':[{'content_offset_seconds':12,'commenter':{'_id':'u'},'message':{'body':'KEKW'}}]}))
else:
 p=pathlib.Path(a[a.index('--output')+1].replace('%(ext)s','live_chat.json'));p.write_text(json.dumps({'replayChatItemAction':{'videoOffsetTimeMsec':'12000','actions':[{'addChatItemAction':{'item':{'liveChatTextMessageRenderer':{'authorExternalChannelId':'u','message':{'runs':[{'text':'Pog'}]}}}}}]}}))
"##).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("AVTO_YTDLP", &fake);
    std::env::set_var("AVTO_TWITCH_DOWNLOADER", &fake);
    let options = MonitorOptions::default();
    let y = chat::download(
        "youtube",
        "https://youtube.com/watch?v=abcdefghijk",
        &options,
        &dir.join("yt"),
    )
    .await
    .unwrap();
    assert_eq!(parser::load(&y, 30.0).unwrap().messages, 1);
    let t = chat::download(
        "twitch",
        "https://www.twitch.tv/videos/123",
        &options,
        &dir.join("tw"),
    )
    .await
    .unwrap();
    assert_eq!(parser::load(&t, 30.0).unwrap().format, "twitch");
    assert_eq!(
        chat::version().await.as_deref(),
        Some("TwitchDownloaderCLI 1.56.5+fixture")
    );
    std::fs::write(dir.join("version-error"), "").unwrap();
    assert!(chat::version().await.is_none());
    assert!(
        chat::download("kick", "https://kick.com/a", &options, &dir.join("kick"))
            .await
            .unwrap_err()
            .contains("импорт")
    );
    let lines = std::fs::read_to_string(dir.join("args.jsonl")).unwrap();
    let args: Vec<Vec<String>> = lines
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(args[0].contains(&"--ignore-config".into()));
    assert!(args[0].contains(&"live_chat".into()));
    assert!(args
        .iter()
        .all(|a| !a.contains(&"--cookies-from-browser".into())));
    assert_eq!(&args[1][..3], &["chatdownload", "--id", "123"]);
    assert!(args[1]
        .windows(2)
        .any(|p| p == ["--collision", "Overwrite"]));
    assert!(args[1].windows(2).any(|p| p == ["--threads", "2"]));
    std::env::remove_var("AVTO_YTDLP");
    std::env::remove_var("AVTO_TWITCH_DOWNLOADER");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
#[ignore = "Нужны согласованный TwitchDownloaderCLI AVTO_TWITCH_DOWNLOADER и публичный AVTO_LIVE_TWITCH_VOD; скачивает только чат, без моделей/установки"]
async fn real_public_twitch_chat_download_and_parser_preserve_absolute_offsets() {
    use avtonarezka_lib::{
        analyze::chat as parser,
        sources::{chat, MonitorOptions},
    };
    let url = std::env::var("AVTO_LIVE_TWITCH_VOD").expect("AVTO_LIVE_TWITCH_VOD");
    let dir = std::path::PathBuf::from(
        std::env::var_os("AVTO_LIVE_CHAT_DIR").expect("AVTO_LIVE_CHAT_DIR"),
    );
    assert!(chat::version().await.unwrap().contains("1.56.5"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("chat.json"), "stale incomplete file").unwrap();
    let path = chat::download("twitch", &url, &MonitorOptions::default(), &dir)
        .await
        .unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let duration = std::env::var("AVTO_LIVE_TWITCH_DURATION")
        .unwrap()
        .parse::<f64>()
        .unwrap();
    let features = parser::load(&path, duration).unwrap();
    assert!(features.messages > 0);
    assert_eq!(features.format, "twitch");
    let offsets: Vec<f64> = raw["comments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["content_offset_seconds"].as_f64())
        .collect();
    assert!(!offsets.is_empty());
    assert!(offsets.iter().all(|s| *s >= 0.0 && *s <= duration));
    let short = std::path::PathBuf::from(std::env::var_os("AVTO_LIVE_CHAT_SHORT").unwrap());
    let sample = parser::load(&short, duration).unwrap();
    assert_eq!(sample.messages, 21);
    assert!(sample.curve[..1198].iter().all(|v| *v <= 0.0));
    assert!(sample.curve[1200..1260].iter().any(|v| *v > 0.0));
    eprintln!("PASS public Twitch chat: {} messages, {} numerical hops, absolute sample600–629s, stale output replaced; raw chat stays in isolated fixture",features.messages,features.curve.len());
}
