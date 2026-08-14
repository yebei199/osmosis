use similar_asserts::assert_eq;

use std::time::Duration;

use super::stall::{CALLBACK_BUDGET, CALLBACK_FRAMES};
use crate::codec;
use crate::stream_source::buffered_with;

use super::super::*;
use super::wav;

/// 测试用的旋钮:毫秒级的失联判定,以及小到几十 KB 的起播门槛。
///
/// 生产值意味着一条测试要枯等十几秒 —— 那种测试没人会跑,也就等于没有。
const FAST: Tuning = Tuning {
    prefetch_bytes: 16 * 1024,
    retry_timeout: Duration::from_millis(200),
    give_up_after: 2,
};

/// 起一个 HTTP 服务:先老实给 `prefix` 个字节,之后**装死** ——
/// 连接不关,也不再给任何数据。
///
/// 这才是"没网"真实的样子:wifi 连着、路由器亮着,但出口是个黑洞。拔网线
/// 是另一回事,那会立刻 ECONNRESET,走的是别的错误路径。
///
/// 裸 `TcpListener` 而不是 axum:要的行为恰恰是"不把响应收尾",
/// 任何框架都会替我们收尾,反而做不出这个场景。
///
/// **重连拿到的是空响应**:第二个连接起只给响应头。让它重发一遍数据的话,
/// `on_progress` 会把失联计数清零,于是永远走不到放弃那一步。
fn stalling_server(body: Vec<u8>, prefix: usize) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0")
        .expect("绑不上本地端口");
    let addr =
        listener.local_addr().expect("取不到本地地址");
    let served = Arc::new(AtomicUsize::new(0));

    // 线程与进程同寿:测试进程退出即回收,不值得为它造一套关停。
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let body = body.clone();
            let served = served.clone();

            std::thread::spawn(move || {
                // 请求头读到空行为止。内容不看 —— 对任何请求都给同一个回答。
                let peek = stream
                    .try_clone()
                    .expect("连接复制不了");
                let mut reader = BufReader::new(peek);
                let mut line = String::new();
                while reader
                    .read_line(&mut line)
                    .is_ok_and(|n| n > 2)
                {
                    line.clear();
                }

                let head = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Length: {}\r\n\
                     Content-Type: audio/wav\r\n\
                     Accept-Ranges: bytes\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());

                let first = served
                    .fetch_add(1, Ordering::Relaxed)
                    == 0;
                if first {
                    let _ = stream.write_all(
                        &body[..prefix.min(body.len())],
                    );
                }
                let _ = stream.flush();

                if !first || prefix < body.len() {
                    // 装死。连接留着,字节不再来。
                    std::thread::park();
                }
            });
        }
    });

    format!("http://{addr}/song.wav")
}

/// 一个**不声明 `Accept-Ranges`**、给几十 KB 就装死的服务,并把收到的每条
/// 请求头原样记下来。
///
/// 这是网易云 CDN 在真机日志里的样子(`Accept-Ranges: None`,约 62KB 后断供)。
/// 记请求是本 fixture 存在的理由:要断言的正是**重连那一条请求长什么样**。
fn range_watching_server(
    body: Vec<u8>,
    prefix: usize,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>)
{
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    let listener = TcpListener::bind("127.0.0.1:0")
        .expect("绑不上本地端口");
    let addr =
        listener.local_addr().expect("取不到本地地址");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();

    std::thread::spawn(move || {
        let mut first = true;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let body = body.clone();
            let recorder = recorder.clone();
            let serve_body = first;
            first = false;

            std::thread::spawn(move || {
                let peek = stream
                    .try_clone()
                    .expect("连接复制不了");
                let mut reader = BufReader::new(peek);
                let mut request = String::new();
                let mut line = String::new();
                while reader
                    .read_line(&mut line)
                    .is_ok_and(|n| n > 2)
                {
                    request.push_str(&line);
                    line.clear();
                }
                recorder
                    .lock()
                    .expect("记录锁不该中毒")
                    .push(request);

                // **不给 Accept-Ranges** —— 真实 CDN 就是这样,而
                // stream-download 看不见它就不敢用 range。
                let head = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Length: {}\r\n\
                     Content-Type: audio/wav\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                if serve_body {
                    let _ = stream.write_all(
                        &body[..prefix.min(body.len())],
                    );
                }
                let _ = stream.flush();
                std::thread::park();
            });
        }
    });

    (format!("http://{addr}/song.wav"), seen)
}

/// **重连必须只要还缺的那一段,哪怕服务端没声明支持 range。**
///
/// 不带 Range 的重连拿回来的是**整首歌的开头**,而那些字节会被写在当前写
/// 位置上 —— 于是歌放到一半又从头来一遍,一段接一段。这不是推演:真机日志里
/// 连续四次 `Accept-Ranges: None`,位置停在 63223 / 63219 / 63214 / 126429,
/// 几乎是同一个数和它的两倍,正是"每次重连都重新给开头那 62KB"。
#[test]
fn a_reconnect_asks_only_for_the_bytes_it_still_needs() {
    let (url, requests) =
        range_watching_server(wav(200_000), 32 * 1024);

    let (decoder, _health) = runtime()
        .block_on(load_with(&url, FAST))
        .expect("头 32KB 是完整的 WAV,起播该成功");
    // 取到停摆之后:逼它撞上超时并重连。
    let _ = decoder.take(200_000).count();

    let seen =
        requests.lock().expect("记录锁不该中毒").clone();
    assert!(
        seen.len() >= 2,
        "服务端装死了,该发生过重连,实际只收到 {} 条请求",
        seen.len()
    );
    // 比之前先压成小写。头名字本来就大小写不敏感,而线上写成什么样取决于谁在写:
    // hyper 一律小写,开发机上的本地代理却会把它改写成 `Range:` 再转发。按大写比,
    // 这条测试就在挂了代理的机器上过、在 CI 上挂 —— 已经这么挂过一次。
    assert!(
        seen[1].to_lowercase().contains("range: bytes="),
        "重连没带 Range,拿回来的会是整首歌的开头:\n{}",
        seen[1]
    );
}

/// **服务端装死时,流必须放弃,而且要留下放弃的证据。**
///
/// 不放弃的话下游会永远挂着 —— 那正是改这一版之前的样子:界面停在正在播放,
/// 声音没有,`empty()` 仍为假,谁也不知道出了什么事(见 `docs/adr/0013`)。
#[test]
fn a_silent_server_makes_the_stream_give_up() {
    // 起播门槛 16KB,先给 32KB 再装死:load 能成,读到 32KB 之后才断。
    let url = stalling_server(wav(200_000), 32 * 1024);

    let started = std::time::Instant::now();
    let (decoder, health) = runtime()
        .block_on(load_with(&url, FAST))
        .expect("头几十 KB 是完整的 WAV,起播这一步该成功");
    // 取到源结束为止。放弃机制不成立的话,这一行永远回不来。
    let _ = decoder.count();
    let waited = started.elapsed();

    assert!(
        health.gave_up(),
        "源结束了却没留下放弃的证据,下游会把断流当成放完了"
    );
    assert!(
        waited < Duration::from_secs(3),
        "等了 {waited:?} 才放弃,远超两次 200ms 失联该有的时间"
    );
}

/// **断流不是立刻没声,缓冲会把它藏一会儿。**
///
/// 这条钉的是那句"用户先听到约 5 秒沉默才看到横幅"里的前半段:流已经放弃了,
/// 而存货还在放,用户此刻毫无察觉。判据是**放弃之后仍有多少块出了声** ——
/// 缓冲要是没起作用,那个数会是零,断流当场就是死寂。
///
/// 测试里缓冲调到 1 秒、失联调到 200ms;生产是 5 秒与 5 秒,同一个形状放大。
#[test]
fn the_buffer_keeps_playing_after_the_stream_is_gone() {
    // 约 2 秒的音频后装死。放弃只要 0.4 秒,所以断流发生时存货还厚着。
    let url = stalling_server(wav(200_000), 176 * 1024);
    let (decoder, health) = runtime()
        .block_on(load_with(&url, FAST))
        .expect("头一段是完整的 WAV,起播该成功");

    // 1 秒的缓冲:比放弃所需的 0.4 秒厚,不然"藏住了"这件事无从观察。
    let mut source = buffered_with(
        codec::normalize(decoder),
        codec::SYNC_SAMPLE_RATE as usize
            * codec::SYNC_CHANNELS as usize,
    );

    let mut audible_after_give_up = 0;
    let mut gave_up = false;
    let mut blocks = 0;
    'playing: loop {
        let mut audible = false;
        for _ in 0..CALLBACK_FRAMES
            * codec::SYNC_CHANNELS as usize
        {
            match source.next() {
                Some(sample) => {
                    audible |= sample != 0.0;
                }
                // 发送端走了且通道排空 —— 这一路真的完了。
                None => break 'playing,
            }
        }

        blocks += 1;
        gave_up |= health.gave_up();
        if gave_up && audible {
            audible_after_give_up += 1;
        }

        assert!(
            blocks < 400,
            "源迟迟不结束,缓冲那条线程没收工"
        );
        // 按实时节奏取,否则消费端无限快,缓冲永远是空的。
        std::thread::sleep(CALLBACK_BUDGET);
    }

    assert!(gave_up, "服务端装死了,这条流该放弃");
    assert!(
        audible_after_give_up >= 15,
        "放弃之后只出了 {audible_after_give_up} 块声音(约 {}ms),\
         缓冲没把断流藏住,用户当场就听到死寂",
        audible_after_give_up * 21
    );
}

/// 对照组:整段都送到的流,结束时**不能**留下放弃的证据。
///
/// 没有这条,上一条也可能是"这个句柄永远为真"造成的 —— 那样自动续播会
/// 再也不切歌,而两条测试都还是绿的。
#[test]
fn a_complete_stream_never_reports_a_give_up() {
    let body = wav(200_000);
    let complete = body.len();
    let url = stalling_server(body, complete);

    let (decoder, health) = runtime()
        .block_on(load_with(&url, FAST))
        .expect("完整的 WAV 该能起播");
    let played = decoder.count();

    assert!(played > 0, "一个采样都没放出来");
    assert!(!health.gave_up(), "整段都送到了,却报告成断流");
}
