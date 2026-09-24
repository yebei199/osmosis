/// 会话:登录之后拿到的 token,以及它的落盘。
///
/// token 归本 crate 而不是 `app-core`:它是「怎么发请求」的一部分,
/// 而客户端领域按 `CONTEXT.md` 不认识网络。
use std::sync::RwLock;

/// 当前会话的 token。没登录时是 `None`。
///
/// 全局可变状态在这里是恰当的:一个进程只有一个登录态,
/// 而每一次请求都要用到它 —— 层层传递只会让每个函数都多一个参数。
static TOKEN: RwLock<Option<String>> = RwLock::new(None);

/// 串行化每一条会碰全局 token 的测试。
///
/// 它是进程级状态,而单元测试默认并行:两条测试各自 set 一个 token,断言到的
/// 会是对方那个,而且是偶发的。落盘处也一样 —— 拿着这把锁才好安全地把
/// `OSMOSIS_SESSION_FILE` 指到临时目录去,免得动到真实的那一份。
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> =
    std::sync::Mutex::new(());

/// 当前 token 的副本。
pub fn token() -> Option<String> {
    TOKEN.read().ok().and_then(|slot| slot.clone())
}

/// 记住一个 token(登录成功后),并落盘。
///
/// 列表缓存同时清空:换的可能是另一个账号,上一个人的歌单不该先画出来。
pub fn set(token: &str) {
    if let Ok(mut slot) = TOKEN.write() {
        *slot = Some(token.to_owned());
    }
    super::platform::save_session(Some(token));
    super::cache::forget_all();
}

/// 忘掉 token(登出),并清掉落盘的那份与列表缓存。
pub fn clear() {
    if let Ok(mut slot) = TOKEN.write() {
        *slot = None;
    }
    super::platform::save_session(None);
    super::cache::forget_all();
}

/// 服务端说 token 无效时的判据:这次带的 `sent` 仍是当前会话的 token,才算会话失效(#131)。
/// 判了就忘掉它,落盘那份先挪成 `session.bak` 再清(#127)—— 与 [`clear`] 不同,
/// 删会话不可逆,判错一次就得重登,留一份才查得回来。
///
/// 比较与清除在同一把写锁里做:比完放锁再清的话,中间登上的新会话会被一并清掉。
/// HTTP 与同播信令都走这里,判据只有一份。返回是否确实判了失效。
pub fn expire_if_current(sent: Option<&str>) -> bool {
    let Ok(mut slot) = TOKEN.write() else {
        return false;
    };
    if sent.is_none() || slot.as_deref() != sent {
        return false;
    }
    *slot = None;
    super::platform::backup_session();
    true
}

/// 把一次失败按这次请求带的 token 归类:当前 token 被拒就判会话失效,原样返回;
/// 没带或已被换掉的 token 被拒改报 [`crate::ApiError::Unauthenticated`],会话不动。
pub(crate) fn on_rejected(
    error: crate::ApiError,
    sent: Option<&str>,
) -> crate::ApiError {
    match error {
        crate::ApiError::Server { code, message }
            if code == TOKEN_REJECTED
                && !expire_if_current(sent) =>
        {
            crate::ApiError::Unauthenticated(message)
        }
        other => other,
    }
}

/// 服务端判 token 无效时给的 code。
const TOKEN_REJECTED: &str = "unauthorized";

/// 从落盘处恢复上次的登录态。各端入口在启动时调一次。
///
/// 恢复出来的 token 可能已经被服务端吊销 —— 那不是这里能知道的事,
/// 第一次带着它请求时会得到 401,界面据此回到登录页。
pub fn restore() {
    if let Some(saved) = super::platform::load_session()
        && let Ok(mut slot) = TOKEN.write()
    {
        *slot = Some(saved);
    }
}

/// 这台设备的 id,没有就用 `fresh` 现算一个存下来。
///
/// 与 token 同一个目录、同一条规矩,但**登出不删**:它标的是这台机器,
/// 不是这次登录。落盘的理由见调用方(`ui::syncplay`):遥控器重连按 id 认人。
///
/// 存不下来只是下次再换一个 —— 与设置同一条,不该把启动拦在门外。
pub fn device_id(fresh: impl FnOnce() -> String) -> String {
    if let Some(saved) = super::platform::load_device() {
        return saved;
    }

    let fresh = fresh();
    super::platform::save_device(&fresh);
    fresh
}

#[cfg(test)]
mod tests;
