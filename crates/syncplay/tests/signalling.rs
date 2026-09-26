//! 信令这一层对着真服务端:入册、版本协商、静默判死。
//!
//! 服务端起在本测试进程里,不需要外部进程,也不需要第二台机器。

use std::net::SocketAddr;
use std::time::Duration;

use contract::{DeviceDto, ServerSignal};
use server::syncplay::signaling;
use syncplay::Signalling;

/// 测试路由不鉴权,但 token 仍要是个合法的头值 —— 请求头里放不下的字符,
/// 连接在发起之前就被本地拒了。
const TOKEN: &str = "test-token";

/// 起一个只有信令路由的服务端,端口交给系统分配。
async fn start_signalling_server() -> SocketAddr {
    start_signalling_server_with(
        signaling::Timing::default(),
    )
    .await
}

/// 同上,但探活的时限由调用方给 —— 判死那条测试要一个不 Ping 的服务端。
async fn start_signalling_server_with(
    timing: signaling::Timing,
) -> SocketAddr {
    // 不鉴权的测试路由:本文件验的是信令链路,不是鉴权,起一个真数据库
    // 只为了造一个 token 是本末倒置。鉴权本身在 server/tests/live_signaling.rs。
    let app =
        signaling::unauthenticated_test_router(timing);

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

/// 连上信令服务器,并把设备放进名册。
#[tokio::test]
async fn hello_puts_device_in_roster() {
    let addr = start_signalling_server().await;

    let mut host = Signalling::connect(
        &format!("ws://{addr}"),
        device("host"),
        TOKEN,
    )
    .await
    .expect("连不上信令服务器");

    // 握手应答排在入册之前(`docs/adr/0031`),名册在它后面。
    let welcome = tokio::time::timeout(
        Duration::from_secs(5),
        host.next(),
    )
    .await
    .expect("等握手应答超时")
    .expect("连接已关闭");
    assert!(
        matches!(
            welcome,
            ServerSignal::Welcome { protocol_version }
                if protocol_version == contract::PROTOCOL_VERSION
        ),
        "第一条该是 Welcome,实得 {welcome:?}"
    );

    let first = tokio::time::timeout(
        Duration::from_secs(5),
        host.next(),
    )
    .await
    .expect("等名册超时")
    .expect("连接已关闭");

    assert!(
        matches!(first, ServerSignal::Roster { ref devices }
            if devices.iter().any(|d| d.id == "host")),
        "自报家门后应出现在名册里,实得 {first:?}"
    );
}

/// 讲旧协议的那一端**入不了册**,而且收到的是一次好好的关闭(#109 AC-7)。
///
/// 这一组是 AC-7 里「旧客户端 + 新服务端」那一格。三件事都要成立,少一件
/// 这道协商就白做:
///
/// 1. 先收到 `Welcome`,里面是服务端的版本 —— 少了它,旧端只知道自己被关了,
///    说不出该升到哪一版;
/// 2. 紧接着是一帧带 POLICY 原因的 Close,不是把 socket 一丢。丢掉的话对端
///    收到的是 reset,而那与「网线被拔了」长得一模一样;
/// 3. 它**不在任何人的名册里**。入册是取得控制权的前提,所以挡在这里等于
///    挡在它能遥控任何设备之前。
///
/// 用裸 WebSocket 而不是 `Signalling::connect`:后者永远报本机编译进去的
/// 那个版本号,演不出一个旧端。
#[tokio::test]
async fn an_old_client_is_refused_before_it_joins_the_roster()
 {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let addr = start_signalling_server().await;

    // 先让一台**正常**的设备连上,它的名册就是判据。
    let mut modern = Signalling::connect(
        &format!("ws://{addr}"),
        device("modern"),
        TOKEN,
    )
    .await
    .expect("连不上信令服务器");
    let _welcome = tokio::time::timeout(
        Duration::from_secs(5),
        modern.next(),
    )
    .await
    .expect("等握手应答超时")
    .expect("连接已关闭");

    // 再来一个讲上一版协议的。
    let (mut old, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal"),
    )
    .await
    .expect("裸 WebSocket 连不上");
    let hello = serde_json::to_string(
        &contract::ClientSignal::Hello {
            device: device("ancient"),
            protocol_version: contract::PROTOCOL_VERSION
                - 1,
        },
    )
    .expect("Hello 序列化失败");
    old.send(WsMessage::text(hello))
        .await
        .expect("发不出 Hello");

    let first = tokio::time::timeout(
        Duration::from_secs(5),
        old.next(),
    )
    .await
    .expect("等应答超时")
    .expect("连接已关闭")
    .expect("读出错");
    let WsMessage::Text(text) = first else {
        panic!("第一条该是文本的 Welcome,实得 {first:?}");
    };
    let welcome: ServerSignal = serde_json::from_str(&text)
        .expect("解不开 Welcome");
    assert!(
        matches!(
            welcome,
            ServerSignal::Welcome { protocol_version }
                if protocol_version == contract::PROTOCOL_VERSION
        ),
        "旧端也该先拿到服务端的版本号,实得 {welcome:?}"
    );

    let second = tokio::time::timeout(
        Duration::from_secs(5),
        old.next(),
    )
    .await
    .expect("等关闭帧超时")
    .expect("连接已关闭")
    .expect("读出错");
    assert!(
        matches!(
            &second,
            WsMessage::Close(Some(frame))
                if frame.reason.contains("protocol version mismatch")
        ),
        "该是一帧带原因的 Close,实得 {second:?}"
    );

    // 正常那台设备的名册里,自始至终只有它自己。
    let roster = tokio::time::timeout(
        Duration::from_secs(5),
        modern.next(),
    )
    .await
    .expect("等名册超时")
    .expect("连接已关闭");
    let ServerSignal::Roster { devices } = roster else {
        panic!("该是名册,实得 {roster:?}");
    };
    assert!(
        devices.iter().all(|seen| seen.id != "ancient"),
        "讲旧协议的那台不该出现在名册里: {devices:?}"
    );
}

/// 一帧都不来时,客户端自己判死这条连接 —— 不等 TCP 发现(#102 F-005)。
///
/// 移动网络上半开的 socket 不会有 FIN,`ws_rx.next()` 于是永远等下去:重连不
/// 触发,重连时那条 resume claim 的自愈也就走不到,遥控器永久停在一个早就没了
/// 的会话上。这里让服务端干脆不 Ping,再把判死的时限压到几百毫秒 —— 75 秒的
/// 判断不能靠真的等 75 秒。
#[tokio::test]
async fn a_silent_connection_is_declared_dead() {
    use std::time::Duration;

    // Ping 间隔调到远大于本条测试的寿命 = 服务端一帧都不会主动发。
    let addr =
        start_signalling_server_with(signaling::Timing {
            hello: Duration::from_secs(10),
            ping_every: Duration::from_secs(3_600),
            misses: 2,
        })
        .await;

    let mut signalling = Signalling::connect_with_idle(
        &format!("ws://{addr}"),
        device("lonely"),
        TOKEN,
        Duration::from_millis(300),
    )
    .await
    .expect("该连得上");

    // 握手应答与入册后的那条名册都是正常流量,先读掉 —— 判死说的是
    // 「此后一帧都没有」。
    let _ = signalling.next().await;
    let _ = signalling.next().await;

    let verdict = tokio::time::timeout(
        Duration::from_secs(5),
        signalling.next(),
    )
    .await
    .expect("判死不该拖过五秒 —— 拖过就是那道超时没接上");

    assert!(
        verdict.is_none(),
        "静默超过时限就该判死,让编排循环去重连"
    );
}
