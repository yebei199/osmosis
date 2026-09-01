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

/// 不带登录态打一条受保护的路由,必须失败。
///
/// 头长什么样由 `platform::native` 的单测证明(那里在本机接住请求原文,
/// 不需要服务端)。这条与下一条证明的是另一件事:**服务端认这个头** ——
/// 两侧对同一个头的拼法各错一半,只有真的连起来才看得见。
#[tokio::test]
#[ignore = "需要 `just dev-server` 正在运行"]
async fn protected_route_without_token_is_rejected() {
    api::session::clear();

    assert!(
        api::daily().await.is_err(),
        "没有登录态却拿到了每日推荐"
    );
}

/// 登录之后同一条路由能通。
///
/// 账号由 `TEST_ACCOUNT` / `TEST_PASSWORD` 给,邀请码由 `TEST_INVITE` 给;
/// 账号不存在就现注册一个 —— 这条测试要能在一个空库上从头跑通。
#[tokio::test]
#[ignore = "需要 `just dev-server` 正在运行,且给定 TEST_INVITE"]
async fn protected_route_with_token_succeeds() {
    let username = std::env::var("TEST_ACCOUNT")
        .unwrap_or_else(|_| "roundtrip".to_owned());
    let password = std::env::var("TEST_PASSWORD")
        .unwrap_or_else(|_| "correct horse".to_owned());
    let invite = std::env::var("TEST_INVITE").expect(
        "需要 TEST_INVITE,与服务端的 INVITE_CODE 一致",
    );

    api::session::clear();

    // 已经注册过就直接登录。注册失败的原因不止"重名",所以登录那步的
    // 错误才是真正该报出来的那个。
    if api::register(&username, &password, &invite)
        .await
        .is_err()
    {
        api::login(&username, &password)
            .await
            .expect("登录失败");
    }

    api::daily().await.expect("登录后仍取不到每日推荐");

    api::logout().await.expect("登出失败");
}
