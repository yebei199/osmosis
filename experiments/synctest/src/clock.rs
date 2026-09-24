//! 单调时钟与校时:谁的时钟读数都不跨机器直接相减,只用往返估出的偏移去换算。

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::timeline::PlanDto;

/// 本机 CLOCK_MONOTONIC,纳秒。AAudio 的时间戳用的就是这个钟。
pub fn mono_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: 传入的是栈上一个有效的 timespec。
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// 线上的几种消息。JSON,一包一条。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t")]
pub enum Msg {
    Ping { t0: i64 },
    Pong { t0: i64, t1: i64, t2: i64 },
    GetPlan,
    Plan(PlanDto),
}

pub fn send(sock: &UdpSocket, to: SocketAddr, msg: &Msg) {
    if let Ok(bytes) = serde_json::to_vec(msg) {
        let _ = sock.send_to(&bytes, to);
    }
}

pub fn recv(sock: &UdpSocket) -> Option<(Msg, SocketAddr)> {
    let mut buf = [0u8; 65_000];
    let (n, from) = sock.recv_from(&mut buf).ok()?;
    serde_json::from_slice(&buf[..n]).ok().map(|m| (m, from))
}

/// 一次往返的结论。`offset` = 服务端时钟 − 本机时钟(在往返中点那一刻)。
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub local_mid: i64,
    pub offset: i64,
    pub rtt: i64,
}

/// 一簇 ping 里挑 RTT 最小的那一次:排队与调度只会让往返变长,最短的那次最接近对称。
pub fn burst(sock: &UdpSocket, server: SocketAddr, pings: usize) -> Option<Sample> {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let mut best: Option<Sample> = None;
    for _ in 0..pings {
        let t0 = mono_ns();
        send(sock, server, &Msg::Ping { t0 });
        // 可能先收到迟到的旧 Pong:认 t0 对得上的那一条。
        for _ in 0..4 {
            let Some((Msg::Pong { t0: echoed, t1, t2 }, _)) = recv(sock) else {
                continue;
            };
            if echoed != t0 {
                continue;
            }
            let t3 = mono_ns();
            let rtt = (t3 - t0) - (t2 - t1);
            let offset = ((t1 - t0) + (t2 - t3)) / 2;
            let sample = Sample {
                local_mid: (t0 + t3) / 2,
                offset,
                rtt,
            };
            if best.is_none_or(|b| sample.rtt < b.rtt) {
                best = Some(sample);
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    best
}

/// 最近若干簇的结论,拟合成「偏移随本机时间线性变化」:两台机器的晶振差几十 ppm,
/// 几分钟下来就是毫秒级,不跟踪漂移的话越放越偏。
#[derive(Default)]
pub struct Estimator {
    samples: VecDeque<Sample>,
}

/// 拟合窗口:一秒一簇,留一分钟。
const WINDOW: usize = 60;

impl Estimator {
    pub fn add(&mut self, sample: Sample) {
        self.samples.push_back(sample);
        while self.samples.len() > WINDOW {
            self.samples.pop_front();
        }
    }

    pub fn min_rtt(&self) -> Option<i64> {
        self.samples.iter().map(|s| s.rtt).min()
    }

    /// 本机时刻 `local` 上的偏移估计。RTT 明显偏大的簇不进拟合。
    pub fn offset_at(&self, local: i64) -> Option<f64> {
        let min_rtt = self.min_rtt()?;
        let good: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|s| s.rtt <= min_rtt + min_rtt / 2 + 200_000)
            .collect();
        if good.len() < 3 {
            let best = self.samples.iter().min_by_key(|s| s.rtt)?;
            return Some(best.offset as f64);
        }
        let x0 = good[0].local_mid as f64;
        let n = good.len() as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
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
        Some(intercept + slope * (local as f64 - x0))
    }

    /// 服务端时刻换算成本机时刻。偏移本身随时间变,所以先粗换一次再在那一刻重取偏移。
    pub fn to_local(&self, server: i64) -> Option<i64> {
        let rough = server - self.offset_at(server)? as i64;
        Some(server - self.offset_at(rough)? as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 偏移线性漂移时,拟合要把漂移跟上,而不是只取平均。
    #[test]
    fn the_estimator_follows_a_drifting_offset() {
        let mut est = Estimator::default();
        // 偏移从 1s 起,每秒漂 50µs(50ppm),RTT 恒 1ms。
        for i in 0..30 {
            est.add(Sample {
                local_mid: i * 1_000_000_000,
                offset: 1_000_000_000 + i * 50_000,
                rtt: 1_000_000,
            });
        }
        let at_40s = est.offset_at(40_000_000_000).unwrap();
        assert!((at_40s - 1_002_000_000.0).abs() < 1_000.0, "{at_40s}");
    }

    /// RTT 特别大的簇是排队出来的,不许拉偏拟合。
    #[test]
    fn slow_round_trips_are_left_out() {
        let mut est = Estimator::default();
        for i in 0..10 {
            est.add(Sample {
                local_mid: i * 1_000_000_000,
                offset: 5_000_000,
                rtt: 1_000_000,
            });
        }
        est.add(Sample {
            local_mid: 10_000_000_000,
            offset: 40_000_000,
            rtt: 80_000_000,
        });
        let offset = est.offset_at(10_000_000_000).unwrap();
        assert!((offset - 5_000_000.0).abs() < 1_000.0, "{offset}");
    }

    /// 换算是来回自洽的:服务端时刻换到本机再加回偏移,回到原处。
    #[test]
    fn to_local_inverts_the_offset() {
        let mut est = Estimator::default();
        for i in 0..5 {
            est.add(Sample {
                local_mid: i * 1_000_000_000,
                offset: -3_000_000,
                rtt: 500_000,
            });
        }
        assert_eq!(est.to_local(2_000_000_000), Some(2_003_000_000));
    }
}
