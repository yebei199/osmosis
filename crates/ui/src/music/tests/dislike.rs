//! #181 调查:真实屏蔽回调在 HTTP 成功后只重载浏览视图,不移除队列。
//! HTTP 出口用确定性响应替身;不证明服务端存储或真实音频。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use similar_asserts::assert_eq;
use slint::{ComponentHandle, Model};

use super::super::fixtures::*;
use super::super::*;
use super::{batch, shown_ids};
use crate::{Library, Viz};

/// 私有网络 namespace 里的 HTTP 边界,记录真实请求并返回规则与过滤后的日推。
fn serve_catalog() -> Arc<Mutex<Vec<String>>> {
    // shared-name: ok — 调用者在自己创建的 network namespace 内,不会占宿主机 3000。
    let address = api::base_url()
        .strip_prefix("http://")
        .expect("调查需要 HTTP 后端");
    let listener = std::net::TcpListener::bind(address)
        .expect("私有网络里的 API 端口");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    std::thread::spawn(move || {
        let mut created = false;
        for incoming in listener.incoming() {
            let mut stream =
                incoming.expect("接 HTTP 请求");
            stream
                .set_read_timeout(Some(
                    Duration::from_secs(5),
                ))
                .expect("HTTP 读取期限");
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .expect("读取 HTTP");
                assert!(read > 0, "HTTP 请求没有完整送到");
                bytes.extend_from_slice(&buffer[..read]);
                let request =
                    String::from_utf8_lossy(&bytes);
                if let Some((headers, body)) =
                    request.split_once("\r\n\r\n")
                {
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) =
                                line.split_once(':')?;
                            key.eq_ignore_ascii_case(
                                "content-length",
                            )
                            .then(|| {
                                value
                                    .trim()
                                    .parse::<usize>()
                                    .expect("body 长度")
                            })
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let request = String::from_utf8(bytes)
                .expect("HTTP 文本");
            let first = request
                .lines()
                .next()
                .expect("请求首行")
                .to_owned();
            recorded
                .lock()
                .expect("请求记录锁")
                .push(request);
            let rule = r#"{"id":"1","kind":"track","value":"1","label":"song 1"}"#;
            let body = if first.starts_with("POST /blocks ")
            {
                created = true;
                rule.to_owned()
            } else if first.starts_with("GET /blocks ") {
                format!(
                    "{{\"rules\":[{}]}}",
                    if created { rule } else { "" }
                )
            } else if first.starts_with("GET /daily ") {
                r#"{"tracks":[{"platform":"netease","id":"2","title":"song 2","alias":null,"artists":["LiSA"],"cover":null,"duration_ms":234000}],"unavailable":0,"hidden":1}"#.to_owned()
            } else {
                "{}".to_owned()
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("返回 HTTP 响应");
        }
    });
    requests
}

/// 走真实回调与异步成功分支:日推重载后隐藏,队列和当前曲不动;电台当前批也不动。
#[test]
fn blocking_the_current_track_reloads_daily_but_keeps_queue_and_radio()
 {
    const CHILD: &str = "OSMOSIS_181_HEADLESS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // 私有网络把编译期 API 地址与其他测试完全隔离;子进程同时隔离 Slint 全局后端。
        let status = std::process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--net", "sh", "-c", "ip link set lo up && exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("测试可执行文件"))
            .args(["--exact", "music::tests::dislike::blocking_the_current_track_reloads_daily_but_keeps_queue_and_radio", "--nocapture"])
            .env(CHILD, "1")
            .status().expect("启动私有网络测试");
        assert!(
            status.success(),
            "无头调查子进程失败: {status}"
        );
        return;
    }
    let requests = serve_catalog();
    let (ui, deck) = deck_window_event_loop();
    let songs = batch(&["1", "2"]);
    let (ticket, _) = deck.views.begin(ViewSource::Daily);
    deck.views.accept(&ticket, songs.clone(), true);
    deck.facets
        .borrow_mut()
        .load(songs.tracks.clone(), true);
    *deck.tracks.borrow_mut() = songs.tracks.clone();
    push_rows(&ui, &deck, None);
    deck.queue
        .borrow_mut()
        .replace(songs.tracks.clone(), 0);
    ui.global::<Player>().set_now_id("1".into());
    ui.global::<Viz>().set_queue_page_open(true);
    queuepage::refresh(&ui, &deck);
    let reload_deck = deck.clone();
    let reloads = Rc::new(std::cell::Cell::new(0));
    let counted = reloads.clone();
    crate::library::block::bind(
        &ui,
        &deck.blocks,
        move |ui| {
            counted.set(counted.get() + 1);
            reload_view(ui, &reload_deck);
        },
    );
    ui.global::<Library>()
        .invoke_block_track("1".into(), "song 1".into());
    assert_eq!(
        shown_ids(&ui),
        vec!["1", "2"],
        "HTTP 完成前列表尚未重载"
    );
    assert_eq!(ui.global::<Player>().get_now_id(), "1");
    let timer = slint::Timer::default();
    let started = Instant::now();
    let mut radio_started = false;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(10),
        move || {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "等待屏蔽与重载超时"
            );
            if radio_started {
                if reloads.get() < 2 {
                    return;
                }
                assert_eq!(
                    shown_ids(&ui),
                    vec!["1", "2"],
                    "电台当前批不重载"
                );
                assert_eq!(
                    ui.global::<Player>().get_now_id(),
                    "1"
                );
                assert_eq!(
                    deck.queue.borrow().tracks().len(),
                    2
                );
                playback::advance::advance(&ui, &deck);
                assert_eq!(
                    deck.queue
                        .borrow()
                        .current()
                        .map(|t| t.id.clone()),
                    Some("2".to_owned()),
                    "下一次 advance 才前进"
                );
                assert_eq!(
                    deck.queue.borrow().tracks().len(),
                    2,
                    "advance 也只跳过而不删除条目"
                );
                let requests =
                    requests.lock().expect("请求记录锁");
                assert_eq!(
                    requests
                        .iter()
                        .filter(|r| r
                            .starts_with("POST /blocks "))
                        .count(),
                    2
                );
                assert_eq!(
                    requests
                        .iter()
                        .filter(|r| r
                            .starts_with("GET /daily "))
                        .count(),
                    1
                );
                assert!(requests.iter().any(|r| {
                    r.contains("\"kind\":\"track\"")
                        && r.contains("\"value\":\"1\"")
                }));
                slint::quit_event_loop()
                    .expect("结束无头调查");
                return;
            }
            if shown_ids(&ui) != vec!["2"]
                || deck.blocks.borrow().is_empty()
            {
                return;
            }
            queuepage::refresh(&ui, &deck);
            assert_eq!(
                ui.global::<Player>().get_now_id(),
                "1",
                "屏蔽成功仍然播放原曲"
            );
            assert_eq!(
                deck.queue
                    .borrow()
                    .current()
                    .map(|t| t.id.as_str()),
                Some("1")
            );
            assert_eq!(
                ui.global::<Viz>()
                    .get_queue_rows()
                    .iter()
                    .map(|row| row.title.to_string())
                    .collect::<Vec<_>>(),
                vec!["歌 1", "歌 2"]
            );
            if !radio_started {
                let (ticket, _) =
                    deck.views.begin(ViewSource::Radio);
                deck.views.accept(
                    &ticket,
                    songs.clone(),
                    true,
                );
                deck.facets
                    .borrow_mut()
                    .load(songs.tracks.clone(), true);
                *deck.tracks.borrow_mut() =
                    songs.tracks.clone();
                push_rows(&ui, &deck, None);
                ui.global::<Library>().invoke_block_track(
                    "1".into(),
                    "song 1".into(),
                );
                radio_started = true;
                return;
            }
        },
    );
    slint::run_event_loop().expect("无头事件循环");
}
