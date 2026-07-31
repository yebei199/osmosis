//! 同播信令的端到端测试:真的开 WebSocket 连接,真的收发。
//!
//! 与 `live_bangdream.rs` 不同,这些**不需要外部进程** —— 服务端在测试进程里起,
//! 端口交给系统分配。所以它们不带 `#[ignore]`,每次 `cargo test` 都跑。
//!
//! 单测证明的是「路由函数把消息投给了谁」;这里证明的是「两条真实连接之间
//! 消息确实过去了」—— 序列化、帧类型、收发两端拆分,任何一环错了单测都看不见。

use std::net::SocketAddr;

use axum::Router;
use axum::routing::get;
use contract::{ClientSignal, DeviceDto, ServerSignal};
use futures_util::{SinkExt, StreamExt};
use server::signaling::{self, SharedRoster};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 在随机端口上起一个只有信令路由的服务端,返回它的地址。
///
/// 端口写 0 让系统分配:写死端口的话两个测试并发跑就会互相撞,
/// 而那种失败每次落在不同的测试上,看起来像随机的 flaky。
async fn start_server() -> SocketAddr {
    let app = Router::new()
        .route("/signal", get(signaling::handler))
        .with_state(SharedRoster::default());

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

/// 连上去并自报家门,返回连接。
async fn connect(addr: SocketAddr, id: &str) -> Socket {
    let (mut socket, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal"),
    )
    .await
    .expect("连不上信令端点");

    let hello = ClientSignal::Hello {
        device: DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        },
    };
    socket
        .send(Message::text(
            serde_json::to_string(&hello)
                .expect("序列化失败"),
        ))
        .await
        .expect("发不出 Hello");

    socket
}

/// 读下一条服务端消息。
async fn next_signal(socket: &mut Socket) -> ServerSignal {
    loop {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.next(),
        )
        .await
        .expect("等服务端消息超时")
        .expect("连接已关闭")
        .expect("读连接出错");

        if let Message::Text(text) = message {
            return serde_json::from_str(&text)
                .expect("服务端消息不是认识的形状");
        }
    }
}

/// 两台设备连上后,彼此都出现在对方的名册里。
#[tokio::test]
async fn two_devices_see_each_other() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    // a 独自在线时先收到一份只有自己的名册。
    assert!(matches!(
        next_signal(&mut a).await,
        ServerSignal::Roster { ref devices } if devices.len() == 1
    ));

    let mut b = connect(addr, "b").await;

    // b 上线让名册变化,两边都该被推到。
    let ServerSignal::Roster { devices } =
        next_signal(&mut a).await
    else {
        panic!("a 没收到更新后的名册");
    };
    let ids: Vec<&str> =
        devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"], "a 看到的名册不对");

    let ServerSignal::Roster { devices } =
        next_signal(&mut b).await
    else {
        panic!("b 没收到名册");
    };
    assert_eq!(devices.len(), 2, "b 看到的名册不对");
}

/// 一台设备断开,另一台**被主动推**新名册。
///
/// 关键是"主动" —— 让客户端轮询的话,下线到被发现之间有一段空窗,
/// 而那段时间里往它推流必然失败,看起来却像 WebRTC 的问题。
#[tokio::test]
async fn disconnect_updates_the_other_device() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    let _ = next_signal(&mut a).await;
    let b = connect(addr, "b").await;
    let _ = next_signal(&mut a).await;

    drop(b);

    let ServerSignal::Roster { devices } =
        next_signal(&mut a).await
    else {
        panic!("a 没收到断开后的名册");
    };
    assert_eq!(
        devices.len(),
        1,
        "b 断开后名册里不该还有它"
    );
    assert_eq!(devices[0].id, "a");
}

/// 信令穿过两条真实连接后**一个字节都没变**。
#[tokio::test]
async fn payload_crosses_unmodified() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    let _ = next_signal(&mut a).await;
    let mut b = connect(addr, "b").await;
    let _ = next_signal(&mut a).await;
    let _ = next_signal(&mut b).await;

    // 带 CRLF、带非 ASCII、带前后空白:任何"顺手清理"都会在这里露馅。
    let payload = "  v=0\r\na=ice-ufrag:紅蓮華\r\n\r\n  ";
    let signal = ClientSignal::Signal {
        to: "b".to_owned(),
        payload: payload.to_owned(),
    };
    a.send(Message::text(
        serde_json::to_string(&signal).expect("序列化失败"),
    ))
    .await
    .expect("发不出信令");

    let received = next_signal(&mut b).await;
    assert_eq!(
        received,
        ServerSignal::Signal {
            from: "a".to_owned(),
            payload: payload.to_owned(),
        }
    );
}

/// 目标不在线时,发信人收到错误 —— 而不是石沉大海。
#[tokio::test]
async fn signal_to_offline_device_reports_error() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    let _ = next_signal(&mut a).await;

    let signal = ClientSignal::Signal {
        to: "从来没上线过".to_owned(),
        payload: "v=0".to_owned(),
    };
    a.send(Message::text(
        serde_json::to_string(&signal).expect("序列化失败"),
    ))
    .await
    .expect("发不出信令");

    let received = next_signal(&mut a).await;
    assert!(
        matches!(
            received,
            ServerSignal::Error { ref code, .. }
                if code == "device_offline"
        ),
        "实得 {received:?}"
    );
}
