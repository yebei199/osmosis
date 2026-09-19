//! 同播的信令端点:`GET /signal` 的 WebSocket 升级。
//!
//! 服务端在同播里只干一件事 —— 把一台设备的 SDP/ICE 转给另一台。**载荷不解析**:
//! 它不是 WebRTC 的参与方,解析等于把上游协议的演化绑到自己身上(`docs/adr/0008`)。
//!
//! 音频不经过这里。主控与听众之间是 P2P,服务端只负责让它们找到彼此。
//!
//! 建连必须带 `Authorization: Bearer <token>`:**账号由服务端从 token 定**,
//! 设备只自报 id 与名字(`DeviceDto`)。归属自报的话,任何人都能把自己塞进
//! 别人的名册里,而那不会报任何错 —— 只是多出一台"在线设备"。
//!
//! 浏览器的 `WebSocket` 构造器设不了请求头,所以 web 端暂时连不上这里。
//! 将来的出路是一次性短期 ticket(登录态换一个只能用一次的查询参数),
//! 不是把这道门放宽成"没有 token 也行"。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{
    Message, WebSocket, WebSocketUpgrade,
};
use axum::response::Response;
use contract::{ClientSignal, DeviceDto, ServerSignal};
use tokio::sync::mpsc;

use crate::account::Account;
use crate::roster::Roster;

/// 每条连接的发件箱容量。
///
/// 信令消息稀疏(建连时几条,之后就没了),这个数只是给慢客户端留的缓冲。
/// 满了就断开它 —— 一条堵住的连接留着只会让名册说谎。
const OUTBOX_CAPACITY: usize = 32;

/// 往一条连接里发消息的出口。
pub type Sink = mpsc::Sender<ServerSignal>;

/// 共享的在线名册。
pub type SharedRoster = Arc<Mutex<Roster<Sink>>>;

/// 账号主键。名册按它分桶,一台设备只看得见同账号的设备。
pub type AccountId = i64;

/// 一条连接的三个时限。
///
/// 做成参数而不是常量:测试要在毫秒级上验超时与探活,而生产要的是秒级 ——
/// 写死的话这几条要么跑不动,要么每次 `cargo test` 都付上几十秒墙钟。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 鉴权通过之后,等 `Hello` 的上限。不自报家门的连接白占着一个连接槽。
    pub hello: Duration,
    /// 多久主动 Ping 一次对端。
    pub ping_every: Duration,
    /// 连续几次 Ping 没有回音就判死。
    pub misses: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(10),
            // 30 秒一次、容忍两次 —— 死连接最迟一分钟出册。再短的话
            // 移动网络上一次正常的短暂卡顿就会把人踢下线。
            ping_every: Duration::from_secs(30),
            misses: 2,
        }
    }
}

/// `GET /signal` —— 鉴权,然后升级成 WebSocket。
///
/// `Account` 是提取器:没有它这条路由就不鉴权,而"要不要鉴权"写在签名里
/// 正是 [`crate::auth`] 那套做法的用意。未鉴权连接在升级之前就得到 401。
pub async fn handler(
    upgrade: WebSocketUpgrade,
    account: Account,
    State(roster): State<SharedRoster>,
) -> Response {
    let timing = Timing::default();
    let account_id = account.id;
    upgrade.on_upgrade(move |socket| {
        serve(socket, roster, account_id, timing)
    })
}

/// 一条连接的一生:等 Hello → 入册 → 转发信令 → 断开时出册。
///
/// 账号由调用方给定,不从连接里读 —— 它是鉴权的产物。
///
/// 收与发拆成两半跑:发件端要能在**没有任何来信**时主动推名册(别的设备上下线),
/// 只在收信循环里顺带发的话,一台安静的设备永远收不到名册更新。
pub async fn serve(
    socket: WebSocket,
    roster: SharedRoster,
    account: AccountId,
    timing: Timing,
) {
    use futures_util::{SinkExt, StreamExt};

    let (mut ws_tx, mut ws_rx) = socket.split();
    let (sink, mut outbox) = mpsc::channel(OUTBOX_CAPACITY);

    // 发件端:把收件箱里的消息序列化后写进连接。
    let pump = tokio::spawn(async move {
        while let Some(message) = outbox.recv().await {
            let Ok(text) = serde_json::to_string(&message)
            else {
                continue;
            };
            if ws_tx
                .send(Message::text(text))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // 第一句必须是 Hello。不自报家门就不入册,也就不出现在任何人的名册里。
    // 超时是必要的:鉴权只保证对端有账号,不保证它还打算说话。
    let Ok(Some(device)) = tokio::time::timeout(
        timing.hello,
        accept_hello(&mut ws_rx),
    )
    .await
    else {
        pump.abort();
        return;
    };
    let device_id = device.id.clone();
    tracing::debug!(
        account,
        device = %device_id,
        "设备入册"
    );

    {
        let mut guard = roster.lock().expect("名册锁中毒");
        // 旧连接的出口交还给我们:丢掉它,那条僵死连接的 pump 会随之退出。
        drop(guard.join(device, sink));
        broadcast_roster(&guard);
    }

    while let Some(Ok(message)) = ws_rx.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(parsed) =
            serde_json::from_str::<ClientSignal>(&text)
        else {
            continue;
        };

        let guard = roster.lock().expect("名册锁中毒");
        if let Some(reply) =
            route(&guard, &device_id, parsed)
            && let Some(own) = guard.sink(&device_id)
        {
            // try_send 而非 send:这里握着锁,await 会把整个名册卡住。
            // 发件箱满说明这台设备已经读不动了,丢掉这条应答不比卡住所有人差。
            let _ = own.try_send(reply);
        }
    }

    let mut guard = roster.lock().expect("名册锁中毒");
    guard.leave(&device_id);
    broadcast_roster(&guard);
    drop(guard);
    pump.abort();
}

/// 不鉴权的信令路由,**只给测试用**:账号由查询参数 `?account=<id>` 给,缺省 1。
///
/// 生产路由是 [`handler`],那条路的账号只能来自 token。分桶、代次、探活这些
/// 与"账号从哪来"无关的行为,因此不必为了起一个数据库而变成集成测试。
#[doc(hidden)]
pub fn unauthenticated_test_router(
    timing: Timing,
) -> axum::Router {
    use axum::extract::Query;
    use axum::routing::get;

    async fn upgrade(
        upgrade: WebSocketUpgrade,
        Query(query): Query<TestQuery>,
        State((roster, timing)): State<(
            SharedRoster,
            Timing,
        )>,
    ) -> Response {
        let account = query.account.unwrap_or(1);
        upgrade.on_upgrade(move |socket| {
            serve(socket, roster, account, timing)
        })
    }

    axum::Router::new()
        .route("/signal", get(upgrade))
        .with_state((SharedRoster::default(), timing))
}

/// [`unauthenticated_test_router`] 的查询参数。
#[derive(serde::Deserialize)]
#[doc(hidden)]
pub struct TestQuery {
    account: Option<AccountId>,
}

/// 读到第一条 `Hello` 为止。连接先断或格式不对就放弃这条连接。
async fn accept_hello(
    ws_rx: &mut futures_util::stream::SplitStream<
        WebSocket,
    >,
) -> Option<DeviceDto> {
    use futures_util::StreamExt;

    while let Some(Ok(message)) = ws_rx.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        if let Ok(ClientSignal::Hello { device }) =
            serde_json::from_str::<ClientSignal>(&text)
        {
            return Some(device);
        }
    }
    None
}

/// 把当前名册推给所有在线设备。
///
/// 每次名册变化都推,不让客户端轮询:一台设备下线到别人发现之间的空窗期里,
/// 推流必然失败,而失败原因看起来会像是 WebRTC 出了问题。
fn broadcast_roster(roster: &Roster<Sink>) {
    let message = ServerSignal::Roster {
        devices: roster.devices(),
    };
    for sink in roster.sinks() {
        // 发不进去的连接已经死了,它下一次读失败时会自己出册。
        let _ = sink.try_send(message.clone());
    }
}

/// 处理一条来自设备的消息。返回要发回给它自己的应答(没有则 `None`)。
fn route(
    roster: &Roster<Sink>,
    from: &str,
    message: ClientSignal,
) -> Option<ServerSignal> {
    match message {
        // 已经入册的连接再发 Hello 没有意义,忽略。
        ClientSignal::Hello { .. } => None,
        ClientSignal::Signal { to, payload } => {
            let Some(target) = roster.sink(&to) else {
                return Some(ServerSignal::Error {
                    code: "device_offline".to_owned(),
                    message: format!("设备 {to} 不在线"),
                });
            };
            // payload 原样转发 —— 不解析、不规范化、不裁剪空白。
            let forwarded = ServerSignal::Signal {
                from: from.to_owned(),
                payload,
            };
            match target.try_send(forwarded) {
                Ok(()) => None,
                Err(_) => Some(ServerSignal::Error {
                    code: "device_unreachable".to_owned(),
                    message: format!(
                        "设备 {to} 收不下消息"
                    ),
                }),
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn device(id: &str) -> DeviceDto {
        DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        }
    }

    /// 建一个装了两台设备的名册,并交出它们的收件端。
    fn two_devices() -> (
        Roster<Sink>,
        mpsc::Receiver<ServerSignal>,
        mpsc::Receiver<ServerSignal>,
    ) {
        let (sink_a, rx_a) = mpsc::channel(OUTBOX_CAPACITY);
        let (sink_b, rx_b) = mpsc::channel(OUTBOX_CAPACITY);
        let mut roster = Roster::default();
        roster.join(device("a"), sink_a);
        roster.join(device("b"), sink_b);
        (roster, rx_a, rx_b)
    }

    /// 信令进了目标的收件箱,发信人自己的收件箱是空的。
    #[test]
    fn signal_reaches_only_the_target() {
        let (roster, mut rx_a, mut rx_b) = two_devices();

        let reply = route(
            &roster,
            "a",
            ClientSignal::Signal {
                to: "b".to_owned(),
                payload: "v=0...".to_owned(),
            },
        );

        assert!(reply.is_none(), "转发成功时不该有应答");
        assert_eq!(
            rx_b.try_recv(),
            Ok(ServerSignal::Signal {
                from: "a".to_owned(),
                payload: "v=0...".to_owned(),
            })
        );
        assert!(
            rx_a.try_recv().is_err(),
            "发信人不该收到自己的信令"
        );
    }

    /// 载荷一个字节都不许改 —— 服务端不解析它,也就没有理由规范化它。
    #[test]
    fn payload_crosses_unmodified() {
        let (roster, _rx_a, mut rx_b) = two_devices();
        // 一段带换行、带非 ASCII、带前后空白的载荷:任何"顺手清理"都会露馅。
        let payload = "  v=0\r\na=ice-ufrag:红蓮\r\n\r\n  "
            .to_owned();

        route(
            &roster,
            "a",
            ClientSignal::Signal {
                to: "b".to_owned(),
                payload: payload.clone(),
            },
        );

        assert_eq!(
            rx_b.try_recv(),
            Ok(ServerSignal::Signal {
                from: "a".to_owned(),
                payload,
            })
        );
    }

    /// 目标不在线要回错误。
    ///
    /// 静默丢弃的话主控发完 offer 就一直等应答,界面上表现为"卡住",
    /// 而真实原因是对方早就下线了。
    #[test]
    fn signal_to_offline_device_reports_error() {
        let (roster, _rx_a, _rx_b) = two_devices();

        let reply = route(
            &roster,
            "a",
            ClientSignal::Signal {
                to: "不在线".to_owned(),
                payload: "v=0...".to_owned(),
            },
        );

        assert!(
            matches!(
                reply,
                Some(ServerSignal::Error { ref code, .. })
                    if code == "device_offline"
            ),
            "实得 {reply:?}"
        );
    }

    /// 名册变化推给每一台在线设备,而不是只推给变化的那台。
    #[test]
    fn roster_is_pushed_to_everyone() {
        let (roster, mut rx_a, mut rx_b) = two_devices();

        broadcast_roster(&roster);

        let expected = ServerSignal::Roster {
            devices: vec![device("a"), device("b")],
        };
        assert_eq!(rx_a.try_recv(), Ok(expected.clone()));
        assert_eq!(rx_b.try_recv(), Ok(expected));
    }
}
