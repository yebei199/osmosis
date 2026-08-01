//! 鉴权:把请求头里的 token 换成一个 [`Account`]。
//!
//! 做成提取器而不是中间件,是为了让「这条路由要不要鉴权」写在**签名里** ——
//! handler 的参数表上有 `Account` 就是要,没有就是不要。中间件的话这件事记在
//! 路由装配处,与 handler 隔着一段距离,加新路由时最容易漏。

use axum::{
    Json,
    extract::{FromRef, FromRequestParts},
    http::{StatusCode, request::Parts},
};
use contract::ErrorDto;
use sqlx::PgPool;

use crate::account::{self, Account};
use crate::error;

/// `Authorization: Bearer <token>` 的前缀。
const BEARER: &str = "Bearer ";

impl<S> FromRequestParts<S> for Account
where
    PgPool: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<ErrorDto>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix(BEARER))
            .ok_or_else(|| {
                error::unauthorized("缺少 Authorization 头")
            })?;

        let pool = PgPool::from_ref(state);
        let mut conn = pool
            .acquire()
            .await
            .map_err(|err| error::map_error(&err.into()))?;

        account::authenticate(&mut conn, token)
            .await
            .map_err(|err| error::map_error(&err))
    }
}
