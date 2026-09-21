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
use axum::http::HeaderMap;
use axum::http::header::ORIGIN;
use axum::response::{IntoResponse, Response};
use contract::{ClientSignal, DeviceDto, ServerSignal};
use tokio::sync::mpsc;

use crate::error;
use crate::store::account::Account;
use crate::syncplay::control::Control;
use crate::syncplay::roster::Roster;

/// 每条连接的发件箱容量。
///
/// 信令消息稀疏(建连时几条,之后就没了),这个数只是给慢客户端留的缓冲。
/// 满了就断开它 —— 一条堵住的连接留着只会让名册说谎。
const OUTBOX_CAPACITY: usize = 32;

/// 往一条连接里发消息的出口。
pub type Sink = mpsc::Sender<ServerSignal>;

/// 共享的在线名册。
pub type SharedRoster = Arc<Mutex<Roster<Sink>>>;

/// 共享的遥控控制权槽位。
///
/// 与名册分两把锁,并且**永远先锁名册再锁它** —— 唯一同时用上两者的地方是
/// [`dispatch`],锁序在那一处定死,别处不许再取第二种顺序。
pub type SharedControl = Arc<Mutex<Control>>;

/// 账号主键。名册按它分桶,一台设备只看得见同账号的设备。
pub type AccountId = i64;

/// 允许的浏览器来源。原生端不带 `Origin`,这张表只约束浏览器。
pub type AllowedOrigins = Arc<Vec<String>>;

/// 单条消息的上限。
///
/// 真源在契约里(`contract::MAX_SIGNAL_BYTES`):发送端要在**发之前**照同一个数
/// 拦住自己,而超限在这一侧的后果是整条连接断掉,不是丢一条消息(见那里的说明)。
/// 两边各写一个字面量的话,客户端那道自检迟早与这里对不上。
const MAX_MESSAGE_BYTES: usize = contract::MAX_SIGNAL_BYTES;

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

/// `GET /signal` —— 鉴权、校验来源、限流,然后升级成 WebSocket。
///
/// `Account` 是提取器:没有它这条路由就不鉴权,而"要不要鉴权"写在签名里
/// 正是 [`crate::gate::auth`] 那套做法的用意。未鉴权连接在升级之前就得到 401。
pub async fn handler(
    upgrade: WebSocketUpgrade,
    account: Account,
    headers: HeaderMap,
    State(roster): State<SharedRoster>,
    State(control): State<SharedControl>,
    State(origins): State<AllowedOrigins>,
) -> Response {
    // 浏览器一定带 `Origin`,原生端不带。带了就必须在白名单里 ——
    // 同源策略管不到 WebSocket,不校验的话任意网页都能借用户的登录态连上来。
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin.to_str().is_ok_and(|origin| {
            origins.iter().any(|allowed| allowed == origin)
        });
        if !allowed {
            return error::forbidden("来源不在白名单里")
                .into_response();
        }
    }

    // 建连限流不在这里了:它挂在路由上(`gate::ratelimit` 的
    // `signal_connect`)。**层只拦升级请求本身**,拦不到升级之后那条
    // WebSocket 上的消息 —— 那一侧的上限仍是 `MAX_MESSAGE_BYTES`,
    // 两道闸各管各的。
    let account_id = account.id;

    upgrade.max_message_size(MAX_MESSAGE_BYTES).on_upgrade(
        move |socket| {
            serve(
                socket,
                roster,
                control,
                account_id,
                Timing::default(),
            )
        },
    )
}

/// 一条连接的一生:等 Hello → 入册 → 转发信令与探活 → 断开时出册。
///
/// 账号由调用方给定,不从连接里读 —— 它是鉴权的产物。
///
/// 收、发、探活在**同一个循环**里轮转。发件端要能在没有任何来信时主动推名册
/// (别的设备上下线),所以不能写成"读到一条才发一条";而 `select!` 一次挑一件
/// 事做,三者各不相误,也不必为发件端另起一个任务再想办法叫停它。
pub async fn serve(
    socket: WebSocket,
    roster: SharedRoster,
    control: SharedControl,
    account: AccountId,
    timing: Timing,
) {
    use futures_util::{SinkExt, StreamExt};

    let (mut ws_tx, mut ws_rx) = socket.split();
    let (sink, mut outbox) = mpsc::channel(OUTBOX_CAPACITY);

    // 第一句必须是 Hello。不自报家门就不入册,也就不出现在任何人的名册里。
    // 超时是必要的:鉴权只保证对端有账号,不保证它还打算说话。
    let Ok(Some((device, spoken))) = tokio::time::timeout(
        timing.hello,
        accept_hello(&mut ws_rx),
    )
    .await
    else {
        return;
    };
    let device_id = device.id.clone();

    // 版本协商,**在入册之前**。入册是取得控制权的前提,所以讲不同协议的
    // 那一端在能遥控任何设备之前就被挡住了(`docs/adr/0031`)。
    //
    // 应答无论对错都发:对得上时它是新客户端确认「对端也是新的」的唯一凭据
    // (旧服务端永远不发这条,客户端据此认出它)。对不上就发完即关,
    // 不入册、不进任何人的名册。
    let welcome = ServerSignal::Welcome {
        protocol_version: contract::PROTOCOL_VERSION,
    };
    if let Ok(text) = serde_json::to_string(&welcome)
        && ws_tx.send(Message::text(text)).await.is_err()
    {
        return;
    }
    if spoken != contract::PROTOCOL_VERSION {
        tracing::info!(
            account,
            device = %device_id,
            spoken,
            ours = contract::PROTOCOL_VERSION,
            "协议版本对不上,拒绝入册"
        );
        // 好好地关,不要一走了之:直接 return 会把 socket 丢掉,对端收到的是
        // 一个没有关闭握手的 reset —— 而那与「网线被拔了」长得一模一样,
        // 正是这道协商要分开的两件事。带上原因码,日志里也看得出来。
        let _ = ws_tx
            .send(Message::Close(Some(
                axum::extract::ws::CloseFrame {
                    code: axum::extract::ws::close_code::POLICY,
                    reason: "protocol version mismatch"
                        .into(),
                },
            )))
            .await;
        return;
    }

    let generation = {
        let mut guard = roster.lock().expect("名册锁中毒");
        // 旧连接的出口交还给我们:丢掉它,那条连接的循环下一轮就收到通道已关,
        // 自己收工 —— 不必从外面去掐它。
        let (generation, stale) =
            guard.join(account, device, sink);
        drop(stale);
        broadcast_roster(&guard, account);
        generation
    };
    tracing::debug!(
        account,
        device = %device_id,
        generation,
        "设备入册"
    );

    // 探活:每 `ping_every` 发一次 Ping,连着 `misses` 次没有任何回音就判死。
    // 没有它的话,一条被路由器悄悄丢掉的连接要等 TCP 自己发现 —— 那是十几分钟,
    // 而这段时间里名册一直说这台设备在线,谁往它推流谁卡住。
    let mut ping = tokio::time::interval(timing.ping_every);
    // interval 的第一次 tick 立刻就绪,先把它吃掉,免得刚连上就发一次 Ping。
    ping.tick().await;
    let mut unanswered = 0;

    loop {
        tokio::select! {
            outgoing = outbox.recv() => {
                // 通道关了:本条连接已被同 id 的新连接顶替。
                let Some(message) = outgoing else { break };
                let Ok(text) = serde_json::to_string(&message) else {
                    continue;
                };
                if ws_tx.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            incoming = ws_rx.next() => {
                let Some(Ok(message)) = incoming else { break };
                // 任何一帧都算活着 —— Pong 只是其中最常见的那种。
                unanswered = 0;
                if let Message::Text(text) = message {
                    dispatch(&roster, &control, account, &device_id, &text);
                }
            }
            _ = ping.tick() => {
                if unanswered >= timing.misses {
                    break;
                }
                unanswered += 1;
                if ws_tx
                    .send(Message::Ping(axum::body::Bytes::new()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }

    let mut guard = roster.lock().expect("名册锁中毒");
    // 被顶替掉的那条连接的清理什么都不该动:它的 leave 返回 false,
    // 而顺手清掉控制权会把刚重连上的那条遥控关系带走。
    if guard.leave(account, &device_id, generation) {
        // 下线的若是**被控端**,它身上的遥控关系没了,遥控器得知道。
        // 下线的若是遥控器,槽位原样留着 —— 手机没电不能让 pc1 停。
        let freed = control
            .lock()
            .expect("控制权锁中毒")
            .release(account, &device_id);
        if let Some(controller) = freed
            && let Some(sink) =
                guard.sink(account, &controller)
        {
            let _ = sink.try_send(
                ServerSignal::ControlRevoked {
                    by: device_id.clone(),
                },
            );
        }
    }
    broadcast_roster(&guard, account);
}

/// 处理一条文本帧:解析、路由,应答塞回发信人自己的收件箱。
///
/// 单独一个**同步**函数,是为了让名册的锁不可能被握过一个 await 点 ——
/// 握着它 await 会把所有人的名册一起卡住,而那种卡是偶发且难查的。
fn dispatch(
    roster: &SharedRoster,
    control: &SharedControl,
    account: AccountId,
    device_id: &str,
    text: &str,
) {
    let Ok(parsed) =
        serde_json::from_str::<ClientSignal>(text)
    else {
        return;
    };

    let guard = roster.lock().expect("名册锁中毒");
    let mut control = control.lock().expect("控制权锁中毒");
    if let Some(reply) = route(
        &guard,
        &mut control,
        account,
        device_id,
        parsed,
    ) && let Some(own) = guard.sink(account, device_id)
    {
        // try_send 而非 send:发件箱满说明这台设备已经读不动了,
        // 丢掉这条应答不比卡住所有人差。
        let _ = own.try_send(reply);
    }
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
        State((roster, control, timing)): State<(
            SharedRoster,
            SharedControl,
            Timing,
        )>,
    ) -> Response {
        let account = query.account.unwrap_or(1);
        upgrade
            .max_message_size(MAX_MESSAGE_BYTES)
            .on_upgrade(move |socket| {
                serve(
                    socket, roster, control, account,
                    timing,
                )
            })
    }

    axum::Router::new()
        .route("/signal", get(upgrade))
        .with_state((
            SharedRoster::default(),
            SharedControl::default(),
            timing,
        ))
}

/// [`unauthenticated_test_router`] 的查询参数。
#[derive(serde::Deserialize)]
#[doc(hidden)]
pub struct TestQuery {
    account: Option<AccountId>,
}

/// 读到第一条 `Hello` 为止,连同它自报的协议版本。
///
/// 连接先断或格式不对就放弃这条连接。旧客户端的 `Hello` 里没有版本字段,
/// 那一侧由契约里的 `#[serde(default)]` 兜成 0 —— 解不出来的话这里只会静默
/// 丢掉它,而连接会挂在超时上,「版本不对」就此与「网络不好」长得一样。
async fn accept_hello(
    ws_rx: &mut futures_util::stream::SplitStream<
        WebSocket,
    >,
) -> Option<(DeviceDto, u32)> {
    use futures_util::StreamExt;

    while let Some(Ok(message)) = ws_rx.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        if let Ok(ClientSignal::Hello {
            device,
            protocol_version,
        }) = serde_json::from_str::<ClientSignal>(&text)
        {
            return Some((device, protocol_version));
        }
    }
    None
}

/// 把某个账号的名册推给它自己的全部在线设备。
///
/// 每次名册变化都推,不让客户端轮询:一台设备下线到别人发现之间的空窗期里,
/// 推流必然失败,而失败原因看起来会像是 WebRTC 出了问题。
fn broadcast_roster(
    roster: &Roster<Sink>,
    account: AccountId,
) {
    let message = ServerSignal::Roster {
        devices: roster.devices(account),
    };
    for sink in roster.sinks(account) {
        // 发不进去的连接已经死了,它下一次读失败时会自己出册。
        let _ = sink.try_send(message.clone());
    }
}

/// 处理一条来自设备的消息。返回要发回给它自己的应答(没有则 `None`)。
///
/// 目标只在**同一个账号**的桶里找:找不到就是"不在线",与真的不在线一个说法。
/// 分两种说法的话,等于告诉发信人"这个 id 存在,只是不归你" —— 那是一条
/// 白送的枚举信道。
fn route(
    roster: &Roster<Sink>,
    control: &mut Control,
    account: AccountId,
    from: &str,
    message: ClientSignal,
) -> Option<ServerSignal> {
    match message {
        // 已经入册的连接再发 Hello 没有意义,忽略。
        ClientSignal::Hello { .. } => None,
        // 遥控器模式那几条归 `crate::syncplay::control`:这里只管同播的转发。
        remote @ (ClientSignal::ClaimControl { .. }
        | ClientSignal::ExitControlled
        | ClientSignal::Command { .. }
        | ClientSignal::State { .. }
        | ClientSignal::SnapshotRequest {
            ..
        }) => crate::syncplay::control::route(
            roster, control, account, from, remote,
        ),
        ClientSignal::Signal { to, payload } => {
            let Some(target) = roster.sink(account, &to)
            else {
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

    /// 两个账号,用来验"信令转不出自己那一桶"。
    const ALICE: AccountId = 1;
    const BOB: AccountId = 2;

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
        roster.join(ALICE, device("a"), sink_a);
        roster.join(ALICE, device("b"), sink_b);
        (roster, rx_a, rx_b)
    }

    /// 信令进了目标的收件箱,发信人自己的收件箱是空的。
    #[test]
    fn signal_reaches_only_the_target() {
        let (roster, mut rx_a, mut rx_b) = two_devices();

        let reply = route(
            &roster,
            &mut Control::default(),
            ALICE,
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
            &mut Control::default(),
            ALICE,
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
            &mut Control::default(),
            ALICE,
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

    /// 发给别人账号下的设备,一律当作不在线 —— 而且真的没送过去。
    #[test]
    fn signal_across_accounts_is_refused() {
        let (mut roster, _rx_a, _rx_b) = two_devices();
        let (sink_c, mut rx_c) =
            mpsc::channel(OUTBOX_CAPACITY);
        roster.join(BOB, device("c"), sink_c);

        let reply = route(
            &roster,
            &mut Control::default(),
            ALICE,
            "a",
            ClientSignal::Signal {
                to: "c".to_owned(),
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
        assert!(
            rx_c.try_recv().is_err(),
            "消息不该落到别人账号的设备上"
        );
    }

    /// 名册变化推给每一台在线设备,而不是只推给变化的那台。
    #[test]
    fn roster_is_pushed_to_everyone() {
        let (roster, mut rx_a, mut rx_b) = two_devices();

        broadcast_roster(&roster, ALICE);

        let expected = ServerSignal::Roster {
            devices: vec![device("a"), device("b")],
        };
        assert_eq!(rx_a.try_recv(), Ok(expected.clone()));
        assert_eq!(rx_b.try_recv(), Ok(expected));
    }

    /// 广播不外溢到别的账号。
    ///
    /// 名册里有谁是设备名,而设备名多半是主机名 —— 那是别人机器的名字。
    #[test]
    fn broadcast_does_not_leak_to_other_accounts() {
        let (mut roster, _rx_a, _rx_b) = two_devices();
        let (sink_c, mut rx_c) =
            mpsc::channel(OUTBOX_CAPACITY);
        roster.join(BOB, device("c"), sink_c);

        broadcast_roster(&roster, ALICE);

        assert!(
            rx_c.try_recv().is_err(),
            "别人账号的设备不该收到这份名册"
        );
    }
}
