//! 固定窗口限流:同一个键在一个窗口里最多放行几次。
//!
//! 登录与注册按来源 IP 限,信令建连按账号限。没有这道闸,一台机器就能
//! 对着 `/login` 穷举密码,或者反复建连把名册刷成一片噪声。
//!
//! 固定窗口而不是令牌桶:窗口边界上最坏能放行两倍的量,而这里防的是
//! 每秒几百次的自动化,不是精确配额。为这点精度引一个 crate 不值得。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 键的数量超过这个数就顺手清一遍过期条目。
///
/// IP 是攻击者能随意换的,不清的话这张表只涨不落。清理只在超过阈值时做,
/// 于是正常流量下一次都不会跑。
const PRUNE_ABOVE: usize = 4096;

/// 一个共享的限流器。
pub type SharedLimiter = Arc<Mutex<RateLimiter>>;

/// 按键计数的固定窗口限流器。
pub struct RateLimiter {
    window: Duration,
    seen: HashMap<String, (Instant, u32)>,
}

impl RateLimiter {
    /// 窗口长度由调用方给;每次 [`check`](Self::check) 各自带自己的配额。
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            seen: HashMap::new(),
        }
    }

    /// 记一次并回答放不放行。`allowance` 是这个键在一个窗口里的上限。
    ///
    /// 配额写在调用处而不是构造处:登录与建连的合理频率差一个数量级,
    /// 而它们共用一张表没有坏处 —— 键各自带着前缀。
    pub fn check(
        &mut self,
        key: &str,
        allowance: u32,
    ) -> bool {
        let now = Instant::now();

        if self.seen.len() > PRUNE_ABOVE {
            let window = self.window;
            self.seen.retain(|_, (started, _)| {
                now.duration_since(*started) < window
            });
        }

        let (started, count) = self
            .seen
            .entry(key.to_owned())
            .or_insert((now, 0));

        if now.duration_since(*started) >= self.window {
            *started = now;
            *count = 0;
        }

        *count += 1;
        *count <= allowance
    }
}

impl Default for RateLimiter {
    /// 一分钟一个窗口。
    fn default() -> Self {
        Self::new(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配额之内放行,超了就挡。
    #[test]
    fn allows_up_to_the_quota_then_blocks() {
        let mut limiter = RateLimiter::default();

        assert!(limiter.check("ip:1", 2));
        assert!(limiter.check("ip:1", 2));
        assert!(
            !limiter.check("ip:1", 2),
            "第三次该被挡下"
        );
    }

    /// 键之间互不影响 —— 一个人打满了不该把别人一起锁在门外。
    #[test]
    fn keys_are_counted_separately() {
        let mut limiter = RateLimiter::default();

        assert!(limiter.check("ip:1", 1));
        assert!(!limiter.check("ip:1", 1));
        assert!(
            limiter.check("ip:2", 1),
            "另一个键不该受影响"
        );
    }

    /// 窗口过去之后重新开始计。
    #[test]
    fn the_window_resets() {
        let mut limiter =
            RateLimiter::new(Duration::from_millis(30));

        assert!(limiter.check("ip:1", 1));
        assert!(!limiter.check("ip:1", 1));

        std::thread::sleep(Duration::from_millis(40));

        assert!(
            limiter.check("ip:1", 1),
            "新窗口里该重新放行"
        );
    }
}
