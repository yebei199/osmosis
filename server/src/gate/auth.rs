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

use crate::error;
use crate::store::account::{self, Account};

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
        // 这一条请求上已经认过了就直接用那一份。
        //
        // 限流按账号分桶,而它在中间件层跑、拿不到提取器的返回值 ——
        // 所以前置一个 `from_extractor_with_state::<Account, _>` 把账号放进
        // extensions,限流从那里读(见 `crate::gate::ratelimit`)。handler 上
        // 那个 `Account` 参数于是会认证第二遍:同一条请求打两次库,而且两次
        // 之间 token 若正好过期,前后还会不一致。
        //
        // **只在这一条请求内复用**,不跨请求缓存 —— 那就成了一份没人负责
        // 失效的会话副本。
        if let Some(account) =
            parts.extensions.get::<Self>()
        {
            return Ok(account.clone());
        }

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

        let account =
            account::authenticate(&mut conn, token)
                .await
                .map_err(|err| error::map_error(&err))?;
        parts.extensions.insert(account.clone());
        Ok(account)
    }
}
