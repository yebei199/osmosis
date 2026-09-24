//! 与服务端校时(#137 ⑤)。
//!
//! 播放组的共同计划写在**服务端**的单调时钟上;每台成员各自与服务端往返多次，估出
//! 「服务端时钟 − 本机时钟」,再把计划里的时刻换算到本机 `CLOCK_MONOTONIC` 上 —— 音频输出
//! 报的呈现时刻用的就是这个钟。两台设备的时钟读数从不直接相减，系统时钟也不动。
//!
//! 机制照搬 ② 的原型(`experiments/synctest/src/clock.rs`,实测两台设备稳态 ±2ms):
//! 排队与调度只会让往返变长，所以 RTT 明显偏大的样本不进拟合;两台机器的晶振差几十 ppm,
//! 几分钟就是毫秒级，所以拟合的是一条随本机时间变化的直线，不是一个平均数。

use std::collections::VecDeque;

/// 拟合窗口：半秒一次往返，留一分钟。
const WINDOW: usize = 120;

/// RTT 超过最短那次多少就不进拟合:最短的一半再加 200µs(同原型)。
fn too_slow(rtt: i64, min_rtt: i64) -> bool {
    rtt > min_rtt + min_rtt / 2 + 200_000
}

/// 本机 `CLOCK_MONOTONIC`,纳秒。与 `audio::clock::monotonic_ns` 是同一个钟 ——
/// 两个 crate 互不依赖，各读一次。
pub fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: 传入的是栈上一个有效的 timespec。
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts)
    };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// 一次往返的结论。`offset` = 服务端时钟 − 本机时钟(纳秒，在往返中点那一刻)。
#[derive(Clone, Copy, Debug)]
struct Sample {
    local_mid: i64,
    offset: i64,
    rtt: i64,
}

/// 校时的结论。服务端每次启动换一个纪元，纪元一换，之前的样本全部作废。
#[derive(Debug, Default)]
pub struct Clock {
    epoch: Option<u64>,
    samples: VecDeque<Sample>,
}

impl Clock {
    /// 记一次往返：本机 `sent_ns` 发出、`received_ns` 收到，服务端答的是 `server_us`。
    pub fn add(
        &mut self,
        epoch: u64,
        sent_ns: i64,
        server_us: u64,
        received_ns: i64,
    ) {
        if received_ns < sent_ns {
            return;
        }
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.samples.clear();
        }
        let local_mid =
            sent_ns + (received_ns - sent_ns) / 2;
        self.samples.push_back(Sample {
            local_mid,
            offset: server_us as i64 * 1_000 - local_mid,
            rtt: received_ns - sent_ns,
        });
        while self.samples.len() > WINDOW {
            self.samples.pop_front();
        }
    }

    /// 样本在哪个纪元上。计划的纪元对不上就不能换算。
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
    }

    /// 本机时刻 `local_ns` 上的偏移估计(纳秒)。
    fn offset_at(&self, local_ns: i64) -> Option<f64> {
        let min_rtt =
            self.samples.iter().map(|s| s.rtt).min()?;
        let good: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|s| !too_slow(s.rtt, min_rtt))
            .collect();
        if good.len() < 3 {
            let best = self
                .samples
                .iter()
                .min_by_key(|s| s.rtt)?;
            return Some(best.offset as f64);
        }
        let x0 = good[0].local_mid as f64;
        let n = good.len() as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) =
            (0.0, 0.0, 0.0, 0.0);
        for s in &good {
            let x = s.local_mid as f64 - x0;
            let y = s.offset as f64;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let denom = n * sxx - sx * sx;
        if denom.abs() < 1.0 {
            return Some(sy / n);
        }
        let slope = (n * sxy - sx * sy) / denom;
        let intercept = (sy - slope * sx) / n;
        Some(intercept + slope * (local_ns as f64 - x0))
    }

    /// 服务端纪元 `epoch` 上的时刻 `server_us` 换算成本机单调时钟(纳秒)。
    ///
    /// 纪元对不上、或者还一次往返都没有，给 `None`:照一个换算不了的时刻出声，比不出声更糟。
    pub fn to_local_ns(
        &self,
        epoch: u64,
        server_us: u64,
    ) -> Option<i64> {
        if self.epoch != Some(epoch) {
            return None;
        }
        let server_ns = server_us as i64 * 1_000;
        // 偏移本身随时间变：先粗换一次，再在那一刻重取偏移。
        let rough =
            server_ns - self.offset_at(server_ns)? as i64;
        Some(server_ns - self.offset_at(rough)? as i64)
    }

    /// 本机时刻换算成服务端时钟(微秒)。主端据此把「此刻」写进计划。
    pub fn to_server_us(
        &self,
        local_ns: i64,
    ) -> Option<u64> {
        let server_ns =
            local_ns + self.offset_at(local_ns)? as i64;
        u64::try_from(server_ns / 1_000).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;

    /// 往返对称时，偏移就是服务端读数减去往返中点。
    #[test]
    fn one_round_trip_gives_the_offset() {
        let mut clock = Clock::default();
        // 本机 10s 发、10.002s 收，服务端答 15.001s:偏移 +5s。
        clock.add(
            1,
            10 * S,
            15_001_000,
            10 * S + 2_000_000,
        );

        assert_eq!(
            clock.to_local_ns(1, 20_000_000),
            Some(15 * S)
        );
        assert_eq!(
            clock.to_server_us(15 * S),
            Some(20_000_000)
        );
    }

    /// 偏移线性漂移(50ppm)时，拟合要把漂移跟上，而不是只取平均。
    #[test]
    fn the_fit_follows_a_drifting_offset() {
        let mut clock = Clock::default();
        for i in 0..30 {
            let sent = i * S;
            let received = sent + 1_000_000;
            let mid = sent + 500_000;
            let offset = S + i * 50_000;
            clock.add(
                7,
                sent,
                ((mid + offset) / 1_000) as u64,
                received,
            );
        }
        // 本机 40s 那一刻偏移该是 1s + 2ms。
        let server_us =
            ((40 * S + S + 2_000_000) / 1_000) as u64;
        let local =
            clock.to_local_ns(7, server_us).unwrap();
        assert!((local - 40 * S).abs() < 5_000, "{local}");
    }

    /// RTT 特别大的往返是排队出来的，不许把拟合拉偏。
    #[test]
    fn slow_round_trips_are_left_out() {
        let mut clock = Clock::default();
        for i in 0..10 {
            let sent = i * S;
            clock.add(
                1,
                sent,
                ((sent + 500_000 + 5_000_000) / 1_000)
                    as u64,
                sent + S / 1_000,
            );
        }
        // 一次 80ms 的往返，服务端读数偏了 40ms。
        let sent = 10 * S;
        clock.add(
            1,
            sent,
            ((sent + 40_000_000 + 45_000_000) / 1_000)
                as u64,
            sent + 80_000_000,
        );

        let local = clock
            .to_local_ns(
                1,
                ((11 * S + 5_000_000) / 1_000) as u64,
            )
            .unwrap();
        assert!((local - 11 * S).abs() < 5_000, "{local}");
    }

    /// 服务端重启换了纪元：旧样本作废，旧纪元的时刻不再换算。
    #[test]
    fn a_new_epoch_drops_the_old_samples() {
        let mut clock = Clock::default();
        clock.add(1, 0, 5_000_000, 1_000_000);
        clock.add(2, 10 * S, 3_000_000, 10 * S + 2_000_000);

        assert_eq!(clock.epoch(), Some(2));
        assert_eq!(
            clock.to_local_ns(1, 6_000_000),
            None,
            "旧纪元的计划作废"
        );
        // 新纪元只剩那一次：偏移 3.000s − 10.001s。
        assert_eq!(
            clock.to_local_ns(2, 3_000_000),
            Some(10 * S + 1_000_000)
        );
    }

    /// 一次往返都没有时什么都换算不了;收到早于发出的(时钟错乱)不收。
    #[test]
    fn nothing_converts_without_a_sample() {
        let mut clock = Clock::default();
        assert_eq!(clock.to_local_ns(1, 1), None);
        clock.add(1, 10, 1, 5);
        assert_eq!(clock.epoch(), None);
    }
}
