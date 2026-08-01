//! 失败到 HTTP 响应的映射:上游 gRPC 的,以及自家账号那侧的。
//!
//! HTTP 状态码只做**粗分类**:4xx 是请求方的问题,5xx 是服务端这边的问题。
//! 精确语义交给 [`ErrorDto::code`] —— 客户端按 code 分支,不按状态码。
//! 这样以后细分错误时只需新增 code,不必重排状态码,也就不会破坏老客户端。

use axum::{Json, http::StatusCode};
use contract::ErrorDto;

use crate::account::AccountError;

/// 失败响应:状态码 + [`ErrorDto`]。
pub type Failure = (StatusCode, Json<ErrorDto>);

/// 401,附一句给人看的说明。
pub fn unauthorized(message: &str) -> Failure {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorDto {
            code: "unauthorized".to_owned(),
            message: message.to_owned(),
        }),
    )
}

/// 把账号那侧的失败翻成一对 (HTTP 状态码, 响应体)。
///
/// 密码错与用户不存在映射到**同一个** code 和同一句话:分开会把
/// 「这个用户名存在」白送给试探的人(见 [`AccountError::BadCredentials`])。
pub fn map_account_error(err: &AccountError) -> Failure {
    let (http, code, message) = match err {
        AccountError::BadInvite => (
            StatusCode::FORBIDDEN,
            "bad_invite",
            "邀请码不对".to_owned(),
        ),
        AccountError::UsernameTaken => (
            StatusCode::CONFLICT,
            "username_taken",
            "用户名已被占用".to_owned(),
        ),
        AccountError::BadCredentials => (
            StatusCode::UNAUTHORIZED,
            "bad_credentials",
            "用户名或密码不对".to_owned(),
        ),
        AccountError::BadToken => (
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "登录状态已失效".to_owned(),
        ),
        AccountError::Invalid(why) => (
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            (*why).to_owned(),
        ),
        // 数据库出错是本服务这边的问题,细节只进日志 ——
        // 原样抛出去可能带上连接串,而客户端拿它也没办法。
        AccountError::Db(err) => {
            tracing::error!(%err, "数据库操作失败");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "内部错误".to_owned(),
            )
        }
    };

    (
        http,
        Json(ErrorDto {
            code: code.to_owned(),
            message,
        }),
    )
}

/// 把上游的 gRPC 失败翻成一对 (HTTP 状态码, 响应体)。
pub fn map_status(
    status: &tonic::Status,
) -> (StatusCode, ErrorDto) {
    let (http, code) = match status.code() {
        // 连不上 bang-dream:它没起来,或者地址配错了。
        tonic::Code::Unavailable => (
            StatusCode::BAD_GATEWAY,
            "upstream_unreachable",
        ),
        // 网易云那边没登录。客户端见到这个 code 应提示去扫码,
        // 而不是重试 —— 重试一万次也不会自己登上。
        tonic::Code::Unauthenticated => (
            StatusCode::SERVICE_UNAVAILABLE,
            "netease_not_logged_in",
        ),
        tonic::Code::NotFound => {
            (StatusCode::NOT_FOUND, "not_found")
        }
        tonic::Code::InvalidArgument => {
            (StatusCode::BAD_REQUEST, "invalid_argument")
        }
        // 兜底也算上游的错:本服务只是转发,自己没有失败的余地。
        _ => (StatusCode::BAD_GATEWAY, "upstream_failed"),
    };

    (
        http,
        ErrorDto {
            code: code.to_owned(),
            message: status.message().to_owned(),
        },
    )
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;
    use tonic::{Code, Status};

    use super::*;

    /// bang-dream 进程没起来 → 502。故障在上游,不是本服务内部出错,
    /// 用 500 会让客户端和日志都指错方向。
    #[test]
    fn unavailable_maps_to_502() {
        let (code, body) = map_status(
            &Status::unavailable("connection refused"),
        );

        assert_eq!(code, StatusCode::BAD_GATEWAY);
        assert_eq!(body.code, "upstream_unreachable");
    }

    /// 网易云未登录 → 503:本服务暂时干不了这活。
    /// 不用 401 —— 401 会被理解成「这台设备要登录」,而设备本来就没有认证(ADR 0009)。
    #[test]
    fn unauthenticated_maps_to_503() {
        let (code, body) = map_status(
            &Status::unauthenticated("not logged in"),
        );

        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.code, "netease_not_logged_in");
    }

    /// 歌曲不存在 → 404,原样传递上游的判断。
    #[test]
    fn not_found_maps_to_404() {
        let (code, body) =
            map_status(&Status::not_found("no such track"));

        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body.code, "not_found");
    }

    /// 参数不合法 → 400。这是唯一由请求方负责的一类,必须落在 4xx。
    #[test]
    fn invalid_argument_maps_to_400() {
        let (code, body) = map_status(
            &Status::invalid_argument("empty keyword"),
        );

        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body.code, "invalid_argument");
    }

    /// 没有专门映射的 gRPC 码走兜底 502,不静默吞掉也不猜语义。
    #[test]
    fn unmapped_status_falls_back_to_502() {
        let (code, body) = map_status(&Status::new(
            Code::ResourceExhausted,
            "rate limited",
        ));

        assert_eq!(code, StatusCode::BAD_GATEWAY);
        assert_eq!(body.code, "upstream_failed");
    }

    /// 上游给的说明要带到响应体里 —— 但只给人看。
    /// 客户端做分支判断只许用 `code`,`message` 的措辞不属于契约。
    #[test]
    fn error_body_carries_stable_code() {
        let (_, body) = map_status(&Status::unavailable(
            "connection refused",
        ));

        assert_eq!(body.code, "upstream_unreachable");
        assert!(
            body.message.contains("connection refused")
        );
    }
}
