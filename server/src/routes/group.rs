//! 组的全局播放状态的意图路由(#142)。
//!
//! 意图走 HTTP 而不是信令:每一下点击都要一个确定的应答 —— 成了就是应用之后组的样子,
//! 没成就是一句看得懂的原因。状态的广播走信令(`ServerSignal::GroupState`)。
//! 规则与落库都在 `server::syncplay::group`,这里只管 HTTP 的形状。

use axum::Json;
use axum::extract::State;
use contract::{
    GroupLeaveDto, GroupOutputsDto, GroupPlayDto,
    GroupReplyDto, GroupTransportDto,
};
use server::error::{self, Failure};
use server::store::account::Account;
use server::syncplay::group::{self, Intent};

use crate::AppState;

/// `GET /group` —— 组此刻的样子。冷启动、重连之后取一次。
pub(crate) async fn current(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<GroupReplyDto>, Failure> {
    let current = group::current(&state.pool, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;
    Ok(Json(GroupReplyDto { state: current }))
}

/// `POST /group/play` —— 点歌。
pub(crate) async fn play(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<GroupPlayDto>,
) -> Result<Json<GroupReplyDto>, Failure> {
    apply(
        &state,
        &account,
        &body.device_id,
        Intent::Play(body.pick),
    )
    .await
}

/// `POST /group/transport` —— 暂停、继续、上一首、下一首、跳转、随机、循环。
pub(crate) async fn transport(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<GroupTransportDto>,
) -> Result<Json<GroupReplyDto>, Failure> {
    apply(
        &state,
        &account,
        &body.device_id,
        Intent::Transport(body.op),
    )
    .await
}

/// `POST /group/outputs` —— 改在哪几台出声。
pub(crate) async fn outputs(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<GroupOutputsDto>,
) -> Result<Json<GroupReplyDto>, Failure> {
    apply(
        &state,
        &account,
        &body.device_id,
        Intent::Outputs {
            outputs: body.outputs,
            seed: body.seed,
        },
    )
    .await
}

/// `POST /group/leave` —— 本机退出组。
pub(crate) async fn leave(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<GroupLeaveDto>,
) -> Result<Json<GroupReplyDto>, Failure> {
    apply(&state, &account, &body.device_id, Intent::Leave)
        .await
}

async fn apply(
    state: &AppState,
    account: &Account,
    device: &str,
    intent: Intent,
) -> Result<Json<GroupReplyDto>, Failure> {
    let applied = group::apply(
        &state.pool,
        &state.roster,
        account.id,
        device,
        intent,
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    Ok(Json(GroupReplyDto { state: applied }))
}
