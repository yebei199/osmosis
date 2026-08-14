//! 开户、登录、登出 —— 三条都顺带维护会话 token。

use contract::{LoginDto, RegisterDto, SessionDto};

use crate::{ApiError, base_url, platform, session};

/// `POST /register` —— 凭邀请码开户,顺带拿到会话。
///
/// 成功即记住 token,调用方不必再单独调 [`session::set`] —— 那一步漏了的现象是
/// 「注册成功但接着全是 401」,而人会去查服务端。
pub async fn register(
    username: &str,
    password: &str,
    invite: &str,
) -> Result<SessionDto, ApiError> {
    let dto: SessionDto = platform::send_json(
        reqwest::Method::POST,
        format!("{}/register", base_url()),
        Some(RegisterDto {
            username: username.to_owned(),
            password: password.to_owned(),
            invite: invite.to_owned(),
        }),
    )
    .await?;

    session::set(&dto.token);

    Ok(dto)
}

/// `POST /login` —— 用用户名密码换会话,同样自动记住 token。
pub async fn login(
    username: &str,
    password: &str,
) -> Result<SessionDto, ApiError> {
    let dto: SessionDto = platform::send_json(
        reqwest::Method::POST,
        format!("{}/login", base_url()),
        Some(LoginDto {
            username: username.to_owned(),
            password: password.to_owned(),
        }),
    )
    .await?;

    session::set(&dto.token);

    Ok(dto)
}

/// `POST /logout` —— 吊销这一条会话。
///
/// 本地那份**无论服务端怎么答都要清掉**:请求失败时用户的意图仍然是登出,
/// 留着一个可能已失效的 token 只会让下一次操作莫名其妙地 401。
pub async fn logout() -> Result<(), ApiError> {
    let result = platform::send_no_content::<()>(
        reqwest::Method::POST,
        format!("{}/logout", base_url()),
        None,
    )
    .await;

    session::clear();

    result
}
