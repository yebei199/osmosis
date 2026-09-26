//! 校时走一遍真信令(#137 ⑤):客户端与服务端往返,换算出来的本机时刻对得上。
//!
//! 组的全局状态(#142)要落库,对着真库的测试在 `server/tests/groups.rs`。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use contract::DeviceDto;
use server::syncplay::signaling;
use syncplay::clock::monotonic_ns;
use syncplay::{Client, Event};

const TOKEN: &str = "test-token";
const PATIENCE: Duration = Duration::from_secs(5);

async fn start_server() -> SocketAddr {
    let app = signaling::unauthenticated_test_router(
        signaling::Timing::default(),
    );
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上端口");
    let addr = listener.local_addr().expect("取不到地址");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("服务端挂了");
    });
    addr
}

fn spawn_client(
    addr: SocketAddr,
    id: &str,
) -> (Client, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let client = Client::start(
        &format!("ws://{addr}"),
        DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        },
        || Some(TOKEN.to_owned()),
        move |event| {
            let _ = tx
                .lock()
                .expect("事件通道锁中毒")
                .send(event);
        },
    );
    (client, rx)
}

async fn wait_for<T>(
    rx: &mpsc::Receiver<Event>,
    what: &str,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if let Ok(event) = rx.try_recv()
            && let Some(picked) = pick(&event)
        {
            return picked;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "等 {what} 超时"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 客户端自己与服务端往返校时：同一台机器上，换算出来的本机时刻与真实的差不过一两毫秒。
#[tokio::test]
async fn the_clock_converges_against_the_server() {
    let addr = start_server().await;
    let (client, _rx) = spawn_client(addr, "phone");
    tokio::time::sleep(Duration::from_secs(3)).await;

    let clock = client.clock();
    let clock = clock.lock().expect("校时锁中毒");
    let epoch = clock.epoch().expect("三秒里该有往返了");
    assert_eq!(epoch, server::syncplay::clock::epoch());
    let server_us = server::syncplay::clock::now_us();
    let local = monotonic_ns();
    let converted = clock
        .to_local_ns(epoch, server_us)
        .expect("纪元对得上就换算得了");
    assert!(
        (converted - local).abs() < 2_000_000,
        "换算差了 {}µs",
        (converted - local) / 1_000
    );
}
