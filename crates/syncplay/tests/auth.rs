//! 握手被拒时,客户端得分得清"这个 token 不作数了"和"网络不好"。
//!
//! 分不清的代价是一个安静的死循环:拿着同一个已经失效的 token 每隔几秒重连一次,
//! 每次都得到 401,而界面上只有一句不再变化的"连接失败"。

use std::net::SocketAddr;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use contract::DeviceDto;
use syncplay::{Signalling, SyncError};

/// 起一个只会用这个状态码回绝握手的服务端,响应体为空。
async fn start_rejecting_server(
    status: StatusCode,
) -> SocketAddr {
    serve(Router::new().route(
        "/signal",
        get(move || async move { status }),
    ))
    .await
}

/// 起一个像真服务端那样回绝的:401 带上 `ErrorDto`,code 是 `unauthorized`
/// (`server/src/gate/auth.rs` 认不出 token 时回的就是这一份)。
async fn start_token_rejecting_server() -> SocketAddr {
    serve(Router::new().route(
        "/signal",
        get(|| async {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(contract::ErrorDto {
                    code: "unauthorized".to_owned(),
                    message: "登录状态已失效".to_owned(),
                }),
            )
        }),
    ))
    .await
}

async fn serve(app: Router) -> SocketAddr {
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

fn device() -> DeviceDto {
    DeviceDto {
        id: "a".to_owned(),
        name: "设备 a".to_owned(),
    }
}

/// 服务端判 token 无效的 401 报成 [`SyncError::Unauthorized`] ——
/// 编排循环据此**不重试**,界面据此清掉会话。
#[tokio::test]
async fn a_401_reads_as_unauthorized() {
    let addr = start_token_rejecting_server().await;

    // `Signalling` 不派生 `Debug`,`expect_err` 用不上。
    let Err(error) = Signalling::connect(
        &format!("ws://{addr}"),
        device(),
        // 真 token 是十六进制串。非 ASCII 的值连请求头都进不去,
        // 那是另一条失败路径,不是这里要验的。
        "stale-token",
    )
    .await
    else {
        panic!("服务端回的是 401,连接必须失败");
    };

    assert!(
        matches!(error, SyncError::Unauthorized),
        "实得 {error}"
    );
}

/// 其余失败仍是可重试的那一种。
///
/// 与上一条分开:把所有握手失败都当成"登录失效"的话,服务端重启一次
/// 就会把人踢回登录页,而他的 token 好端端的。
#[tokio::test]
async fn other_failures_stay_retryable() {
    let addr = start_rejecting_server(
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;

    let Err(error) = Signalling::connect(
        &format!("ws://{addr}"),
        device(),
        "healthy-token",
    )
    .await
    else {
        panic!("服务端回的是 500,连接必须失败");
    };

    assert!(
        matches!(error, SyncError::Signalling(_)),
        "实得 {error}"
    );
}

/// 不带 `unauthorized` code 的 401 不算「token 不作数」(#127)。
///
/// 它可能来自半路的代理、或者一个根本不是我们服务端的东西。当成 token 失效的话,
/// 界面会删掉一份好好的会话,而删会话不可逆;当成可重试,代价只是多连几次。
#[tokio::test]
async fn a_bare_401_stays_retryable() {
    let addr =
        start_rejecting_server(StatusCode::UNAUTHORIZED)
            .await;

    let Err(error) = Signalling::connect(
        &format!("ws://{addr}"),
        device(),
        "healthy-token",
    )
    .await
    else {
        panic!("服务端回的是 401,连接必须失败");
    };

    assert!(
        matches!(error, SyncError::Signalling(_)),
        "实得 {error}"
    );
}
