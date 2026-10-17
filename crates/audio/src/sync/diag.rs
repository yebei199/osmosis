//! 同步源的诊断日志(#145):对齐处的 Skip、Seek 与欠载。
//!
//! 这几种事件每块都可能发生，照写会刷屏，所以每类一秒最多一行，那一行带上累计次数与其间
//! 省掉的条数。
//!
//! 事件在声卡回调线程上发生，但日志不在那里写：写日志要拿 logger 的锁、可能写文件，实时线程
//! 不能等这些(#192)。这里只产出 [`Note`],由 `SyncShared` 排进有界队列，持流线程取走再写。

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

/// 跳的是哪一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    Skip,
    Seek,
    Wait,
}

/// 为什么跳：刚开始跟、跟着跟着偏了、还是不出声时定位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// 这条时间线上第一块，起点就差得远。
    Entry,
    /// 已经在跟，误差超过了 `JUMP_NS`。
    Drift,
    /// 暂停着或还没起播，停的位置不对。
    Hold,
}

/// 一条待写的诊断。全是定长数据，排队时不分配(跳不动那条除外，见 [`Note::SeekFailed`])。
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    Jump {
        kind: Jump,
        why: Why,
        err_ns: i64,
        offset_step_ns: i64,
        /// 距上一次欠载结束多少毫秒，没欠载过是 `None`。
        since_starve_ms: Option<u128>,
        total: u64,
        suppressed: u64,
    },
    Starved {
        gap_ns: i64,
        media_ns: i64,
        aligning: bool,
        total: u64,
        suppressed: u64,
    },
    /// 跳不动。错误文本要分配一次：这条只在曲尾跳转失败时出现，不在每块的常规路径上。
    SeekFailed { error: String, backwards: bool },
}

impl core::fmt::Display for Note {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            Self::Jump {
                kind,
                why,
                err_ns,
                offset_step_ns,
                since_starve_ms,
                total,
                suppressed,
            } => {
                let since = since_starve_ms.map_or_else(
                    || "无".to_owned(),
                    |ms| format!("{ms}ms"),
                );
                write!(
                    f,
                    "同步源 {kind:?}: 误差 {:.1}ms, 原因 {why:?}, 锚点这一步挪了 {:.1}ms, \
                     距上次欠载 {since}, 累计 {total} 次, 省略 {suppressed} 条",
                    ms(*err_ns),
                    ms(*offset_step_ns),
                )
            }
            Self::Starved {
                gap_ns,
                media_ns,
                aligning,
                total,
                suppressed,
            } => write!(
                f,
                "同步源 Starved: 断流 {:.1}ms, 媒体 {:.0}ms 处, 对齐中 {aligning}, \
                 累计 {total} 次, 省略 {suppressed} 条",
                ms(*gap_ns),
                ms(*media_ns),
            ),
            Self::SeekFailed { error, backwards } => {
                write!(
                    f,
                    "同步源 Seek 失败({error}),改为{}",
                    if *backwards {
                        "静音等着"
                    } else {
                        "丢帧"
                    }
                )
            }
        }
    }
}

/// 同步源的诊断状态。
#[derive(Debug, Default)]
pub struct Diag {
    skip: Throttle,
    seek: Throttle,
    wait: Throttle,
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

    /// 距上一次欠载结束多少毫秒，没欠载过给 `None`。
    fn since_starve(&self, now: Instant) -> Option<u128> {
        self.last_starve_end
            .map(|at| (now - at).as_millis())
    }

    /// 跳了一次。`err_ns` 是跳之前的误差。该写一行时返回那一行。
    pub fn jump(
        &mut self,
        kind: Jump,
        why: Why,
        err_ns: i64,
    ) -> Option<Note> {
        let now = Instant::now();
        let throttle = match kind {
            Jump::Skip => &mut self.skip,
            Jump::Seek => &mut self.seek,
            Jump::Wait => &mut self.wait,
        };
        let (total, suppressed) = throttle.hit(now)?;
        Some(Note::Jump {
            kind,
            why,
            err_ns,
            offset_step_ns: self.offset_step_ns,
            since_starve_ms: self.since_starve(now),
            total,
            suppressed,
        })
    }

    /// 一段欠载结束了：断了多久，当时是不是在对齐中(刚跳过、还静音着)。该写一行时返回那一行。
    pub fn starved(
        &mut self,
        gap_ns: i64,
        media_ns: i64,
        aligning: bool,
    ) -> Option<Note> {
        let now = Instant::now();
        self.last_starve_end = Some(now);
        let (total, suppressed) = self.starve.hit(now)?;
        Some(Note::Starved {
            gap_ns,
            media_ns,
            aligning,
            total,
            suppressed,
        })
    }
}
