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
use server::syncplay::signaling;
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

/// 一份最小的小状态。序号跟着位置走,免得每个调用点再造一个。
fn report(position_ms: u64) -> RemoteStateDto {
    RemoteStateDto {
        track: None,
        position_ms,
        state: RemotePlayState::Playing,
        volume: 1.0,
        queue_id: None,
        revision: None,
        applied_revision: None,
        entry_id: None,
        queue_len: 0,
        epoch: 1_700_000_000_000,
        state_seq: position_ms,
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

/// 槽位上查不到时,被控端的上报换回一条 `NotControlled`。
///
/// 走一遍真信令,因为这条自愈的价值全在「被控端**自己**收得到」上:
/// 服务端槽位可能在它不知情时就没了(重启、关系被别处撤掉),而它的锁定态
/// 只有用户按「退出被遥控」才清。没有这条,它就挂着假横幅锁死本机(#102 F-004)。
#[tokio::test]
async fn a_target_without_a_grant_is_told_it_is_free() {
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

    // 被控端自己退出:槽位没了,但它此刻并不知道服务端还记不记得这件事。
    pc.exit_controlled();
    wait_for(&phone_rx, "ControlRevoked", |event| {
        matches!(event, Event::ControlRevoked { .. })
            .then_some(())
    })
    .await;

    // 槽位已经空了,这一条上报因此落在「查无槽位」那一支上。
    pc.report(report(7_000));

    wait_for(&pc_rx, "NotControlled", |event| {
        matches!(event, Event::NotControlled).then_some(())
    })
    .await;
}

// -----------------------------------------------------------------
// 断线与接管失败(#118)
//
// 两端的遥控态此前只有收到服务端消息才清,而断着的时候消息过不来;
// 接管失败时服务端只回一行报错,遥控器的输出停在那台设备上。
// -----------------------------------------------------------------

/// 一根可以拔的线:客户端连它,它连服务端。拔过之后照样接新连接,
/// 客户端要能重连上来。
///
/// 拔线时只掐客户端那一半,服务端那一半留着不关 —— 移动网络上掉线就是
/// 这个样子:服务端要等探活(默认一分钟)才发现,而客户端早就重连上来了。
struct Cable {
    addr: SocketAddr,
    cut: tokio::sync::broadcast::Sender<()>,
}

impl Cable {
    /// 拔线:掐掉此刻所有连接的客户端那一半。
    fn cut(&self) {
        let _ = self.cut.send(());
    }
}

/// 起一根通往 `upstream` 的线。
async fn cable(upstream: SocketAddr) -> Cable {
    use tokio::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑不上端口");
    let addr = listener.local_addr().expect("取不到地址");
    let (cut, _) = tokio::sync::broadcast::channel(4);
    // 拔下来的服务端那一半收在这里,测试结束前都不关:关了服务端就立刻知道
    // 对端走了,测的就成了「好好地断开」,而不是掉线。
    let limbo: Arc<std::sync::Mutex<Vec<TcpStream>>> =
        Arc::default();

    let cutter = cut.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) =
                listener.accept().await
            else {
                return;
            };
            let Ok(mut server) =
                TcpStream::connect(upstream).await
            else {
                continue;
            };
            let mut cut = cutter.subscribe();
            let limbo = limbo.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
                    _ = cut.recv() => {
                        drop(client);
                        limbo.lock().expect("锁中毒").push(server);
                    }
                }
            });
        }
    });

    Cable { addr, cut }
}

/// 信令断了要说出来。
///
/// 此前断线时客户端一声不吭,界面上的「被遥控」锁停在断线前,直到服务端
/// 再发一条消息 —— 而断着的时候它发不过来,本机于是连歌都点不了。
#[tokio::test]
async fn a_dropped_link_is_reported() {
    let addr = start_server().await;
    let line = cable(addr).await;
    let (_pc, pc_rx) = spawn_client(line.addr, "pc");
    wait_until_both_online(&pc_rx, "pc").await;

    line.cut();

    wait_for(&pc_rx, "Disconnected", |event| {
        matches!(event, Event::Disconnected).then_some(())
    })
    .await;
}

/// 被控端掉线又重连上来,遥控关系就此结束,遥控器得到撤权。
///
/// 掉线期间被控端已经解锁、可能在本机放着别的歌了;服务端却还没发现旧连接
/// 死了,重连把它顶替掉,槽位原样留着(被顶替的那次出册不清槽)。不结束的话
/// 遥控器接着往一台已经不听它的设备发命令,两端对「谁在遥控谁」各执一词。
#[tokio::test]
async fn a_target_that_comes_back_is_no_longer_controlled()
{
    let addr = start_server().await;
    let line = cable(addr).await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (_pc, pc_rx) = spawn_client(line.addr, "pc");
    wait_until_both_online(&phone_rx, "pc").await;
    phone.claim("pc");
    wait_for(&phone_rx, "ControlGranted", |event| {
        matches!(event, Event::ControlGranted { .. })
            .then_some(())
    })
    .await;
    wait_for(&pc_rx, "ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;

    line.cut();

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
    assert_eq!(by, "pc", "撤权该说是被控端自己走的");
}

/// 让 `phone` 经一根线遥控 `pc`,再拔掉 `phone` 的线,等它重连上来。
///
/// `release` 为真时,拔线前先交出持权(界面失联回本机时做的就是这一下)。
/// 返回 `pc` 的事件通道,断言交给调用方:重连之后 `pc` 有没有被重新锁上。
async fn controller_reconnects(
    release: bool,
) -> mpsc::Receiver<Event> {
    let addr = start_server().await;
    let line = cable(addr).await;
    let (phone, phone_rx) =
        spawn_client(line.addr, "phone");
    let (pc, pc_rx) = spawn_client(addr, "pc");
    wait_until_both_online(&phone_rx, "pc").await;
    phone.claim("pc");
    wait_for(&phone_rx, "ControlGranted", |event| {
        matches!(event, Event::ControlGranted { .. })
            .then_some(())
    })
    .await;
    wait_for(&pc_rx, "ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;

    if release {
        phone.release_control();
    }
    line.cut();
    wait_for(&phone_rx, "Disconnected", |event| {
        matches!(event, Event::Disconnected).then_some(())
    })
    .await;
    // 重连上来的凭据:新连接入册后的第一份名册。
    wait_until_both_online(&phone_rx, "pc").await;
    // 客户端与 `pc` 都活到断言结束 —— 丢掉就等于它们下线了。
    std::mem::forget((phone, pc));
    pc_rx
}

/// 对照组:没交出持权的遥控器重连后续上权,被控端重新收到 `ControlledBy`。
///
/// 这一条是下一条的前提 —— 续不上的话,下一条「没被重新锁上」什么也说明不了。
#[tokio::test]
async fn a_held_claim_is_resumed_after_the_controller_reconnects()
 {
    let pc_rx = controller_reconnects(false).await;

    wait_for(&pc_rx, "续权后的 ControlledBy", |event| {
        matches!(event, Event::ControlledBy { .. })
            .then_some(())
    })
    .await;
}

/// 交出持权之后遥控器重连,被控端不再被锁(#118)。
///
/// 界面失联回本机时若不交出,客户端手上那份带代次的记录会在重连时去续权,
/// 而服务端槽位没换人就续上了 —— 被控端又挂起「正被遥控」,遥控器那头却
/// 早已是本机输出,谁也不在遥控它。
#[tokio::test]
async fn a_released_claim_is_not_resumed_after_the_controller_reconnects()
 {
    let pc_rx = controller_reconnects(true).await;

    // 续权是重连后的第一条上行,回环上毫秒级就到;给一秒足够看出来。
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !pc_rx.try_iter().any(|event| matches!(
            event,
            Event::ControlledBy { .. }
        )),
        "交出持权之后重连不该把被控端重新锁上"
    );
}
