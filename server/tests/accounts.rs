//! 账号与会话的集成测试,对着真实 Postgres 跑。
//!
//! 不写成 `#[ignore]`:这是鉴权路径,平时不跑的安全测试等于没有。
//! 起库见 `just pg`;库不在时这里会明确报出来,而不是静默跳过。
//!
//! 每个测试在自己的事务里跑完就回滚,因此可以并行,且不留痕迹。

use axum::{
    extract::FromRequestParts, http::Request,
    http::StatusCode,
};
use server::account::{self, Account, register};
use server::db;
use server::error::AppError;
use sqlx::{PgPool, Postgres, Transaction};

/// 与 `main.rs` 的默认值一致。那个常量属于进程装配,不在 lib 里,
/// 这里重复一次 —— 它漂移了下面每条测试立刻连不上,不会静默失效。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

/// 测试用的邀请码。
const INVITE: &str = "let-me-in";

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );

    db::connect(&url).await.unwrap_or_else(|err| {
        panic!(
            "连不上数据库({url}): {err}\n\
             起一个:just pg"
        )
    })
}

/// 一个用完即回滚的事务。测试之间因此互不可见,也不留数据。
async fn tx() -> Transaction<'static, Postgres> {
    pool().await.begin().await.expect("开事务失败")
}

/// 注册一个账号,用默认邀请码。
async fn make_account(
    tx: &mut Transaction<'static, Postgres>,
    username: &str,
    password: &str,
) -> Result<Account, AppError> {
    register(tx, username, password, INVITE, INVITE).await
}

/// 注册成功后能用同一副用户名密码登录,拿到的 token 能过鉴权。
/// 这条串起了整条链路,下面几条都是它的边界情形。
#[tokio::test]
async fn register_then_login_returns_a_working_token() {
    let mut tx = tx().await;

    let created =
        make_account(&mut tx, "alice_1", "correct horse")
            .await
            .expect("注册应该成功");

    let token =
        account::login(&mut tx, "alice_1", "correct horse")
            .await
            .expect("登录应该成功");

    let authenticated =
        account::authenticate(&mut tx, &token)
            .await
            .expect("刚签发的 token 应该有效");

    assert_eq!(authenticated, created);
}

/// 邀请码不对就不给注册。服务面向公网,没有它任何人都能开户。
#[tokio::test]
async fn register_requires_the_invite_code() {
    let mut tx = tx().await;

    let result = register(
        &mut tx,
        "mallory",
        "correct horse",
        "guessed",
        INVITE,
    )
    .await;

    assert!(matches!(result, Err(AppError::BadInvite)));

    // 而且没有留下账号 —— 拒绝要发生在写库之前
    assert!(matches!(
        account::login(&mut tx, "mallory", "correct horse")
            .await,
        Err(AppError::BadCredentials)
    ));
}

/// 用户名已被占用时明确拒绝,而不是静默建出第二个同名账号。
/// 唯一索引建在 lower(username) 上,所以大小写不同也算占用。
#[tokio::test]
async fn duplicate_username_is_rejected() {
    let mut tx = tx().await;

    make_account(&mut tx, "bob_1", "correct horse")
        .await
        .expect("第一次注册应该成功");

    let again =
        make_account(&mut tx, "BOB_1", "another password")
            .await;

    assert!(matches!(again, Err(AppError::UsernameTaken)));
}

/// 密码错了拒绝登录,且回的错误与"用户不存在"不可区分 ——
/// 两者分开会把「这个用户名存在」白送给试探的人。
#[tokio::test]
async fn wrong_password_and_unknown_user_are_indistinguishable()
 {
    let mut tx = tx().await;

    make_account(&mut tx, "carol_1", "correct horse")
        .await
        .expect("注册应该成功");

    let wrong_password = account::login(
        &mut tx,
        "carol_1",
        "battery staple",
    )
    .await;
    let unknown_user = account::login(
        &mut tx,
        "nobody_here",
        "battery staple",
    )
    .await;

    assert!(matches!(
        wrong_password,
        Err(AppError::BadCredentials)
    ));
    assert!(matches!(
        unknown_user,
        Err(AppError::BadCredentials)
    ));

    // 连回给客户端的那句话也必须一样
    let (wrong_status, wrong_body) =
        server::error::map_error(
            &wrong_password.unwrap_err(),
        );
    let (unknown_status, unknown_body) =
        server::error::map_error(
            &unknown_user.unwrap_err(),
        );

    assert_eq!(wrong_status, unknown_status);
    assert_eq!(wrong_body.0, unknown_body.0);
}

/// 没见过的 token 一律拒绝。
#[tokio::test]
async fn unknown_token_is_rejected() {
    let mut tx = tx().await;

    let result = account::authenticate(
        &mut tx,
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .await;

    assert!(matches!(result, Err(AppError::BadToken)));
}

/// 登出只吊销这一台设备的会话,同账号在别处的 token 仍然有效。
/// 做错了的现象是"手机登出把桌面也踢了",而那要等到真有两台设备时才会被发现。
#[tokio::test]
async fn logout_revokes_only_that_session() {
    let mut tx = tx().await;

    make_account(&mut tx, "dave_1", "correct horse")
        .await
        .expect("注册应该成功");

    let phone =
        account::login(&mut tx, "dave_1", "correct horse")
            .await
            .expect("第一台设备登录");
    let desktop =
        account::login(&mut tx, "dave_1", "correct horse")
            .await
            .expect("第二台设备登录");

    account::logout(&mut tx, &phone)
        .await
        .expect("登出应该成功");

    assert!(matches!(
        account::authenticate(&mut tx, &phone).await,
        Err(AppError::BadToken)
    ));
    assert!(
        account::authenticate(&mut tx, &desktop)
            .await
            .is_ok(),
        "另一台设备的会话不该被牵连"
    );
}

/// 受保护的路由不带 token 时 401。
///
/// 用提取器直接验:它跑在 handler 之前,拒绝了就**根本到不了**上游那一步 ——
/// 「不碰上游」由这个顺序保证,不需要另外断言。
#[tokio::test]
async fn protected_route_without_token_is_unauthorized() {
    let pool = pool().await;

    let (mut parts, ()) =
        Request::builder().body(()).unwrap().into_parts();

    let rejection =
        Account::from_request_parts(&mut parts, &pool)
            .await
            .expect_err("没带 token 不该通过");

    assert_eq!(rejection.0, StatusCode::UNAUTHORIZED);
}

/// 两个账号拿到的上游用户标识不同。它是 bang-dream 分片凭据的键,
/// 撞了就是两个账号共用一份网易云登录。
#[tokio::test]
async fn each_account_gets_its_own_upstream_user_id() {
    let mut tx = tx().await;

    let first =
        make_account(&mut tx, "erin_1", "correct horse")
            .await
            .expect("注册应该成功");
    let second =
        make_account(&mut tx, "erin_2", "correct horse")
            .await
            .expect("注册应该成功");

    assert_ne!(
        first.upstream_user_id(),
        second.upstream_user_id()
    );
    // 而且必须落在 bang-dream 允许的字符集里,否则那侧一律 INVALID_ARGUMENT
    assert!(first.upstream_user_id().chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }));
}
