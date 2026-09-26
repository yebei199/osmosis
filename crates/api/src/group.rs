//! 组的全局播放状态的意图(#142)。
//!
//! 每一下都是一次 HTTP 往返:成了就带回应用之后组的样子,没成就是一句看得懂的原因。
//! 状态的广播走信令(`syncplay` 的 `Event::GroupState`),这里只管发意图。

use contract::{
    GroupLeaveDto, GroupOutputsDto, GroupPickDto,
    GroupPlayDto, GroupReplyDto, GroupSeedDto,
    GroupStateDto, GroupTransportDto, TransportOpDto,
};

use crate::url::group_url;
use crate::{ApiError, platform};

/// `GET /group` —— 组此刻的样子。没有组是 `None`。
pub async fn group_state()
-> Result<Option<GroupStateDto>, ApiError> {
    let reply: GroupReplyDto =
        platform::get_json(group_url("")).await?;
    Ok(reply.state)
}

/// `POST /group/play` —— 点歌。
pub async fn group_play(
    device_id: &str,
    pick: GroupPickDto,
) -> Result<Option<GroupStateDto>, ApiError> {
    post(
        "/play",
        GroupPlayDto {
            device_id: device_id.to_owned(),
            pick,
        },
    )
    .await
}

/// `POST /group/transport` —— 暂停、继续、上一首、下一首、跳转、随机、循环。
pub async fn group_transport(
    device_id: &str,
    op: TransportOpDto,
) -> Result<Option<GroupStateDto>, ApiError> {
    post(
        "/transport",
        GroupTransportDto {
            device_id: device_id.to_owned(),
            op,
        },
    )
    .await
}

/// `POST /group/outputs` —— 改在哪几台出声。
pub async fn group_outputs(
    device_id: &str,
    outputs: Vec<String>,
    seed: Option<GroupSeedDto>,
) -> Result<Option<GroupStateDto>, ApiError> {
    post(
        "/outputs",
        GroupOutputsDto {
            device_id: device_id.to_owned(),
            outputs,
            seed,
        },
    )
    .await
}

/// `POST /group/leave` —— 本机退出组。
pub async fn group_leave(
    device_id: &str,
) -> Result<Option<GroupStateDto>, ApiError> {
    post(
        "/leave",
        GroupLeaveDto {
            device_id: device_id.to_owned(),
        },
    )
    .await
}

async fn post<T: serde::Serialize + Send + 'static>(
    path: &str,
    body: T,
) -> Result<Option<GroupStateDto>, ApiError> {
    let reply: GroupReplyDto = platform::send_json(
        reqwest::Method::POST,
        group_url(path),
        Some(body),
    )
    .await?;
    Ok(reply.state)
}
