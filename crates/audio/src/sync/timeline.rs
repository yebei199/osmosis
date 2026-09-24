//! 时间线：某一刻媒体该在哪。
//!
//! 时间一律是**本机**单调时钟(`CLOCK_MONOTONIC`,纳秒)。跨机器的换算在校时那一层做完，
//! 交到这里的已经是本机时刻：这一层从不拿两台机器的时钟读数相减。

/// 时间线上的一个锚点:本机单调时钟 `at_ns` 这一刻，媒体该在 `media_ns`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub at_ns: i64,
    pub media_ns: i64,
}

/// 同步源要跟的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// 不跟时间线：顺序往下放(本机单独播放)。
    Free,
    /// 跟时间线。
    ///
    /// - `playing` 为假：停在锚点那个媒体位置，不出声、不消耗媒体;
    /// - `start_ns` 之前：还没到起播的那一刻，同样不出声。
    ///
    /// 锚点会随校时微调(每秒一次),`start_ns` 不跟着动：它是「一起响」的那一刻，
    /// 已经过去了就不再有意义。
    Follow {
        anchor: Anchor,
        playing: bool,
        start_ns: i64,
    },
}

impl Target {
    /// `present_ns` 这一刻媒体该在第几纳秒。`None` 表示这一刻不该出声(暂停着，或者还没到起播)。
    /// `Free` 没有「该在哪」,也给 `None`。
    pub fn desired(&self, present_ns: i64) -> Option<i64> {
        let _ = present_ns;
        todo!()
    }

    /// 离起播还有多少纳秒。已经开始、暂停着或者不跟时间线，都给 `None`。
    pub fn until_start(&self, present_ns: i64) -> Option<i64> {
        let _ = present_ns;
        todo!()
    }
}
