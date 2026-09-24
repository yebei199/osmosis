//! 本机单调时钟的原始读数。
//!
//! 同步播放里所有「某一刻」都用它:输出后端报的呈现时刻、时间线的锚点、校时换算出来的本机时刻。
//! 不用 `std::time::Instant`:它不给出纪元,而安卓 AAudio 的时间戳是 `CLOCK_MONOTONIC`
//! 的绝对读数,得在同一个钟上比。

/// `CLOCK_MONOTONIC`,纳秒。
pub fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: 传入的是栈上一个有效的 timespec,clock_gettime 只往里写。
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts)
    };
    i64::from(ts.tv_sec) * 1_000_000_000
        + i64::from(ts.tv_nsec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单调:后读的不小于先读的,而且真的在走。
    #[test]
    fn it_moves_forward() {
        let first = monotonic_ns();
        std::thread::sleep(
            std::time::Duration::from_millis(2),
        );
        let second = monotonic_ns();
        assert!(
            second - first >= 1_000_000,
            "{first} → {second}"
        );
    }
}
