//! 网易云账号绑定的线上格式。
//!
//! 绑定**按账号分片**(见 `docs/adr/0017`):这里说的是「这个账号的网易云
//! 凭据」,不是这台设备的,也不是全局的。同一个人在桌面绑好,手机上登同一个
//! 账号就已经是绑好的。

use serde::{Deserialize, Serialize};

/// 扫码还在等人扫。
pub const QR_WAITING: &str = "waiting";
/// 已经扫到了,等手机上按确认。
pub const QR_SCANNED: &str = "scanned";
/// 确认了,凭据已落到上游。
pub const QR_CONFIRMED: &str = "confirmed";
/// 这张码过期了,要换一张。
pub const QR_EXPIRED: &str = "expired";

/// `GET /netease/status` 的响应体:这个账号绑没绑网易云。
///
/// 未绑定是**状态而不是错误**,所以这条路由未绑定时照样是 200
/// (上游也这么答,见 bang-dream 的 `docs/adr/0005`)。回 4xx 的话客户端要
/// 在错误分支里读一个正常值,而「没绑」正是个人页要显示的那个值。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct NeteaseStatusDto {
    pub bound: bool,
    /// 平台昵称。绑上了才有,没绑就是 `None` 而不是空串 ——
    /// 空串会被界面当成一个真实存在的空名字画出来。
    pub nickname: Option<String>,
}

/// `POST /netease/qr` 的响应体:一张新的二维码登录会话。
///
/// 只给 `url` 不给图。二维码的像素由客户端渲染 —— 上游 proto 的
/// `CreateQRLoginResponse.url` 就是这么规定的。服务端渲成 PNG 再 base64 的话,
/// web 端还得再解一次码,而那侧没有解码器(ui 的 `image` 只编进原生端)。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct QrLoginDto {
    /// 这一张码的标识。问它的进展时要带上。
    pub key: String,
    /// 二维码要承载的内容,客户端据此画图。
    pub url: String,
}

/// `GET /netease/qr/{key}` 的响应体:这一刻扫到哪一步了。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct QrEventDto {
    /// [`QR_WAITING`]、[`QR_SCANNED`]、[`QR_CONFIRMED`]、[`QR_EXPIRED`] 之一。
    ///
    /// 用字符串而不是枚举:上游将来多一种状态时,老客户端该把它当成「还在等」
    /// 继续轮询,而不是整条响应解不出来 —— 那会让界面停在一张永远不动的码上。
    pub state: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没绑的时候服务端不发 nickname,那不该让整条响应解不出来。
    ///
    /// 界面正是在「没绑」这一支上要显示二维码 —— 这条响应解不出来,
    /// 现象就是个人页永远停在加载中。
    #[test]
    fn a_status_without_a_nickname_still_parses() {
        let dto: NeteaseStatusDto =
            serde_json::from_str(r#"{"bound":false}"#)
                .expect("少一个昵称不该让整条响应解不出来");

        assert!(!dto.bound);
        assert_eq!(dto.nickname, None);
    }

    /// 没见过的状态照样解得出来,由调用方决定怎么对待它。
    #[test]
    fn an_unknown_state_is_not_a_decode_failure() {
        let dto: QrEventDto = serde_json::from_str(
            r#"{"state":"brand_new"}"#,
        )
        .expect("没见过的状态不该解不出来");

        assert_eq!(dto.state, "brand_new");
    }
}
