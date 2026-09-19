//! 遥控器模式走一遍真信令:接管、发命令、回上报、被顶掉。
//!
//! 两台「设备」都在本测试进程里,对着一个同样起在进程里的服务端说话 ——
//! 与 `handshake.rs` 同一套办法。这里不碰 WebRTC:遥控器模式一个字节的音频
//! 都不过网,被控端自己拉直链自己播(`docs/adr/0030`)。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use contract::{
    DeviceDto, RemoteCommand, RemotePlayState,
    RemoteStateDto,
};
use server::signaling;
use syncplay::{Client, Event};

/// 测试路由不鉴权,但 token 仍要是个合法的头值。
const TOKEN: &str = "test-token";

/// 等一条事件的上限。本地回环上一条 WebSocket 往返是毫秒级,
/// 五秒是给 CI 上的慢机器留的。
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

fn device(id: &str) -> DeviceDto {
    DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    }
}

/// 起一个客户端,并把它抛出的事件汇进一条通道。
///
/// 事件回调跑在同播自己的后台线程上,所以这里用 `std::sync::mpsc`
/// 而不是 tokio 的 —— 测试在 tokio 上等,但发的那一头不是。
fn spawn_client(
    addr: SocketAddr,
    id: &str,
) -> (Client, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let client = Client::start(
        &format!("ws://{addr}"),
        device(id),
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

/// 等一条满足条件的事件,其余的丢掉(名册会先到几条)。
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

/// 等两台设备都出现在彼此的名册里 —— 接管之前必须先看得见对方。
async fn wait_until_both_online(
    rx: &mpsc::Receiver<Event>,
    other: &str,
) {
    let other = other.to_owned();
    wait_for(rx, "名册", |event| match event {
        Event::Roster(devices) => devices
            .iter()
            .any(|d| d.id == other)
            .then_some(()),
        _ => None,
    })
    .await;
}

fn report(position_ms: u64) -> RemoteStateDto {
    RemoteStateDto {
        track: None,
        position_ms,
        state: RemotePlayState::Playing,
        queue: Vec::new(),
        queue_index: 0,
        volume: 1.0,
        sent_at: position_ms,
    }
}

/// 接管一台设备:遥控器拿到代次,被控端知道自己被谁遥控了。
#[tokio::test]
async fn claiming_reaches_both_ends() {
    let addr = start_server().await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (_pc, pc_rx) = spawn_client(addr, "pc");
    wait_until_both_online(&phone_rx, "pc").await;

    phone.claim("pc");

    let target =
        wait_for(&phone_rx, "ControlGranted", |event| {
            match event {
                Event::ControlGranted {
                    target, ..
                } => Some(target.clone()),
                _ => None,
            }
        })
        .await;
    assert_eq!(target, "pc");

    let by =
        wait_for(
            &pc_rx,
            "ControlledBy",
            |event| match event {
                Event::ControlledBy { device } => {
                    Some(device.id.clone())
                }
                _ => None,
            },
        )
        .await;
    assert_eq!(by, "phone");
}

/// 命令从遥控器到被控端,上报从被控端回遥控器。
///
/// 两个方向一条测试:它们是同一条回路,分开验证不了「转到了对的那一端」。
#[tokio::test]
async fn commands_and_reports_cross_in_both_directions() {
    let addr = start_server().await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (pc, pc_rx) = spawn_client(addr, "pc");
    wait_until_both_online(&phone_rx, "pc").await;
    phone.claim("pc");
    wait_for(&pc_rx, "ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;

    phone.command(RemoteCommand::Seek { ms: 42_000 });
    let cmd =
        wait_for(&pc_rx, "Command", |event| match event {
            Event::Command { cmd } => Some(cmd.clone()),
            _ => None,
        })
        .await;
    assert_eq!(cmd, RemoteCommand::Seek { ms: 42_000 });

    pc.report(report(7_000));
    let state =
        wait_for(&phone_rx, "RemoteState", |event| {
            match event {
                Event::RemoteState { from, state } => {
                    Some((from.clone(), state.position_ms))
                }
                _ => None,
            }
        })
        .await;
    assert_eq!(state, ("pc".to_owned(), 7_000));
}

/// 第二台主动接管,第一台收到撤权。
#[tokio::test]
async fn a_second_controller_revokes_the_first() {
    let addr = start_server().await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (_pc, pc_rx) = spawn_client(addr, "pc");
    let (spare, spare_rx) = spawn_client(addr, "spare");
    wait_until_both_online(&phone_rx, "pc").await;
    wait_until_both_online(&spare_rx, "pc").await;
    phone.claim("pc");
    wait_for(&pc_rx, "ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;

    spare.claim("pc");

    let by =
        wait_for(&phone_rx, "ControlRevoked", |event| {
            match event {
                Event::ControlRevoked { by } => {
                    Some(by.clone())
                }
                _ => None,
            }
        })
        .await;
    assert_eq!(by, "spare");
}

/// 要快照:被控端收到请求,回一条上报,遥控器收到它。
#[tokio::test]
async fn a_snapshot_request_comes_back_as_a_report() {
    let addr = start_server().await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (pc, pc_rx) = spawn_client(addr, "pc");
    wait_until_both_online(&phone_rx, "pc").await;
    phone.claim("pc");
    wait_for(&pc_rx, "ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;

    phone.request_snapshot();

    wait_for(&pc_rx, "SnapshotRequest", |event| {
        matches!(event, Event::SnapshotRequest)
            .then_some(())
    })
    .await;
    pc.report(report(1_234));
    let position =
        wait_for(&phone_rx, "RemoteState", |event| {
            match event {
                Event::RemoteState { state, .. } => {
                    Some(state.position_ms)
                }
                _ => None,
            }
        })
        .await;
    assert_eq!(position, 1_234);
}

/// 被控端按了「退出被遥控」:遥控器失权,而且知道是被控端撤的。
#[tokio::test]
async fn the_target_can_take_its_control_back() {
    let addr = start_server().await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (pc, pc_rx) = spawn_client(addr, "pc");
    wait_until_both_online(&phone_rx, "pc").await;
    phone.claim("pc");
    wait_for(&pc_rx, "ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;

    pc.exit_controlled();

    let by =
        wait_for(&phone_rx, "ControlRevoked", |event| {
            match event {
                Event::ControlRevoked { by } => {
                    Some(by.clone())
                }
                _ => None,
            }
        })
        .await;
    assert_eq!(by, "pc");
}
