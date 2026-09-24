//! 两个音频后端共用的出声核心。后端只负责告诉它「这一块第一帧什么时候真的出声」。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::timeline::{Follower, Segment, desired, render};

/// 换算到本机时钟之后的计划。主线程写,音频线程读。
#[derive(Clone, Debug, Default)]
pub struct LocalPlan {
    pub segments: Vec<Segment>,
    pub end_ns: i64,
}

/// 音频线程报给主线程的状态,全是原子量,音频线程不拿锁。
#[derive(Default)]
pub struct Stats {
    /// 上一块开头的误差,单位纳秒(正 = 超前)。
    pub err_ns: AtomicI64,
    /// 速率修正,百万分之一。
    pub corr_ppm: AtomicI64,
    pub jumps: AtomicU64,
    pub callbacks: AtomicU64,
    /// 拿到真实呈现时间戳的块数 / 只能估算的块数。
    pub ts_ok: AtomicU64,
    pub ts_estimated: AtomicU64,
    /// 这一块从现在到出声还要多久,纳秒(= 输出延迟)。
    pub latency_ns: AtomicI64,
    pub playing: AtomicBool,
}

/// 共享给音频线程的那一份。
pub struct Shared {
    pub plan: Mutex<Option<LocalPlan>>,
    pub stats: Stats,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            plan: Mutex::new(None),
            stats: Stats::default(),
        })
    }
}

/// 活在音频线程里的那一半。
pub struct Core {
    media: Arc<Vec<f32>>,
    media_rate: f64,
    device_rate: f64,
    follower: Follower,
    plan: Option<LocalPlan>,
    shared: Arc<Shared>,
    /// 输出增益。0 = 代码路径照走、只是不出声,给用户不在场时的功能验证用。
    gain: f32,
}

impl Core {
    pub fn new(
        media: Arc<Vec<f32>>,
        media_rate: f64,
        device_rate: f64,
        shared: Arc<Shared>,
        gain: f32,
    ) -> Self {
        Self {
            media,
            media_rate,
            device_rate,
            follower: Follower::default(),
            plan: None,
            shared,
            gain,
        }
    }

    /// 填一块。`present_ns` = 这一块第一帧的出声时刻(本机单调钟),`now_ns` = 此刻,
    /// `measured` = 前者是真时间戳还是估的。
    pub fn fill(&mut self, out: &mut [f32], channels: usize, present_ns: i64, now_ns: i64, measured: bool) {
        // ponytail: 音频线程上 try_lock,拿不到就沿用上一份计划;真做产品要换无锁的交接。
        if let Ok(guard) = self.shared.plan.try_lock()
            && let Some(plan) = guard.as_ref()
        {
            self.plan = Some(plan.clone());
        }
        let want = self
            .plan
            .as_ref()
            .and_then(|p| desired(&p.segments, p.end_ns, present_ns, self.media_rate));
        let step = self
            .follower
            .plan_buffer(want, self.media_rate, self.media_rate / self.device_rate);
        render(&self.media, &mut self.follower.ptr, step, out, channels);
        if self.gain != 1.0 {
            out.iter_mut().for_each(|s| *s *= self.gain);
        }

        let s = &self.shared.stats;
        let err_ns = (self.follower.last_err / self.media_rate * 1e9) as i64;
        s.err_ns.store(err_ns, Ordering::Relaxed);
        s.corr_ppm.store((self.follower.corr * 1e6) as i64, Ordering::Relaxed);
        s.jumps.store(self.follower.jumps, Ordering::Relaxed);
        s.callbacks.fetch_add(1, Ordering::Relaxed);
        if measured {
            s.ts_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            s.ts_estimated.fetch_add(1, Ordering::Relaxed);
        }
        s.latency_ns.store(present_ns - now_ns, Ordering::Relaxed);
        s.playing.store(self.follower.playing, Ordering::Relaxed);
    }
}
