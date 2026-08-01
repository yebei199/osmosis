//! 账号与会话:注册、登录、登出,以及 token 到账号的还原。
//!
//! 账号回答「数据归谁」—— 本地歌单、播放事件挂在它名下,网易云凭据按它分片
//! (见 `docs/adr/0017`)。它与「设备」正交:设备回答的是「同播里推给哪台」。
//!
//! 本模块不认识 HTTP。错误到状态码的映射在 [`crate::error`],鉴权提取器在
//! [`crate::auth`] —— 这里只有规则本身,可以脱离 axum 单独测。

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{
        SaltString,
        rand_core::{OsRng, RngCore},
    },
};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;

/// 会话 token 的字节数。32 字节 = 256 位,穷举不现实。
const TOKEN_BYTES: usize = 32;

/// 一个已认证的账号。
///
/// 只带调用方用得上的两个字段:`id` 是上游用户标识的来源,`username` 给人看。
/// 密码哈希不进这个结构 —— 它没有任何消费者,带着只会增加被打印出来的机会。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub username: String,
}

impl Account {
    /// 传给 bang-dream 的用户标识:它按这个键分片保存网易云凭据。
    ///
    /// 用主键的十进制串而不是用户名:用户名可以改,而凭据的归属不该跟着改名走。
    /// 它不是秘密,鉴权靠 token,不靠这个值猜不到。
    pub fn upstream_user_id(&self) -> String {
        self.id.to_string()
    }
}

/// 账号相关的失败。
///
/// 与 [`crate::error`] 里 gRPC 那侧同一个思路:类型只说**发生了什么**,
/// 状态码与错误码的映射集中在一处。
#[derive(Debug)]
pub enum AccountError {
    /// 邀请码不对。
    BadInvite,
    /// 用户名已被占用。
    UsernameTaken,
    /// 用户名或密码不对。
    ///
    /// **刻意不区分**这两种:分开回答等于把「这个用户名存在」白送给试探的人。
    BadCredentials,
    /// token 不认识,或已被登出。
    BadToken,
    /// 用户名或密码不满足最低要求。
    Invalid(&'static str),
    /// 数据库出错。
    Db(sqlx::Error),
}

impl From<sqlx::Error> for AccountError {
    fn from(err: sqlx::Error) -> Self {
        Self::Db(err)
    }
}

/// 注册一个账号。
///
/// `invite` 必须与部署时配置的邀请码一致 —— 服务面向公网,没有这道门任何人都能开户
/// (见 `docs/adr/0017`)。
pub async fn register(
    conn: &mut PgConnection,
    username: &str,
    password: &str,
    invite: &str,
    expected_invite: &str,
) -> Result<Account, AccountError> {
    if invite != expected_invite {
        return Err(AccountError::BadInvite);
    }

    let username = username.trim();
    if username.is_empty() {
        return Err(AccountError::Invalid(
            "用户名不能为空",
        ));
    }

    if password.len() < 8 {
        return Err(AccountError::Invalid(
            "密码至少 8 个字符",
        ));
    }

    let hash = hash_password(password)?;

    let row: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO accounts (username, password_hash)
         VALUES ($1, $2)
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .bind(username)
    .bind(&hash)
    .fetch_optional(conn)
    .await?;

    // ON CONFLICT DO NOTHING 时没有返回行 —— 唯一索引是 lower(username),
    // 所以 Alice 与 alice 会撞在一起,这正是要的。
    let (id,) = row.ok_or(AccountError::UsernameTaken)?;

    Ok(Account {
        id,
        username: username.to_owned(),
    })
}

/// 用用户名密码换一个会话 token。
///
/// 返回的是**明文** token,只在这一刻存在;库里只留它的哈希。
pub async fn login(
    conn: &mut PgConnection,
    username: &str,
    password: &str,
) -> Result<String, AccountError> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, password_hash FROM accounts
         WHERE lower(username) = lower($1)",
    )
    .bind(username.trim())
    .fetch_optional(&mut *conn)
    .await?;

    let Some((id, stored)) = row else {
        return Err(AccountError::BadCredentials);
    };

    if !verify_password(password, &stored) {
        return Err(AccountError::BadCredentials);
    }

    let token = new_token();

    sqlx::query(
        "INSERT INTO sessions (token_hash, account_id) VALUES ($1, $2)",
    )
    .bind(token_hash(&token))
    .bind(id)
    .execute(conn)
    .await?;

    Ok(token)
}

/// 由 token 还原出账号。
pub async fn authenticate(
    conn: &mut PgConnection,
    token: &str,
) -> Result<Account, AccountError> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT accounts.id, accounts.username
         FROM sessions JOIN accounts ON accounts.id = sessions.account_id
         WHERE sessions.token_hash = $1",
    )
    .bind(token_hash(token))
    .fetch_optional(conn)
    .await?;

    let (id, username) =
        row.ok_or(AccountError::BadToken)?;

    Ok(Account { id, username })
}

/// 吊销一个会话。
///
/// 只删这一条 —— 同账号在别处的 token 仍然有效。一并删掉的话,
/// 手机上登出会把桌面也踢下线,而那要等到真有两台设备时才会被发现。
pub async fn logout(
    conn: &mut PgConnection,
    token: &str,
) -> Result<(), AccountError> {
    sqlx::query(
        "DELETE FROM sessions WHERE token_hash = $1",
    )
    .bind(token_hash(token))
    .execute(conn)
    .await?;

    Ok(())
}

/// 把密码哈希成 argon2 的 PHC 串(自带随机盐与参数)。
fn hash_password(
    password: &str,
) -> Result<String, AccountError> {
    let salt = SaltString::generate(&mut OsRng);

    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AccountError::Invalid("密码无法哈希"))
}

/// 校验密码。哈希串解析不了时判为不匹配 —— 那是这一行数据坏了,
/// 无论如何都不该放人进来。
fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// 生成一个新的会话 token,十六进制。
fn new_token() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);

    to_hex(&bytes)
}

/// token 落库前的哈希。
///
/// 不用 argon2:token 是 256 位随机串,没有字典可查,慢哈希只是白白拖慢每次请求。
/// 这一步要挡的是「库泄露了,拿到的行能直接当会话用」。
fn token_hash(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 密码能对上自己的哈希。
    #[test]
    fn password_verifies_against_its_own_hash() {
        let hash = hash_password("correct horse").unwrap();

        assert!(verify_password("correct horse", &hash));
    }

    /// 错密码对不上。
    #[test]
    fn wrong_password_does_not_verify() {
        let hash = hash_password("correct horse").unwrap();

        assert!(!verify_password("battery staple", &hash));
    }

    /// 同一个密码两次哈希得到不同结果 —— 盐生效了。
    /// 相同就说明没加盐,一张彩虹表就能把全库还原。
    #[test]
    fn same_password_hashes_differently_each_time() {
        let first = hash_password("same").unwrap();
        let second = hash_password("same").unwrap();

        assert_ne!(first, second);
        assert!(verify_password("same", &first));
        assert!(verify_password("same", &second));
    }

    /// 两次签发的 token 不同,且长度是 32 字节的十六进制。
    #[test]
    fn session_tokens_are_unique() {
        let first = new_token();
        let second = new_token();

        assert_ne!(first, second);
        assert_eq!(first.len(), TOKEN_BYTES * 2);
    }

    /// 落库的是 token 的哈希,不是明文 —— 库泄露不该等于会话被接管。
    #[test]
    fn session_token_is_stored_hashed() {
        let token = new_token();
        let stored = token_hash(&token);

        assert_ne!(stored, token);
        assert!(!stored.contains(&token));
        // 同一个 token 每次算出同一个哈希,否则根本查不回来
        assert_eq!(stored, token_hash(&token));
    }
}
