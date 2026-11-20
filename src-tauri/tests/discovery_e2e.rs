//! Провайдеры автопоиска против поддельных серверов на записанных ответах: токен, пагинации нет, ошибки видны.
use avtonarezka_lib::discovery::providers::Net;
use std::sync::{Arc, Mutex};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

/// Минимальный HTTP-сервер: по пути отдаёт (статус, тело); запоминает все запросы (первая строка + заголовки).
async fn serve(routes: Vec<(&'static str, u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            let routes = routes.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                seen.lock().unwrap().push(req);
                let route = routes.iter().find(|r| path.starts_with(r.0));
                let (code, body) = route.map_or((404, "{}"), |r| (r.1, r.2));
                let resp = format!("HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    (addr, log)
}

#[tokio::test]
async fn twitch_token_streams_and_clips() {
    let (base, log) = serve(vec![
        ("/oauth2/token", 200, r#"{"access_token":"tok","expires_in":3600}"#),
        ("/helix/streams", 200, r#"{"data":[{"user_login":"zx","user_name":"ZX","game_name":"Dota 2","type":"live","title":"Финал","viewer_count":4200,"language":"ru"}]}"#),
        ("/helix/games/top", 200, r#"{"data":[{"id":"29595","name":"Dota 2"}]}"#),
        ("/helix/clips", 200, r#"{"data":[{"id":"C1","url":"https://clips.twitch.tv/C1","broadcaster_name":"B","title":"Момент","view_count":500,"language":"ru"},{"id":"C2","url":"https://clips.twitch.tv/C2","broadcaster_name":"B","title":"en","view_count":900,"language":"en"}]}"#),
    ])
    .await;
    let net = Net { twitch_base: format!("{base}/helix"), twitch_token: format!("{base}/oauth2/token"), ..Net::default() };
    let found = net.twitch("cid", "secret", "ru", 1).await.unwrap();
    let keys: Vec<_> = found.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["twitch:live:zx", "twitch:clip:C1"], "английский клип отфильтрован");
    let log = log.lock().unwrap().join("\n");
    assert!(log.contains("client_id=cid") && log.contains("grant_type=client_credentials"), "токен запрошен: {log}");
    let l = log.to_lowercase();
    assert!(l.contains("authorization: bearer tok") && l.contains("client-id: cid"), "{log}");
    assert!(log.contains("language=ru") && log.contains("game_id=29595"));
}

#[tokio::test]
async fn failures_are_reported_not_hidden() {
    let (base, _) = serve(vec![("/oauth2/token", 400, r#"{"message":"invalid client"}"#), ("/yt/videos", 403, r#"{"error":{"message":"quotaExceeded"}}"#), ("/kick/livestreams", 401, "{}")]).await;
    let net = Net { twitch_base: format!("{base}/helix"), twitch_token: format!("{base}/oauth2/token"), youtube_base: format!("{base}/yt"), kick_base: format!("{base}/kick"), ..Net::default() };
    assert!(net.twitch("a", "b", "ru", 1).await.unwrap_err().to_string().contains("HTTP 400"));
    assert!(net.youtube("k", "RU").await.unwrap_err().to_string().contains("403"));
    assert!(net.kick("t").await.unwrap_err().to_string().contains("401"));
}

#[tokio::test]
async fn kick_and_youtube_and_topics() {
    let (base, log) = serve(vec![
        ("/kick/livestreams", 200, r#"{"data":[{"slug":"abc","stream_title":"Стрим","viewer_count":777,"language":"ru","category":{"name":"Just Chatting"}}]}"#),
        ("/yt/videos", 200, r#"{"items":[{"id":"v1","snippet":{"title":"T","channelTitle":"C","categoryId":"20","defaultAudioLanguage":"ru"},"statistics":{"viewCount":"10"},"contentDetails":{"duration":"PT5M"}}]}"#),
        ("/trends", 200, "<rss><channel><item><title>Тема дня</title></item></channel></rss>"),
        ("/wiki/ru.wikipedia", 200, r#"{"items":[{"articles":[{"article":"Статья_дня","views":5}]}]}"#),
    ])
    .await;
    let net = Net { kick_base: format!("{base}/kick"), youtube_base: format!("{base}/yt"), trends_url: format!("{base}/trends"), wiki_base: format!("{base}/wiki"), ..Net::default() };
    assert_eq!(net.kick("tok").await.unwrap()[0].obs.category, "Just Chatting");
    assert_eq!(net.youtube("KEY", "RU").await.unwrap()[0].key, "youtube:vod:v1");
    assert_eq!(net.topics("RU", "ru").await.unwrap(), vec!["Тема дня".to_string(), "Статья дня".to_string()]);
    let log = log.lock().unwrap().join("\n");
    assert!(log.contains("key=KEY") && log.contains("chart=mostPopular") && log.contains("geo=RU"), "{log}");
}

/// Живые запросы: нужны ключи в окружении; запускается руками (`-- --ignored`).
#[tokio::test]
#[ignore]
async fn live_topics() {
    let t = Net::default().topics("RU", "ru").await.unwrap();
    println!("{} тем: {:?}", t.len(), &t[..t.len().min(5)]);
    assert!(!t.is_empty());
}
