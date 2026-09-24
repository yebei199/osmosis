use std::path::Path;
use std::time::{Duration, SystemTime};

use similar_asserts::assert_eq;

use super::{
    off_thread, route_of, send, send_json, sweep_dir,
};
use crate::ApiError;

/// 建一个独有的空临时目录,随返回值一起删。
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("建不出临时目录")
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
    let tmp = scratch();
    let dir = tmp.path();
    file(dir, "old", 100, 300);
    file(dir, "mid", 100, 200);
    file(dir, "new", 100, 100);

    // 预算 250:删掉最旧那一个就到 200,不该再动第二个
    sweep_dir(dir, 250);

    assert_eq!(names(dir), vec!["mid", "new"]);
}

/// 没超预算时一个都不删 —— 清理不该在正常情况下动手。
#[test]
fn the_sweep_keeps_everything_under_budget() {
    let tmp = scratch();
    let dir = tmp.path();
    file(dir, "a", 100, 200);
    file(dir, "b", 100, 100);

    sweep_dir(dir, 1024);

    assert_eq!(names(dir), vec!["a", "b"]);
}

/// 目录还不存在时安静返回 —— 第一次启动就是这个样子,不是故障。
#[test]
fn the_sweep_tolerates_a_missing_directory() {
    let tmp = scratch();
    let dir = tmp.path().join("not-created-yet");
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
    let dir = tempfile::tempdir().expect("建不出临时目录");
    // SAFETY: 拿着 TEST_LOCK,此刻没有别的测试在读写这个变量
    unsafe {
        std::env::set_var(
            "OSMOSIS_SESSION_FILE",
            dir.path().join("session"),
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

/// 环境里有代理变量时照样直连。
///
/// 集群地址在 tailnet 里,本机代理根本到不了它:v0.1.1 的现象是启动器继承了
/// 用户会话的 `HTTPS_PROXY`,登录一律「连不上服务端」,而把变量去掉就通。
/// 关掉 reqwest 的 `system-proxy` 特性挡不住这件事 —— 那个特性只管 macOS 和
/// Windows 的系统设置,环境变量是 hyper-util 无条件读的。
///
/// 代理指向端口 1(特权端口,本机不会有人监听):真去走代理就连不上,
/// 这条用例于是必然失败。
#[test]
fn a_proxy_in_the_environment_is_ignored() {
    let _guard = crate::session::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // SAFETY: 拿着 TEST_LOCK,此刻没有别的测试在读写这两个变量。
    // 设完不还原:客户端本来就该无视它们,留着反而覆盖到后面的用例。
    unsafe {
        std::env::set_var(
            "HTTP_PROXY",
            "http://127.0.0.1:1",
        );
        std::env::set_var(
            "HTTPS_PROXY",
            "http://127.0.0.1:1",
        );
    }

    let (base, requests) = recording_server(http_response(
        "200 OK",
        "application/json",
        "{}",
    ));

    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    local
        .block_on(send::<()>(
            reqwest::Method::GET,
            base,
            None,
        ))
        .expect("环境里有代理变量时请求没能直连出去");

    captured(&requests);
}

/// 后台活真的跑在别的线程上,结果原样回到调用方。
///
/// 在调用方线程上跑也能拿到同一个结果 —— 只有线程号分得出「挪走了」和
/// 「没挪走」,而没挪走的现象是界面照冻,测试照绿。
#[test]
fn off_thread_work_runs_on_another_thread() {
    let caller = std::thread::current().id();
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    let ran_on = local
        .block_on(off_thread(|| {
            std::thread::current().id()
        }))
        .expect("活没有 panic,该有结果");

    assert_ne!(ran_on, caller, "活还在调用方线程上跑");
}

/// 活自己 panic 了,调用方拿到 `None`,而不是跟着一起倒下。
///
/// 调用方是 UI 线程:一张解不动的封面不该把整个界面带走。
#[test]
fn a_panicking_job_yields_none() {
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    let result: Option<()> =
        local.block_on(off_thread(|| panic!("解码炸了")));

    assert_eq!(result, None);
}

/// 记下自己是在哪个线程上被反序列化出来的。
struct DecodedOn(std::thread::ThreadId);

impl<'de> serde::Deserialize<'de> for DecodedOn {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self(std::thread::current().id()))
    }
}

/// 响应体的反序列化也在后台线程上,不在等结果的那个线程上。
///
/// 上千首的歌单解一次是实打实的 CPU 活;请求挪走了而解码没挪,
/// 界面照样冻在那一下(#117)。
#[test]
fn the_response_is_decoded_off_the_callers_thread() {
    let (base, _requests) = recording_server(
        http_response("200 OK", "application/json", "{}"),
    );
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    let decoded = local
        .block_on(send_json::<(), DecodedOn>(
            reqwest::Method::GET,
            base,
            None,
        ))
        .expect("该拿到一个解出来的响应");

    assert_ne!(
        decoded.0,
        std::thread::current().id(),
        "响应体还在调用方线程上解"
    );
}

/// 起一个保持连接的 HTTP 服务,数它一共接了几条连接。
///
/// 与 [`recording_server`] 相反,这里**不**回 `Connection: close`:每条连接
/// 一个线程,在上面一直按序应答,直到客户端关掉它。
fn keep_alive_server()
-> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>)
{
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::Ordering;

    let listener =
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("绑不上本地端口");
    let addr =
        listener.local_addr().expect("取不到本地地址");
    let accepted = std::sync::Arc::new(
        std::sync::atomic::AtomicUsize::new(0),
    );
    let counter = accepted.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(
                    stream
                        .try_clone()
                        .expect("连接复制不了"),
                );
                loop {
                    // 读完一条不带请求体的请求头;对端关了就收工
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader
                            .read_line(&mut line)
                            .unwrap_or(0)
                            == 0
                        {
                            return;
                        }
                        if line.trim_end().is_empty() {
                            break;
                        }
                    }
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\n\
                          Content-Type: application/json\r\n\
                          Content-Length: 2\r\n\r\n{}",
                    );
                    let _ = stream.flush();
                }
            });
        }
    });

    (format!("http://{addr}"), accepted)
}

/// 同一主机的两次请求走同一条连接。
///
/// 每次新建客户端时,每条请求都要重做一遍 TCP + TLS 握手:开发机到生产
/// 实测 0.45–0.7s,连 36 字节的 `/health` 在手机上也要 1.5s 起(#122)。
#[tokio::test]
async fn consecutive_requests_reuse_one_connection() {
    let (base, accepted) = keep_alive_server();

    for _ in 0..2 {
        send::<()>(
            reqwest::Method::GET,
            base.clone(),
            None,
        )
        .await
        .expect("本机服务该应答");
    }

    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "两次请求各建了一条连接,连接没有复用"
    );
}
// ── 每次调用一行分段耗时(#121) ─────────────────────────────────────

/// 把 `log` 的输出接到一个进程级的缓冲里,好断言「打了哪几行」。
///
/// `log` 一个进程只认一个 logger,装一次、各用例按自己独有的路径筛自己那几行。
fn logged_lines(marker: &str) -> Vec<String> {
    capture_logs();
    LOG_LINES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|line| line.contains(marker))
        .cloned()
        .collect()
}

static LOG_LINES: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        LOG_LINES
            .lock()
            .unwrap_or_else(|poisoned| {
                poisoned.into_inner()
            })
            .push(record.args().to_string());
    }

    fn flush(&self) {}
}

fn capture_logs() {
    static INSTALL: std::sync::Once =
        std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = log::set_logger(&Capture);
        log::set_max_level(log::LevelFilter::Info);
    });
}

/// 一次解码成功的调用只打一行,四段耗时与状态码、字节数都在上面。
///
/// 这一行是区分「网络慢」和「界面冻」的唯一凭据:少一段,
/// 那一段的时间就只能靠猜(#121)。
#[test]
fn a_decoded_call_logs_one_line_with_every_stage() {
    capture_logs();
    let (base, _requests) =
        recording_server(http_response(
            "200 OK",
            "application/json",
            r#"{"x":1}"#,
        ));
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    local
        .block_on(send_json::<(), serde_json::Value>(
            reqwest::Method::GET,
            format!("{base}/timing-decoded/42?q=secret"),
            None,
        ))
        .expect("该拿到一个解出来的响应");

    let lines = logged_lines("/timing-decoded/");
    assert_eq!(
        lines.len(),
        1,
        "一次调用该打恰好一行: {lines:?}"
    );
    let line = &lines[0];
    for field in [
        "GET",
        "/timing-decoded/:id",
        "status=200",
        "bytes=7",
        "head=",
        "body=",
        "decode=",
        "total=",
    ] {
        assert!(line.contains(field), "缺 {field}: {line}");
    }
    assert!(
        !line.contains("secret") && !line.contains("42"),
        "查询串与路径里的 id 不该进日志: {line}"
    );
}

/// 服务端拒绝了也打一行,带上状态码与失败归类 —— 慢在失败的那次上时,
/// 没有这一行就看不见它。
#[test]
fn a_rejected_call_still_logs_its_line() {
    capture_logs();
    let (base, _requests) =
        recording_server(http_response(
            "503 Service Unavailable",
            "application/json",
            r#"{"code":"upstream","message":"回源失败"}"#,
        ));
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    local
        .block_on(send_json::<(), serde_json::Value>(
            reqwest::Method::GET,
            format!("{base}/timing-rejected"),
            None,
        ))
        .expect_err("503 却被当成了成功");

    let lines = logged_lines("/timing-rejected");
    assert_eq!(
        lines.len(),
        1,
        "失败的调用也该打恰好一行: {lines:?}"
    );
    assert!(
        lines[0].contains("status=503"),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].contains("error=server"),
        "{}",
        lines[0]
    );
}

/// 不看响应体的写操作同样打一行。
#[test]
fn a_no_content_call_logs_one_line() {
    capture_logs();
    let (base, _requests) =
        recording_server(http_response(
            "204 No Content",
            "application/json",
            "",
        ));
    let local =
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("起不了测试用的 runtime");

    local
        .block_on(send::<()>(
            reqwest::Method::PUT,
            format!("{base}/timing-write"),
            None,
        ))
        .expect("写操作该发得出去");

    let lines = logged_lines("/timing-write");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("PUT"), "{}", lines[0]);
    assert!(
        lines[0].contains("status=204"),
        "{}",
        lines[0]
    );
}

/// 进日志的是路由的形状,不是具体的那条地址:查询串整个去掉,
/// 带数字或大写的路径段(id、二维码 key)换成 `:id`。
#[test]
fn a_route_keeps_only_the_shape_of_the_path() {
    let cases = [
        ("http://h/daily", "/daily"),
        (
            "http://h/search/tracks?q=%E5%91%A8",
            "/search/tracks",
        ),
        (
            "http://h/playlists/platform/123456/tracks",
            "/playlists/platform/:id/tracks",
        ),
        ("http://h/netease/qr/a1B2-c3", "/netease/qr/:id"),
        ("http://h/queues/7/head", "/queues/:id/head"),
        ("not a url", "?"),
    ];
    for (url, route) in cases {
        assert_eq!(route_of(url), route, "{url}");
    }
}
