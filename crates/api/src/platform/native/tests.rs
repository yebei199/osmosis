use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use similar_asserts::assert_eq;

use super::{send, sweep_dir};
use crate::ApiError;

/// 建一个空的临时目录,名字带上用例名免得两个用例互相踩。
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("osmosis-sweep-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建不出临时目录");
    dir
}

/// 写一个指定大小、指定"有多旧"的文件。
///
/// mtime 用 `File::set_modified` 精确设定,而不是靠 sleep 拉开时间差 ——
/// 那种测试在慢机器上会时好时坏。
fn file(
    dir: &Path,
    name: &str,
    size: usize,
    age_secs: u64,
) {
    let path = dir.join(name);
    std::fs::write(&path, vec![0u8; size])
        .expect("写不出测试文件");
    let handle = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("打不开测试文件");
    handle
        .set_modified(
            SystemTime::now()
                - Duration::from_secs(age_secs),
        )
        .expect("设不了 mtime");
}

fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("读不到临时目录")
        .filter_map(|entry| {
            Some(
                entry
                    .ok()?
                    .file_name()
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    found.sort();
    found
}

/// 超出预算时从最旧的删起,删到线下就停手。
///
/// 删过头的现象是刚看过的那一屏封面下次还要重取 —— 缓存在,却总不命中。
#[test]
fn the_sweep_deletes_oldest_first_until_under_budget() {
    let dir = scratch("oldest-first");
    file(&dir, "old", 100, 300);
    file(&dir, "mid", 100, 200);
    file(&dir, "new", 100, 100);

    // 预算 250:删掉最旧那一个就到 200,不该再动第二个
    sweep_dir(&dir, 250);

    assert_eq!(names(&dir), vec!["mid", "new"]);
}

/// 没超预算时一个都不删 —— 清理不该在正常情况下动手。
#[test]
fn the_sweep_keeps_everything_under_budget() {
    let dir = scratch("under-budget");
    file(&dir, "a", 100, 200);
    file(&dir, "b", 100, 100);

    sweep_dir(&dir, 1024);

    assert_eq!(names(&dir), vec!["a", "b"]);
}

/// 目录还不存在时安静返回 —— 第一次启动就是这个样子,不是故障。
#[test]
fn the_sweep_tolerates_a_missing_directory() {
    let dir = scratch("missing").join("not-created-yet");
    sweep_dir(&dir, 0);
    assert!(!dir.exists());
}

// ── 一次真实往返长什么样 ──────────────────────────────────────────
//
// 下面几条对着本机一个真的 socket 发请求。`send` 把方法、登录态、请求体拼
// 进 reqwest 内部,那些字段在进程里没有任何可读的出口 —— 只有让它真的发出去、
// 在另一头把原文接住,才证明得了「Authorization 头确实带上了」这类事。

/// 服务端接住的一条请求。头的键统一成小写,HTTP/1.1 不区分大小写。
struct Captured {
    start_line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// 起一个对任何请求都给同一个回答的 HTTP 服务,并把收到的请求原样交出来。
///
/// 裸 `TcpListener` 而不是某个框架:要断言的正是**发出去的那条请求的原文**,
/// 框架会先把它解析成自己的类型,反倒看不见原文;这里也不需要框架的别的东西。
///
/// 线程与进程同寿 —— 测试进程退出即回收,不值得为它造一套关停。
fn recording_server(
    response: String,
) -> (String, std::sync::mpsc::Receiver<Captured>) {
    use std::io::{BufRead, BufReader, Read, Write};

    let listener =
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("绑不上本地端口");
    let addr =
        listener.local_addr().expect("取不到本地地址");
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let peek =
                stream.try_clone().expect("连接复制不了");
            let mut reader = BufReader::new(peek);

            let mut start_line = String::new();
            let _ = reader.read_line(&mut start_line);

            let mut headers: Vec<(String, String)> =
                Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0)
                    == 0
                {
                    break;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) =
                    line.split_once(':')
                {
                    headers.push((
                        name.trim().to_ascii_lowercase(),
                        value.trim().to_owned(),
                    ));
                }
            }

            // 请求体按 Content-Length 读满。少读一个字节,下一条请求
            // 就会从半截请求体开始解析,现象是莫名其妙的 400。
            let length: usize = headers
                .iter()
                .find(|(name, _)| name == "content-length")
                .and_then(|(_, value)| value.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            if length > 0 {
                let _ = reader.read_exact(&mut body);
            }

            let _ = sender.send(Captured {
                start_line: start_line
                    .trim_end()
                    .to_owned(),
                headers,
                body: String::from_utf8_lossy(&body)
                    .into_owned(),
            });

            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://{addr}"), receiver)
}

/// 拼一条完整的 HTTP 响应。`Connection: close` 让每次往返各用一条连接,
/// 上面那个单线程服务因此不会卡在复用连接的读上。
fn http_response(
    status: &str,
    content_type: &str,
    body: &str,
) -> String {
    format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n\
         {body}",
        body.len()
    )
}

fn captured(
    receiver: &std::sync::mpsc::Receiver<Captured>,
) -> Captured {
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("服务端没收到任何请求")
}

/// 登录态跟着会话走:没登录时不带 `Authorization`,登录后每条请求都带。
///
/// 两半写在**一个**测试里:token 是进程级全局状态,拆开会并行地互相踩。
///
/// 这个头是在 reqwest 内部拼的,纯函数测不到它 —— 而漏了它的现象是那条路由
/// 一律 401,查的人会先去翻服务端。反过来,登出之后还带着旧 token 同样是错:
/// 服务端会把它当成一次仍然有效的会话。
///
/// 用 `#[test]` 加一个手起的 runtime 而不是 `#[tokio::test]`:那把锁要罩住
/// **整条**测试,而在 async fn 里跨 await 持有 `MutexGuard` 是 clippy 的红线。
/// 这里没有别的任务在跑,同步地 block_on 两次往返就够了。
#[test]
fn the_authorization_header_follows_the_session() {
    let _guard = crate::session::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // 登录态落盘处指到临时目录,免得动到真实的那一份
    let dir = std::env::temp_dir()
        .join("osmosis-send-auth-header");
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: 拿着 TEST_LOCK,此刻没有别的测试在读写这个变量
    unsafe {
        std::env::set_var(
            "OSMOSIS_SESSION_FILE",
            dir.join("session"),
        );
    }

    let (base, requests) = recording_server(http_response(
        "200 OK",
        "application/json",
        "{}",
    ));

    // 请求本身跑在 `send` 自己那个后台 runtime 上,这里等的只是它的
    // JoinHandle —— 单线程 runtime 足够
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    crate::session::clear();
    local
        .block_on(send::<()>(
            reqwest::Method::GET,
            base.clone(),
            None,
        ))
        .expect("没登录也该发得出请求");
    let anonymous = captured(&requests);
    assert_eq!(
        anonymous.header("authorization"),
        None,
        "没登录却带上了 Authorization 头"
    );

    crate::session::set("a-token");
    local
        .block_on(send::<()>(
            reqwest::Method::GET,
            base,
            None,
        ))
        .expect("登录后的请求也该发得出去");
    let authorized = captured(&requests);
    assert_eq!(
        authorized.header("authorization"),
        Some("Bearer a-token"),
        "登录之后请求没有带上登录态"
    );

    crate::session::clear();
}

/// 有请求体时按 JSON 发,并且方法用调用方给的那个。
///
/// 写操作全靠这条路把参数送出去。请求体漏了或方法退回 GET,服务端看到的是
/// 一条语义完全不同的请求 —— 而客户端这侧只会看到一个 4xx。
#[tokio::test]
async fn a_request_body_goes_out_as_json() {
    let (base, requests) = recording_server(http_response(
        "204 No Content",
        "application/json",
        "",
    ));

    send(
        reqwest::Method::PUT,
        base,
        Some(serde_json::json!({ "liked": true })),
    )
    .await
    .expect("带请求体的写操作该发得出去");

    let request = captured(&requests);
    assert!(
        request.start_line.starts_with("PUT "),
        "方法没跟着调用方走: {}",
        request.start_line
    );
    assert_eq!(
        request.header("content-type"),
        Some("application/json")
    );
    assert_eq!(request.body, r#"{"liked":true}"#);
}

/// 非 2xx 时把服务端给的 code 带回调用方手里。
///
/// `error_for_status` 做不到这件事:它只看状态码,响应体连同里面的 code 一起
/// 被丢掉。上层按 code 分支(比如「网易云没登录 → 提示扫码」),拿到一句
/// 「HTTP 503」就只能一律当成网络故障去重试,而重试一万次也登不上。
#[tokio::test]
async fn a_rejected_request_keeps_the_code_the_server_gave()
{
    let (base, _requests) = recording_server(
        http_response(
            "503 Service Unavailable",
            "application/json",
            r#"{"code":"netease_not_logged_in","message":"未登录"}"#,
        ),
    );

    let failure =
        send::<()>(reqwest::Method::GET, base, None)
            .await
            .expect_err("503 却被当成了成功");

    match failure {
        ApiError::Server { code, message } => {
            assert_eq!(code, "netease_not_logged_in");
            assert_eq!(message, "未登录");
        }
        other => panic!(
            "服务端明确拒绝了,却退化成了别的错误: {other:?}"
        ),
    }
}

/// 连不上时是 `Transport`,不是 `Server`。
///
/// 两者的区别是**有没有得到答复**:上层据此决定是重试还是照 code 分支。
/// 混成一个的话,断网会被当成服务端的拒绝,而那不会自己好。
#[tokio::test]
async fn an_unreachable_server_is_a_transport_error() {
    // 端口 1 是特权端口,本机上不会有人监听,连接立刻被拒 ——
    // 不必等超时,也不必先绑一个端口再放开
    let failure = send::<()>(
        reqwest::Method::GET,
        "http://127.0.0.1:1".to_owned(),
        None,
    )
    .await
    .expect_err("连不上却当成功返回了");

    assert!(
        matches!(failure, ApiError::Transport(_)),
        "话没传到,不该报成服务端拒绝: {failure:?}"
    );
}
