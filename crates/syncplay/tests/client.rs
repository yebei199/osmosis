//! [`Client`] 这一层的验证:连上之后名册到得了,服务端出毛病时不被它拖着空转。
//!
//! 与 `signalling.rs` 的分工:那边直接用 `Signalling` 证明**零件**能用;这边只调
//! `Client::start`,证明**编排循环**是对的。遥控那几条在 `remote.rs`。

use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use server::syncplay::signaling;
use syncplay::{Client, DeviceDto, Event};

/// 等一件事发生的上界。回环上入册是毫秒级,给足余量。
const PATIENCE: Duration = Duration::from_secs(20);

/// 测试路由不鉴权,但 token 仍要是个合法的头值 —— 请求头里放不下的字符,
/// 连接在发起之前就被本地拒了。
const TOKEN: &str = "test-token";

/// 起一个只有信令路由的服务端,端口交给系统分配。
async fn start_signalling_server() -> SocketAddr {
    // 不鉴权的测试路由:本文件验的是编排循环,不是鉴权,起一个真数据库
    // 只为了造一个 token 是本末倒置。鉴权本身在 server/tests/live_signaling.rs。
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

/// 起一个客户端,并把它抛出的事件收进一条通道。
///
/// 事件回调在后台线程上跑,而断言在测试线程上 —— 通道是两者之间唯一的桥。
fn start_client(
    addr: SocketAddr,
    id: &str,
) -> (Client, mpsc::Receiver<Event>) {
    let (events, received) = mpsc::channel();
    let client = Client::start(
        &format!("ws://{addr}"),
        device(id),
        || Some(TOKEN.to_owned()),
        move |event| {
            let _ = events.send(event);
        },
    );
    (client, received)
}

/// 等一条满足条件的事件,超时就失败。
///
/// 事件是流水:名册会推很多次,中间还夹着别的。逐条查而不是只看第一条。
fn wait_for<T>(
    events: &mpsc::Receiver<Event>,
    what: &str,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> T {
    let deadline = Instant::now() + PATIENCE;
    while let Some(left) =
        deadline.checked_duration_since(Instant::now())
    {
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if let Event::Failed(message) = &event {
            panic!("等 {what} 时出错: {message}");
        }
        if let Some(found) = pick(event) {
            return found;
        }
    }
    panic!("等 {what} 超时");
}

/// 从事件里挑出"这些设备都在名册里了"。
///
/// 收**一组** id 而不是一个:名册是整份推的,三台设备几乎同时连上时,
/// 服务端可能只推一条含全部三台的名册。逐台分开等的话,第一次等就把那条唯一的
/// 名册消费掉了,第二次于是永远等不到 —— 而三台设备其实早就都在线。
fn roster_has(
    ids: &[&str],
) -> impl FnMut(Event) -> Option<()> {
    let ids: Vec<String> =
        ids.iter().map(|id| (*id).to_owned()).collect();
    move |event| match event {
        Event::Roster(devices)
            if ids.iter().all(|id| {
                devices.iter().any(|d| &d.id == id)
            }) =>
        {
            Some(())
        }
        _ => None,
    }
}

/// 两台设备各自连上,彼此都能在名册事件里看到对方。
///
/// 这是界面上输出设备那一列的唯一来源:名册推不到,列表就永远是空的,
/// 而连接本身看起来一切正常。
#[tokio::test(flavor = "multi_thread")]
async fn clients_see_each_other_in_the_roster() {
    let addr = start_signalling_server().await;

    let (_phone, phone_events) =
        start_client(addr, "phone");
    let (_pc, pc_events) = start_client(addr, "pc");

    wait_for(
        &phone_events,
        "手机看到 pc",
        roster_has(&["pc"]),
    );
    wait_for(
        &pc_events,
        "pc 看到手机",
        roster_has(&["phone"]),
    );
}

// ---------------------------------------------------------------------------
// 重连的节奏(#109 F-R3)
//
// 服务端那道 `signal_connect` 限流按**账号**分桶,而账号底下可能有好几台
// 设备。所以一台设备烧额度的速度不是它自己的事:烧干了,同账号另一台
// 设备连不上,报出来的却是那一台的 429。
//
// 「连上过」这件事不能只看 `connect()` 返回 Ok —— 服务端接完就关时那一步
// 照样成功。把退避按这个判据清零,循环就以网络往返的速度空转,而**客户端
// 日志里一行都不会有**:成功建连不打日志,`serve` 返回也不打日志。
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::sync::atomic::{
    AtomicUsize, Ordering as AtomicOrdering,
};

/// 一个「接完就关」的信令服务端,顺便数一数它被连了多少次。
///
/// 真实世界里有好几条路会走到这个形状:同 id 的第二个实例把前一个顶掉、
/// 版本协商拒掉一个旧端、消息超过 `MAX_SIGNAL_BYTES` 让读循环跳出、
/// 滚动发布时 pod 换人。它们的共同点是**连接建得起来,随即就没了**。
async fn start_slamming_door()
-> (SocketAddr, Arc<AtomicUsize>) {
    use axum::extract::WebSocketUpgrade;
    use axum::response::Response;
    use axum::routing::get;

    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);

    let app = axum::Router::new().route(
        "/signal",
        get(move |upgrade: WebSocketUpgrade| {
            let counter = Arc::clone(&counter);
            async move {
                counter
                    .fetch_add(1, AtomicOrdering::Relaxed);
                let response: Response = upgrade
                    .on_upgrade(|socket| async move {
                        drop(socket);
                    });
                response
            }
        }),
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
    (addr, hits)
}

/// 服务端接完就关时,客户端**不许**拿额度当柴烧。
///
/// 服务端那个桶装得下 30 个、每两秒恢复一个。五秒里连上十几次就已经把
/// 一个正常账号的建连额度吃掉一半,而这种空转可以持续几分钟 —— 期间
/// 同账号的另一台设备只会看到 429,并且在两边的日志里都找不到原因。
///
/// 上界给 6:退避从一秒起翻倍(1、2、4…),五秒里最多是第 0、1、3 这几拍,
/// 加上 ±25% 的抖动也超不过这个数。
#[tokio::test]
async fn a_door_slamming_server_is_not_hammered() {
    let (addr, hits) = start_slamming_door().await;

    let (_client, _events) = start_client(addr, "slammed");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let count = hits.load(AtomicOrdering::Relaxed);
    assert!(
        count <= 6,
        "五秒里建了 {count} 次连接 —— 服务端那个桶只有 30 个额度,\
         每两秒恢复一个,这个速度几秒就能把同账号所有设备锁在门外"
    );
}
