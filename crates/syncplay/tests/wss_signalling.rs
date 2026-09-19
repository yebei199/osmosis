//! `wss://` 信令必须走到底层的连接层,而不是在 URL scheme 那一步就被
//! tungstenite 拒绝。#92:线上后端是 https,`ui` 把 api base 升成 `wss://`,
//! 而 `tokio-tungstenite` 没开任何 TLS feature 时,`connect_async` 对
//! `wss://` URL 恒返回 `Url(TlsFeatureNotEnabled)`——这一步在真的发起网络
//! 连接之前就失败,所以连一个必然拒绝的地址也测得出来。

use contract::DeviceDto;
use syncplay::Signalling;

#[tokio::test]
async fn wss_url_reaches_connection_layer_not_tls_url_error()
 {
    let device = DeviceDto {
        id: "test-device".to_owned(),
        name: "测试设备".to_owned(),
    };

    // `tokio-tungstenite` 的 `TlsFeatureNotEnabled` 是纯粹按 URL scheme 判的,
    // 不需要真的握手——但它是在 TCP 连接**建立之后**才检查的(见 connect.rs:
    // 先 `TcpStream::connect`,再进 TLS 分支)。所以必须有个真监听者接住这条
    // TCP 连接,不能像端口 1 那样直接被拒——那种情况下 Io 错误会在 TLS 检查
    // 之前就返回,测不出这个 bug。
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上端口");
    let addr = listener.local_addr().expect("取不到地址");
    tokio::spawn(async move {
        let _ = listener.accept().await;
    });

    // `Signalling` 不派生 `Debug`,`expect_err`/`unwrap_err` 都用不上。
    let Err(err) = Signalling::connect(
        &format!("wss://{addr}"),
        device,
        // 必须是合法的头值:不合法的话连接在发起之前就被本地拒了,测不到这条路。
        "wss-probe-token",
    )
    .await
    else {
        panic!("对端不是真的 wss 服务端,连接必须失败");
    };

    let message = err.to_string();
    assert!(
        !message.contains("TLS support not compiled in"),
        "tokio-tungstenite 未开 TLS feature,wss:// 在连接前就被拒绝: {message}"
    );
}
