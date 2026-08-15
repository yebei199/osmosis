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

/// 当前 token 的副本。
pub fn token() -> Option<String> {
    TOKEN.read().ok().and_then(|slot| slot.clone())
}

/// 记住一个 token(登录成功后),并落盘。
pub fn set(token: &str) {
    if let Ok(mut slot) = TOKEN.write() {
        *slot = Some(token.to_owned());
    }
    super::platform::save_session(Some(token));
}

/// 忘掉 token(登出),并清掉落盘的那份。
pub fn clear() {
    if let Ok(mut slot) = TOKEN.write() {
        *slot = None;
    }
    super::platform::save_session(None);
}

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

#[cfg(test)]
mod tests;
