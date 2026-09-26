//! 同步源的诊断日志(#145):对齐处的 Skip、Seek 与欠载。
//!
//! 这几种事件每块都可能发生，照写会刷屏，所以每类一秒最多一行，那一行带上累计次数与其间
//! 省掉的条数。日志在声卡回调线程里写。
// ponytail: 回调里直接 log,每类限一行每秒;真要零阻塞再改成无锁队列交给别的线程写。

use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(1);

/// 一类事件的限频器。
#[derive(Debug, Default)]
pub struct Throttle {
    last: Option<Instant>,
    total: u64,
    suppressed: u64,
}

impl Throttle {
    /// 记一次事件。该写一行时返回(累计次数，上一行之后省掉的条数)。
    pub fn hit(
        &mut self,
        now: Instant,
    ) -> Option<(u64, u64)> {
        self.total += 1;
        if self.last.is_some_and(|last| now < last + WINDOW)
        {
            self.suppressed += 1;
            return None;
        }
        self.last = Some(now);
        Some((
            self.total,
            core::mem::take(&mut self.suppressed),
        ))
    }
}

/// 为什么跳：刚开始跟、跟着跟着偏了、还是不出声时定位。
#[derive(Debug, Clone, Copy)]
pub enum Why {
    /// 这条时间线上第一块，起点就差得远。
    Entry,
    /// 已经在跟，误差超过了 `JUMP_NS`。
    Drift,
    /// 暂停着或还没起播，停的位置不对。
    Hold,
}

/// 同步源的诊断状态。
#[derive(Debug, Default)]
pub struct Diag {
    skip: Throttle,
    seek: Throttle,
    starve: Throttle,
    /// 上一块时间线的「媒体减本机时刻」:它变了，说明锚点动了(校时一步或服务端重发)。
    last_offset_ns: Option<i64>,
    /// 这一块相对上一块，锚点挪了多少。
    offset_step_ns: i64,
    last_starve_end: Option<Instant>,
}

fn ms(ns: i64) -> f64 {
    ns as f64 / 1e6
}

impl Diag {
    /// 每块记一次时间线的偏移，好在跳的那一行里说出锚点这一步挪了多少。
    pub fn block(&mut self, offset_ns: Option<i64>) {
        self.offset_step_ns =
            match (self.last_offset_ns, offset_ns) {
                (Some(last), Some(now)) => now - last,
                _ => 0,
            };
        self.last_offset_ns = offset_ns;
    }

    /// 距上一次欠载结束多久，没欠载过给 `None`。
    fn since_starve(&self, now: Instant) -> String {
        self.last_starve_end.map_or_else(
            || "无".into(),
            |at| format!("{}ms", (now - at).as_millis()),
        )
    }

    /// 跳了一次：`skip` 为真是往前丢帧，否则 seek。`err_ns` 是跳之前的误差。
    pub fn jump(
        &mut self,
        skip: bool,
        why: Why,
        err_ns: i64,
    ) {
        let now = Instant::now();
        let (kind, throttle) = if skip {
            ("Skip", &mut self.skip)
        } else {
            ("Seek", &mut self.seek)
        };
        let Some((total, suppressed)) = throttle.hit(now)
        else {
            return;
        };
        log::warn!(
            "同步源 {kind}: 误差 {:.1}ms, 原因 {why:?}, 锚点这一步挪了 {:.1}ms, \
             距上次欠载 {}, 累计 {total} 次, 省略 {suppressed} 条",
            ms(err_ns),
            ms(self.offset_step_ns),
            self.since_starve(now),
        );
    }

    /// 一段欠载结束了：断了多久，当时是不是在对齐中(刚跳过、还静音着)。
    pub fn starved(
        &mut self,
        gap_ns: i64,
        media_ns: i64,
        aligning: bool,
    ) {
        let now = Instant::now();
        self.last_starve_end = Some(now);
        let Some((total, suppressed)) =
            self.starve.hit(now)
        else {
            return;
        };
        log::warn!(
            "同步源 Starved: 断流 {:.1}ms, 媒体 {:.0}ms 处, 对齐中 {aligning}, \
             累计 {total} 次, 省略 {suppressed} 条",
            ms(gap_ns),
            ms(media_ns),
        );
    }
}
