//! 网易云绑定:状态、二维码、扫码进展、解绑,以及轮询该怎么走。
//!
//! 绑定按**账号**分片,不是按设备(见服务端的 `docs/adr/0017`)——
//! 这几条都带着会话 token 走,上游据此知道是替谁绑。

use contract::{
    NeteaseStatusDto, QR_CONFIRMED, QR_EXPIRED, QR_SCANNED,
    QrEventDto, QrLoginDto,
};

use crate::url::netease_qr_url;
use crate::{ApiError, base_url, platform};

/// 一次轮询之后界面该做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrStep {
    /// 还在等人扫。
    Waiting,
    /// 扫到了,等手机上按确认。
    Scanned,
    /// 绑好了,不必再问。
    Bound,
    /// 上一张废了,换成这一张。
    Renewed(QrLoginDto),
}

/// `GET /netease/status` —— 这个账号绑没绑网易云。
pub async fn netease_status()
-> Result<NeteaseStatusDto, ApiError> {
    platform::get_json(format!(
        "{}/netease/status",
        base_url()
    ))
    .await
}

/// `POST /netease/qr` —— 要一张新的二维码。
///
/// 只拿到 `url`,像素由界面自己画(见 `contract::QrLoginDto`)。
pub async fn netease_qr() -> Result<QrLoginDto, ApiError> {
    platform::send_json::<(), QrLoginDto>(
        reqwest::Method::POST,
        format!("{}/netease/qr", base_url()),
        None,
    )
    .await
}

/// `GET /netease/qr/{key}` —— 问一次这张码扫到哪一步了。
pub async fn netease_qr_state(
    key: &str,
) -> Result<QrEventDto, ApiError> {
    platform::get_json(netease_qr_url(key)).await
}

/// `DELETE /netease` —— 解绑,上游清掉这个账号的凭据。
pub async fn netease_unbind() -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        reqwest::Method::DELETE,
        format!("{}/netease", base_url()),
        None,
    )
    .await
}

/// 轮询一次:问进展,过期了就顺手换一张新码回来。
///
/// **节奏不在这里**。本 crate 两端(native 与 wasm)都没有可用的定时器 ——
/// native 那侧的 tokio 只开了 `rt-multi-thread`,wasm 上压根没有 tokio,而
/// `cargo xtask boundaries` 有一条检查就叫「api 在 wasm 上不依赖 tokio」。
/// 隔多久问一次由界面的 `slint::Timer` 决定,那三端都有。
pub async fn qr_poll(
    key: &str,
) -> Result<QrStep, ApiError> {
    let event = netease_qr_state(key).await?;

    step_from(&event.state, netease_qr).await
}

/// 把一次轮询的结果翻成下一步。
///
/// 换码要发一次请求,那一步由 `renew` 交出来 —— 客户端两端的服务端地址都烘在
/// 编译期(`base_url`),请求函数在单测里指不到一个假服务端,而「什么时候该
/// 换码」恰恰是这里唯一的规则。
///
/// 没见过的状态当成「还在等」,继续问同一张码:换码是不可撤销的,而用户
/// 可能正对着这一张扫。
async fn step_from<F, Fut>(
    state: &str,
    renew: F,
) -> Result<QrStep, ApiError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<QrLoginDto, ApiError>>,
{
    Ok(match state {
        QR_CONFIRMED => QrStep::Bound,
        QR_SCANNED => QrStep::Scanned,
        QR_EXPIRED => QrStep::Renewed(renew().await?),
        _ => QrStep::Waiting,
    })
}

#[cfg(test)]
mod tests;
