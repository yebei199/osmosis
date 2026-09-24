//! 共享时间轴:计划说「T 时刻媒体该在 P」,跟随器据此决定每一块音频从媒体的哪里取、取多快。

use serde::{Deserialize, Serialize};

/// 计划里的一段:从 `at_ns` 起,媒体从 `pos`(帧)开始按标称速率往前走。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub at_ns: i64,
    pub pos: f64,
}

/// 服务端发布的计划。时间用服务端时钟,客户端自己换算。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlanDto {
    pub version: u64,
    /// 两端必须放同一份媒体:对不上就拒绝执行,不猜。
    pub media_hash: u64,
    pub rate: u32,
    pub segments: Vec<Segment>,
    pub end_ns: i64,
}

/// `t` 时刻媒体该在哪一帧。还没到第一段、或者已经过了结束时刻,就是「不该出声」。
pub fn desired(segments: &[Segment], end_ns: i64, t: i64, rate: f64) -> Option<f64> {
    if t >= end_ns {
        return None;
    }
    let seg = segments.iter().rev().find(|s| s.at_ns <= t)?;
    Some(seg.pos + (t - seg.at_ns) as f64 * rate / 1e9)
}

/// 误差超过这么多就直接跳:起播、seek、重连之后都会走到这里。
pub const JUMP_S: f64 = 0.010;
/// 速率微调的上限:±0.1%,音高上约 1.7 音分,听不出来。
pub const MAX_CORR: f64 = 0.001;
/// 小误差打算用多久追平。
pub const CONVERGE_S: f64 = 2.0;

/// 跟随器:手上的媒体读指针,和当前的速率修正。
#[derive(Default, Debug, Clone)]
pub struct Follower {
    pub ptr: f64,
    pub corr: f64,
    pub jumps: u64,
    /// 上一块开头的误差(帧),正数 = 放得比计划靠后(超前)。
    pub last_err: f64,
    pub playing: bool,
}

impl Follower {
    /// 给出这一块第一帧该对应的媒体位置,返回这一块每输出一帧读指针走多少。
    /// `None` = 这一块放静音、读指针不动。`base_step` = 媒体采样率 / 设备采样率。
    pub fn plan_buffer(&mut self, want: Option<f64>, rate: f64, base_step: f64) -> Option<f64> {
        let Some(want) = want else {
            self.playing = false;
            return None;
        };
        let err = self.ptr - want;
        self.last_err = err;
        if !self.playing || err.abs() > JUMP_S * rate {
            self.ptr = want;
            self.corr = 0.0;
            self.jumps += 1;
            self.playing = true;
            self.last_err = 0.0;
            return Some(base_step);
        }
        self.corr = (-err / (rate * CONVERGE_S)).clamp(-MAX_CORR, MAX_CORR);
        Some(base_step * (1.0 + self.corr))
    }
}

/// 按读指针线性插值,从单声道媒体填一块交错的多声道输出。
pub fn render(media: &[f32], ptr: &mut f64, step: Option<f64>, out: &mut [f32], channels: usize) {
    let Some(step) = step else {
        out.fill(0.0);
        return;
    };
    for frame in out.chunks_mut(channels) {
        let i = ptr.floor();
        let frac = (*ptr - i) as f32;
        let i = i as isize;
        let at = |k: isize| {
            if k >= 0 && (k as usize) < media.len() {
                media[k as usize]
            } else {
                0.0
            }
        };
        let s = at(i) * (1.0 - frac) + at(i + 1) * frac;
        frame.fill(s);
        *ptr += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// 起播之前、结束之后都不出声;中间按段走。
    #[test]
    fn desired_follows_the_segments() {
        let segs = [
            Segment { at_ns: 1_000_000_000, pos: 0.0 },
            Segment { at_ns: 3_000_000_000, pos: 480_000.0 },
        ];
        let end = 5_000_000_000;
        assert_eq!(desired(&segs, end, 500_000_000, RATE), None);
        assert_eq!(desired(&segs, end, 2_000_000_000, RATE), Some(48_000.0));
        // seek 那一段:3s 时跳到媒体第 10 秒。
        assert_eq!(desired(&segs, end, 3_500_000_000, RATE), Some(504_000.0));
        assert_eq!(desired(&segs, end, 5_000_000_000, RATE), None);
    }

    /// 第一块直接跳到计划位置,不慢慢追。
    #[test]
    fn the_first_buffer_jumps() {
        let mut f = Follower::default();
        let step = f.plan_buffer(Some(12_345.0), RATE, 1.0);
        assert_eq!(step, Some(1.0));
        assert_eq!(f.ptr, 12_345.0);
        assert_eq!(f.jumps, 1);
    }

    /// 小误差走速率微调,方向对、且不超过上限;大误差直接跳。
    #[test]
    fn small_errors_nudge_the_rate_and_large_ones_jump() {
        let mut f = Follower::default();
        f.plan_buffer(Some(0.0), RATE, 1.0);
        // 超前 2ms:该放慢。
        f.ptr = 96.0;
        let step = f.plan_buffer(Some(0.0), RATE, 1.0).unwrap();
        assert!(step < 1.0 && step >= 1.0 - MAX_CORR, "{step}");
        // 落后 1 秒:跳。
        let jumps = f.jumps;
        f.plan_buffer(Some(48_000.0 + f.ptr), RATE, 1.0);
        assert_eq!(f.jumps, jumps + 1);
    }

    /// 计划停了就静音,读指针原地不动。
    #[test]
    fn no_plan_means_silence() {
        let media = vec![1.0; 100];
        let mut ptr = 10.0;
        let mut out = vec![0.5; 8];
        render(&media, &mut ptr, None, &mut out, 2);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(ptr, 10.0);
    }

    /// 插值取样、各声道同值、指针按步长前进。
    #[test]
    fn render_interpolates_and_advances() {
        let media: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let mut ptr = 2.5;
        let mut out = vec![0.0; 4];
        render(&media, &mut ptr, Some(1.0), &mut out, 2);
        assert_eq!(out, vec![2.5, 2.5, 3.5, 3.5]);
        assert_eq!(ptr, 4.5);
    }
}
