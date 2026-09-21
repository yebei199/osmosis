//! `/signal` 的鉴权:谁能把连接升上来,谁在门口就被挡住。
//!
//! 与 `live_signaling.rs` 的分工:那边验"消息有没有过去",走不鉴权的测试路由;
//! 这里验的正是**账号从哪来**,所以必须走生产那条 handler,也就必须有真库 ——
//! token 是库里的一行,不是一个能打桩的字符串。起库见 `just pg`。
//!
//! 不写成 `#[ignore]`:这是鉴权路径,平时不跑的安全测试等于没有。

use std::net::SocketAddr;

use axum::extract::FromRef;
use axum::routing::get;
use server::store::{account, db};
use server::syncplay::signaling::{
    self, AllowedOrigins, SharedControl, SharedRoster,
};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite;
use tungstenite::client::IntoClientRequest;

/// 与 `main.rs` 的默认值一致。那个常量属于进程装配,不在 lib 里,
/// 这里重复一次 —— 它漂移了下面每条测试立刻连不上,不会静默失效。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

/// 测试用的邀请码。测试自己既当配置方又当注册方,两边给同一个值。
const INVITE: &str = "let-me-in";

/// 服务端的 state:鉴权提取器要池,信令 handler 要名册。
///
/// 与 `AppState` 同形但只有这几样 —— 那个结构在二进制 crate 里,集成测试
/// 引不到。**限流不在这里**:它挂在路由组上(`main::signal_routes`),
/// 而这一组测的是鉴权与来源校验,两件事各测各的。
#[derive(Clone)]
struct SignalState {
    pool: PgPool,
    roster: SharedRoster,
    control: SharedControl,
    origins: AllowedOrigins,
}

impl FromRef<SignalState> for PgPool {
    fn from_ref(state: &SignalState) -> Self {
        state.pool.clone()
    }
}

impl FromRef<SignalState> for SharedRoster {
    fn from_ref(state: &SignalState) -> Self {
        state.roster.clone()
    }
}

impl FromRef<SignalState> for SharedControl {
    fn from_ref(state: &SignalState) -> Self {
        state.control.clone()
    }
}

impl FromRef<SignalState> for AllowedOrigins {
    fn from_ref(state: &SignalState) -> Self {
        state.origins.clone()
    }
}

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

/// 造一个账号并登录,返回可用的 token。
///
/// 不在事务里跑:服务端从池里另取连接查 token,没提交的那一行它看不见。
/// 所以每条测试用自己的账号名,开跑先删掉上一轮的残留。
async fn token_for(
    pool: &PgPool,
    username: &str,
) -> String {
    sqlx::query(
        "DELETE FROM accounts WHERE lower(username) = lower($1)",
    )
    .bind(username)
    .execute(pool)
    .await
    .expect("清不掉上一轮的账号");

    let mut conn =
        pool.acquire().await.expect("取不到连接");
    account::register(
        &mut conn,
        username,
        "correct horse",
        INVITE,
        INVITE,
    )
    .await
    .expect("注册失败");
    account::login(&mut conn, username, "correct horse")
        .await
        .expect("登录失败")
}

/// 白名单里的那个来源。
const ALLOWED_ORIGIN: &str = "http://127.0.0.1:8073";

/// 在随机端口上起一个带鉴权的信令服务端。
async fn start_server(pool: PgPool) -> SocketAddr {
    let app = axum::Router::new()
        .route("/signal", get(signaling::handler))
        .with_state(SignalState {
            pool,
            roster: SharedRoster::default(),
            control: SharedControl::default(),
            origins: AllowedOrigins::new(vec![
                ALLOWED_ORIGIN.to_owned(),
            ]),
        });

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

/// 试着连上去,`token` 为 `None` 时一个 Authorization 头都不带。
async fn try_connect(
    addr: SocketAddr,
    token: Option<&str>,
) -> Result<(), tungstenite::Error> {
    try_connect_from(addr, token, None).await
}

/// 同上,外加一个自报的 `Origin` —— 原生端不带,浏览器一定带。
async fn try_connect_from(
    addr: SocketAddr,
    token: Option<&str>,
    origin: Option<&str>,
) -> Result<(), tungstenite::Error> {
    let mut request = format!("ws://{addr}/signal")
        .into_client_request()
        .expect("URL 不合法");
    if let Some(token) = token {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .expect("头值不合法"),
        );
    }
    if let Some(origin) = origin {
        request.headers_mut().insert(
            "Origin",
            origin.parse().expect("头值不合法"),
        );
    }

    tokio_tungstenite::connect_async(request)
        .await
        .map(|_| ())
}

/// 握手被拒时的状态码。不是 HTTP 失败(比如连接被重置)就直接失败。
fn rejected_status(err: &tungstenite::Error) -> u16 {
    match err {
        tungstenite::Error::Http(response) => {
            response.status().as_u16()
        }
        other => panic!("期待一个 HTTP 拒绝,实得 {other}"),
    }
}

/// 不带 token 连不上 —— 今天靠 tailnet 挡着,明天要面对公网。
#[tokio::test]
async fn connecting_without_a_token_is_rejected() {
    let addr = start_server(pool().await).await;

    let err = try_connect(addr, None)
        .await
        .expect_err("没有 token 不该连得上");

    assert_eq!(rejected_status(&err), 401);
}

/// 随手编一个 token 也连不上。
///
/// 与上一条分开:漏判"有头就放行"的实现能过上一条,却在这里露馅。
#[tokio::test]
async fn connecting_with_a_bogus_token_is_rejected() {
    let addr = start_server(pool().await).await;

    let err =
        try_connect(addr, Some("this-token-never-existed"))
            .await
            .expect_err("假 token 不该连得上");

    assert_eq!(rejected_status(&err), 401);
}

/// 登录拿到的 token 连得上。
#[tokio::test]
async fn a_real_token_gets_in() {
    let pool = pool().await;
    let token = token_for(&pool, "signal_auth_ok").await;
    let addr = start_server(pool).await;

    try_connect(addr, Some(&token))
        .await
        .expect("真 token 该连得上");
}

/// 白名单之外的浏览器来源连不上。
///
/// 同源策略管不到 WebSocket:不校验 Origin 的话,任意网页都能借用户浏览器里的
/// 登录态连上来 —— 而用户什么都看不到。
#[tokio::test]
async fn a_foreign_origin_is_refused() {
    let pool = pool().await;
    let token =
        token_for(&pool, "signal_auth_origin").await;
    let addr = start_server(pool).await;

    let err = try_connect_from(
        addr,
        Some(&token),
        Some("https://evil.example"),
    )
    .await
    .expect_err("白名单外的来源不该连得上");

    assert_eq!(rejected_status(&err), 403);
}

/// 白名单里的来源照常放行,原生端(不带 Origin)也照常。
#[tokio::test]
async fn an_allowed_origin_still_gets_in() {
    let pool = pool().await;
    let token =
        token_for(&pool, "signal_auth_origin_ok").await;
    let addr = start_server(pool).await;

    try_connect_from(
        addr,
        Some(&token),
        Some(ALLOWED_ORIGIN),
    )
    .await
    .expect("白名单里的来源该连得上");
}

/// 同一个账号短时间内反复建连会被限流。
///
/// 没有这道闸,一个空转的重连循环就能把服务端的连接槽占满,
/// 而每一次建连在日志里都长得和正常重连一模一样。
#[tokio::test]
async fn hammering_the_endpoint_gets_rate_limited() {
    let pool = pool().await;
    let token = token_for(&pool, "signal_auth_flood").await;
    let addr = start_server(pool).await;

    // 配额是每分钟 30 条。前 30 条都该放行。
    for attempt in 0..30 {
        try_connect(addr, Some(&token))
            .await
            .unwrap_or_else(|err| {
                panic!("第 {attempt} 条就被挡了: {err}")
            });
    }

    let err = try_connect(addr, Some(&token))
        .await
        .expect_err("超出配额之后该被挡下");

    assert_eq!(rejected_status(&err), 429);
}
