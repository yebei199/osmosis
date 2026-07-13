//! 真实网络往返。需要开发服务端正在跑,因此默认 `#[ignore]`。
//!
//! 运行:一个终端 `just dev-server`,另一个终端 `just test-api`。
//!
//! 失败路径不在这里测:`base_url()` 是编译期常量,同一进程内无法把它指向别处。
//! 协议版本不匹配由 `api` 自己的 `check_version` 单测覆盖(纯函数,不碰网络);
//! 三态转换与其余错误路径由 `app-core` 的单测覆盖,那里的 `fetch` 是注入的。

/// 打一次真实的 GET /health,校验响应能被解析且协议版本匹配。
#[tokio::test]
#[ignore = "需要 `just dev-server` 正在运行"]
async fn health_fetches_server_status() {
    let dto =
        api::health().await.expect("GET /health 应当成功");
    assert_eq!(dto.status, "ok");
    assert_eq!(
        dto.protocol_version,
        contract::PROTOCOL_VERSION
    );
}
