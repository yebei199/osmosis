//! 校时用的服务端时钟(#137 ⑤)。
//!
//! 多台设备一起出声，靠的是它们各自把「服务端时钟的某一刻」换算到自己的单调时钟上。
//! 所以这里给的是**单调**时钟(进程启动起算的微秒),不是挂钟 —— 挂钟会被 NTP 拨动。
//! 纪元在进程启动时定一次：服务端重启，旧的偏移估计与旧计划里的时刻全都作废，客户端凭
//! 纪元变了认出来。

use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Clock {
    start: Instant,
    epoch: u64,
}

static CLOCK: LazyLock<Clock> = LazyLock::new(|| Clock {
    start: Instant::now(),
    // 启动那一刻的挂钟纳秒:只要两次启动不在同一纳秒，纪元就不同。它不参与任何换算。
    epoch: SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |since| since.as_nanos() as u64),
});

/// 服务端单调时钟，微秒。
pub fn now_us() -> u64 {
    CLOCK.start.elapsed().as_micros() as u64
}

/// 这个时钟的纪元。
pub fn epoch() -> u64 {
    CLOCK.epoch
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单调往前走，纪元不变。
    #[test]
    fn the_clock_moves_and_the_epoch_holds() {
        let (first, epoch) = (now_us(), epoch());
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(now_us() >= first + 1_000);
        assert_eq!(super::epoch(), epoch);
    }
}
