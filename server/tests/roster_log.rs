//! 生产默认 `info` 下,「哪台设备入了册」读得出来(#113)。
//!
//! 从前是 `debug`,一次 429 查下去 grep「设备入册」零结果 —— 那是假阴性:
//! 设备入过册,只是这一行没打。
//!
//! **单独一个测试二进制,里面只有这一条。** 抓日志靠
//! `tracing::subscriber::set_default`,它只管本线程;和别的测试并行跑在同一个
//! 进程里时一行都抓不到(放在 `live_signaling.rs` 里实测:单跑、串行都绿,
//! 并行必红)。

use contract::{ClientSignal, DeviceDto};
use futures_util::{SinkExt, StreamExt};
use server::syncplay::signaling::{self, Timing};
use tokio_tungstenite::tungstenite::Message;

/// 攒日志的地方。`#[tokio::test]` 是单线程运行时,服务端 spawn 出去的任务
/// 也在这条线程上,所以它的日志落得进来。
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(
        &mut self,
        bytes: &[u8],
    ) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| {
                poisoned.into_inner()
            })
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn joining_the_roster_is_logged_at_info() {
    let sink = Sink::default();
    let writer = sink.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || writer.clone())
            .finish(),
    );

    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上端口");
    let addr = listener.local_addr().expect("取不到地址");
    let app = signaling::unauthenticated_test_router(
        Timing::default(),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await
    });

    let (mut socket, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal?account=1"),
    )
    .await
    .expect("连不上信令端点");
    let hello = ClientSignal::Hello {
        device: DeviceDto {
            id: "a".to_owned(),
            name: "设备 a".to_owned(),
        },
        protocol_version: contract::PROTOCOL_VERSION,
    };
    socket
        .send(Message::text(
            serde_json::to_string(&hello)
                .expect("序列化失败"),
        ))
        .await
        .expect("发不出 Hello");

    // 服务端先推名册、后打这一行,收到名册时它未必已经落下 —— 等一会儿,
    // 顺手把下行消息读掉。
    let read = || {
        String::from_utf8_lossy(
            &sink.0.lock().unwrap_or_else(|poisoned| {
                poisoned.into_inner()
            }),
        )
        .into_owned()
    };
    let joined = |log: &str| {
        log.lines().any(|line| {
            line.contains("设备入册")
                && line.contains("device=a")
        })
    };
    for _ in 0..50 {
        if joined(&read()) {
            break;
        }
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            socket.next(),
        )
        .await;
    }
    let log = read();
    assert!(joined(&log), "{log}");
}
