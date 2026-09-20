//! 健康检查与账号:开户、登录、登出。

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
};
use contract::{
    HealthDto, LoginDto, PROTOCOL_VERSION, RegisterDto,
    SessionDto,
};

use server::error;
use server::error::Failure;
use server::store::account::{self, Account};

use crate::{AppState, conn};

/// 同一个来源 IP 一分钟内能试几次登录或注册。
///
/// 人手输密码一分钟撑死几次,这道闸拦的是脚本:没有它,`/login` 就是一条
/// 敞开的口令穷举通道,而每一次失败在日志里都长得一模一样。
const ATTEMPTS_PER_MINUTE: u32 = 20;

/// 记一次来自某个 IP 的账号操作,超了就回 429。
fn count_attempt(
    state: &AppState,
    from: std::net::SocketAddr,
) -> Result<(), Failure> {
    let mut limiter =
        state.limiter.lock().expect("限流器锁中毒");
    if limiter.check(
        &format!("account:{}", from.ip()),
        ATTEMPTS_PER_MINUTE,
    ) {
        Ok(())
    } else {
        Err(error::rate_limited())
    }
}

/// `GET /health` —— 能返回就说明服务端活着。
///
/// 它**不**探测 bang-dream:这里回答的是"本服务活着吗",上游是否可用
/// 由真正用到它的请求各自报告。混在一起的话,上游一挂客户端会以为后端整个死了。
pub(crate) async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok".to_owned(),
        protocol_version: PROTOCOL_VERSION,
    })
}

/// `POST /register` —— 凭邀请码开一个账号,并直接给出可用的会话。
///
/// 注册完顺手登录:否则客户端要连发两次请求,而中间那个"注册成功但没登录"的
/// 状态没有任何用处。
pub(crate) async fn register(
    State(state): State<AppState>,
    ConnectInfo(from): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<RegisterDto>,
) -> Result<Json<SessionDto>, Failure> {
    count_attempt(&state, from)?;

    let mut conn = conn(&state.pool).await?;

    let created = account::register(
        &mut conn,
        &body.username,
        &body.password,
        &body.invite,
        &state.invite,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    let token = account::login(
        &mut conn,
        &body.username,
        &body.password,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(Json(SessionDto {
        token,
        username: created.username,
    }))
}

/// `POST /login` —— 用用户名密码换一个会话 token。
pub(crate) async fn login(
    State(state): State<AppState>,
    ConnectInfo(from): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<LoginDto>,
) -> Result<Json<SessionDto>, Failure> {
    count_attempt(&state, from)?;

    let mut conn = conn(&state.pool).await?;

    let token = account::login(
        &mut conn,
        &body.username,
        &body.password,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    // 回显规范化后的账号名(登录时大小写不敏感),界面据此显示
    let account = account::authenticate(&mut conn, &token)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(SessionDto {
        token,
        username: account.username,
    }))
}

/// `POST /logout` —— 吊销**这一条**会话,别处登录的 token 不受影响。
///
/// 参数里的 `Account` 不是摆设:它保证了只有持有效 token 的人能走到这里。
/// 要删的 token 从请求头再取一次 —— 提取器把它换成了账号,没有留下原文。
pub(crate) async fn logout(
    State(state): State<AppState>,
    _account: Account,
    headers: HeaderMap,
) -> Result<StatusCode, Failure> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| {
            error::unauthorized("缺少 Authorization 头")
        })?;

    let mut conn = conn(&state.pool).await?;
    account::logout(&mut conn, token)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}
