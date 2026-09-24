use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use similar_asserts::assert_eq;

use crate::open_timing::{Connection, Laps};

use super::super::*;
use super::wav;

/// 测试用的旋钮：小门槛，好让开流在毫秒级完成。
const FAST: Tuning = Tuning {
    prefetch_bytes: 16 * 1024,
    retry_timeout: Duration::from_millis(500),
    give_up_after: 2,
};

/// 一个支持 keep-alive 的 HTTP 服务：同一条连接上一个请求接一个请求地回整个 body,
/// 并数它一共接受了几条 TCP 连接。
///
/// 数连接是本 fixture 存在的理由:「第二次开流复用了连接」在客户端这一侧只是一个说法，
/// 服务端只接受过一条连接才是它真的发生了。
fn keep_alive_server(
    body: Vec<u8>,
) -> (String, Arc<AtomicUsize>) {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .expect("绑不上本地端口");
    let addr =
        listener.local_addr().expect("取不到本地地址");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(
                    stream
                        .try_clone()
                        .expect("连接复制不了"),
                );
                loop {
                    // 读完一个请求的头(空行为止);连接关了就收工。
                    let mut line = String::new();
                    let mut got_any = false;
                    while reader
                        .read_line(&mut line)
                        .is_ok_and(|n| n > 0)
                    {
                        got_any = true;
                        if line == "\r\n" {
                            break;
                        }
                        line.clear();
                    }
                    if !got_any {
                        return;
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Length: {}\r\n\
                         Content-Type: audio/wav\r\n\
                         Accept-Ranges: bytes\r\n\r\n",
                        body.len()
                    );
                    if stream
                        .write_all(head.as_bytes())
                        .is_err()
                        || stream.write_all(&body).is_err()
                    {
                        return;
                    }
                    let _ = stream.flush();
                }
            });
        }
    });

    (format!("http://{addr}/song.wav"), accepted)
}

/// 开一次、把整首取完(下载任务读到流尾，连接才回到池里),交回计时。
fn open_and_drain(url: &str) -> Laps {
    let (decoder, _health, laps) = runtime()
        .block_on(load_timed(url, FAST))
        .expect("完整的 WAV 该能起播");
    let _ = decoder.count();
    laps
}

/// **第一次开流是新连接，第二次复用它。** 冷热连接的区分全靠这一条:冷连接要付握手，
/// 热连接不付;分错了，量出来的「握手占比」就是错的。
#[test]
fn a_first_open_connects_and_the_next_one_reuses_the_connection()
 {
    let (url, accepted) = keep_alive_server(wav(100_000));

    let first = open_and_drain(&url);
    let second = open_and_drain(&url);

    assert_eq!(first.connection(), Connection::New);
    assert_eq!(second.connection(), Connection::Reused);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "服务端该只接受过一条连接"
    );
}

/// 一次冷开流，每一段都量到了;IP 直连不走 DNS。
#[test]
fn every_segment_of_a_cold_open_is_measured() {
    let (url, _accepted) = keep_alive_server(wav(100_000));

    let laps = open_and_drain(&url);

    assert_eq!(laps.host, "127.0.0.1");
    assert_eq!(laps.dns, None, "IP 直连不该有 DNS 这一段");
    assert!(
        laps.connect.is_some(),
        "冷开流该量到建连: {laps:?}"
    );
    assert!(laps.head.is_some(), "该量到首字节: {laps:?}");
    assert!(
        laps.prefetch.is_some(),
        "该量到攒够预读: {laps:?}"
    );
    assert!(
        laps.decode.is_some(),
        "该量到解码器建好: {laps:?}"
    );
    assert!(laps.total.is_some(), "该有总耗时: {laps:?}");
}

/// 日志那一行：新连接列出握手两段，复用的不列;写法与 `api:` 那行一致(`名字=毫秒ms`)。
#[test]
fn the_timing_line_lists_each_segment() {
    let ms = Duration::from_millis;
    let cold = Laps {
        host: "cdn.example".to_owned(),
        dns: Some(ms(3)),
        connect: Some(ms(85)),
        head: Some(ms(40)),
        prefetch: Some(ms(120)),
        decode: Some(ms(15)),
        total: Some(ms(263)),
    };
    let warm = Laps {
        host: "cdn.example".to_owned(),
        head: Some(ms(38)),
        prefetch: Some(ms(90)),
        decode: Some(ms(14)),
        total: Some(ms(142)),
        ..Laps::default()
    };

    assert_eq!(
        cold.line(),
        "stream: host=cdn.example conn=new dns=3ms connect=85ms head=40ms \
         prefetch=120ms decode=15ms total=263ms"
    );
    assert_eq!(
        warm.line(),
        "stream: host=cdn.example conn=reused head=38ms prefetch=90ms \
         decode=14ms total=142ms"
    );
}
