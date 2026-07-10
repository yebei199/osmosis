//! 开发用服务端。
//!
//! 它与客户端共享 [`contract`] crate,因此线上格式的一致性由编译器保证 ——
//! 改了 DTO 而忘了改另一侧,构建会直接失败。
//!
//! 运行:`just dev-server`(等价于 `cargo run -p server`)
//!
//! 注意 workspace 的 `default-members` 不含本 crate,裸 `cargo build` 不会编它。

use axum::{Json, Router, routing::get};
use contract::{HealthDto, PROTOCOL_VERSION};
use tower_http::cors::CorsLayer;

/// 默认监听地址。
///
/// 绑 `127.0.0.1` 而非 `0.0.0.0`:手机通过 `adb reverse tcp:3000 tcp:3000`
/// 把自己的 `127.0.0.1:3000` 转发到这里,不需要服务端暴露在局域网上。
const DEFAULT_BIND: &str = "127.0.0.1:3000";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/health", get(health))
        // 浏览器把 `localhost:3000` 视为跨源,wasm 端不开 CORS 连不上。
        // permissive 只适用于开发:它允许任意来源。
        .layer(CorsLayer::permissive());

    let bind = std::env::var("BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| {
            panic!("failed to bind {bind}: {e}")
        });

    println!("listening on http://{bind}");
    axum::serve(listener, app)
        .await
        .expect("server failed");
}

/// `GET /health` —— 能返回就说明服务端活着。
async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok".to_owned(),
        protocol_version: PROTOCOL_VERSION,
    })
}
