//! 网易云账号绑定:状态、二维码、扫码进展、解绑。
//!
//! 绑定按账号分片(`docs/adr/0017`),账号标识由鉴权提取器给出、经
//! [`bangdream::as_user`] 进 gRPC metadata —— 上游据此决定读写谁的凭据。
//! 本服务不认识凭据文件本身,那是 bang-dream 的私有格式。

use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{
    NeteaseStatusDto, QR_WAITING, QrEventDto, QrLoginDto,
};

use server::bangdream::{
    self,
    proto::{
        CreateQrLoginRequest, GetAccountStatusRequest,
        LogoutRequest, Platform, WatchQrLoginRequest,
    },
};
use server::error::Failure;
use server::store::account::Account;

use crate::{AppState, fail};

/// 一次轮询最多等上游多久给出第一条扫码进展。
///
/// 上游每一轮都先发当前态再睡(它的 `internal/rpc/server.go`),所以正常情况
/// 下第一条立刻就到。但那一条要先向网易云问一次 —— 平台不答的话这条路由跟着
/// 悬着,而客户端那侧只有十秒(api 的 `REQUEST_TIMEOUT`),现象是每次轮询都卡
/// 满十秒再报「网络错误」,界面上的码看起来死了。
///
/// 等不到就按「还在等」答:这一刻没有新消息,与真的还没人扫是同一件事,
/// 客户端下一轮再问即可。
const FIRST_EVENT_TIMEOUT: Duration =
    Duration::from_secs(5);

/// `GET /netease/status` —— 这个账号绑没绑网易云。
///
/// 未绑定回 200 而不是错误:那是个人页要显示的**状态**,不是这次请求失败了
/// (上游也这么答,见它的 `docs/adr/0005`)。
pub(crate) async fn status(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<NeteaseStatusDto>, Failure> {
    let mut auth = state.upstream.auth;
    let response = auth
        .get_account_status(bangdream::as_user(
            &account,
            GetAccountStatusRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(bangdream::account_status_to_dto(response)))
}

/// `POST /netease/qr` —— 要一张新的二维码。
///
/// 每次都新建一张,不复用:码本身会过期,而复用一张过期的码的现象是
/// 「扫了没反应」—— 用户会去怀疑自己的网易云。
pub(crate) async fn create_qr(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<QrLoginDto>, Failure> {
    let mut auth = state.upstream.auth;
    let response = auth
        .create_qr_login(bangdream::as_user(
            &account,
            CreateQrLoginRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(bangdream::qr_login_to_dto(response)))
}

/// `GET /netease/qr/{key}` —— 这一刻扫到哪一步了。
///
/// 上游那条是 server stream,这里只取第一条就挂断:客户端两端(native 与
/// wasm)都只有 `get_json` 这一种传输,为一条状态另开一类长连接不值
/// (见 issue #96 的实施计划)。
pub(crate) async fn qr_state(
    State(state): State<AppState>,
    account: Account,
    Path(key): Path<String>,
) -> Result<Json<QrEventDto>, Failure> {
    let mut auth = state.upstream.auth;
    let mut events = auth
        .watch_qr_login(bangdream::as_user(
            &account,
            WatchQrLoginRequest {
                platform: Platform::Netease as i32,
                key,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // 超时与「流上没有消息」是同一个下场:这一刻没有新进展。
    let first = tokio::time::timeout(
        FIRST_EVENT_TIMEOUT,
        events.message(),
    )
    .await
    .unwrap_or(Ok(None))
    .map_err(|status| fail(&status))?;

    Ok(Json(QrEventDto {
        state: first.map_or_else(
            || QR_WAITING.to_owned(),
            |event| bangdream::qr_state_name(event.state),
        ),
    }))
}

/// `DELETE /netease` —— 解绑。上游清掉这个账号的凭据,设备标识保留。
pub(crate) async fn unbind(
    State(state): State<AppState>,
    account: Account,
) -> Result<StatusCode, Failure> {
    let mut auth = state.upstream.auth;
    auth.logout(bangdream::as_user(
        &account,
        LogoutRequest {
            platform: Platform::Netease as i32,
        },
    ))
    .await
    .map_err(|status| fail(&status))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
