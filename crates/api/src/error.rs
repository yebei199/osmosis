//! 服务端地址、请求可能的失败方式,以及错误响应体的翻译。

use contract::ErrorDto;

/// 服务端地址。可在编译期用 `OSMOSIS_API_BASE` 覆盖。
///
/// 默认指向 `127.0.0.1` —— Android 上这是**手机自己**的回环地址,需要
/// `adb reverse tcp:3000 tcp:3000` 把它转发到开发机(见 `just adb-reverse`)。
pub fn base_url() -> &'static str {
    option_env!("OSMOSIS_API_BASE")
        .unwrap_or("http://127.0.0.1:3000")
}

/// 一次请求可能的失败方式。
///
/// 这些都不是线上格式,因此不属于 `contract`:它们描述的是"没能完成一次往返",
/// 而不是"服务端说了什么"。
#[derive(Debug)]
pub enum ApiError {
    /// 连不上、超时、非 2xx —— 请求没能走完。
    Transport(String),
    /// 走完了,但响应体不是我们认识的形状。
    Decode(String),
    /// 双方说的不是同一个版本的协议。
    VersionMismatch { expected: u32, actual: u32 },
    /// 服务端明确拒绝了,并说明了原因。
    ///
    /// 与 [`Self::Transport`] 的区别是**有没有得到答复**:这一个是服务端想清楚了
    /// 才说的不,调用方可以按 `code` 分支;那一个是话没传到。
    ///
    /// 不带 HTTP 状态码:契约规定客户端按 `code` 分支而不是按状态码
    /// (见 `contract::ErrorDto`),带上它只会诱使人去用错的那个。
    Server { code: String, message: String },
}

impl core::fmt::Display for ApiError {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            Self::Transport(message) => {
                write!(f, "网络错误: {message}")
            }
            Self::Decode(message) => {
                write!(f, "响应格式错误: {message}")
            }
            Self::Server { message, .. } => {
                write!(f, "{message}")
            }
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "协议版本不匹配: 本机 v{expected},服务端 v{actual}"
                )
            }
        }
    }
}

impl core::error::Error for ApiError {}

/// 把服务端的错误响应体翻成一个带 code 的错误。
///
/// 解不出 [`ErrorDto`] 就退回 [`ApiError::Transport`] —— 502 网关回的是 HTML,
/// 反向代理回的可能是别的东西。编一个 code 出来会让上层按错误的分支走,
/// 而那种错比"不知道为什么失败"更难查。
pub(crate) fn server_error(
    status: u16,
    body: &str,
) -> ApiError {
    match serde_json::from_str::<ErrorDto>(body) {
        Ok(dto) => ApiError::Server {
            code: dto.code,
            message: dto.message,
        },
        Err(_) => ApiError::Transport(format!(
            "HTTP {status}: {}",
            body.chars().take(200).collect::<String>()
        )),
    }
}

#[cfg(test)]
mod tests;
