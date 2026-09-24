//! 每块输出开头的那一次决定：照放、慢慢追、丢帧、seek,还是先不出声。
//!
//! 纯规则，不碰声卡也不碰媒体：给它时间线、这一块第一帧的呈现时刻和手上的读指针，
//! 它回一个 [`Decision`]。

use super::timeline::Target;

/// 误差超过这么多就不慢慢追了：起播、seek、欠载之后都会走到这里。
///
/// 10ms 是 #137 ② 实测过的门槛:±0.1% 的速率修正追 10ms 要 10 秒，再大就该跳。
pub const JUMP_NS: i64 = 10_000_000;

/// 跳过之后误差回到这以内才放出声。
///
/// 冻结合同是稳态 ±5ms(#137 ⑤),对齐判据取它的一小半：刚放出声的那一刻就该在合同里，
/// 而不是先响一段错位的声音再慢慢追进去。
pub const ALIGNED_NS: i64 = 2_000_000;

/// 速率修正的上限：±0.1%,音高上约 1.7 音分，听不出来。
pub const MAX_CORR: f64 = 0.001;

/// 小误差打算用多久追平。
pub const CONVERGE_NS: f64 = 2e9;

/// 落后多少以内靠往前丢帧追上;再远就 seek。
///
/// 丢帧只要把缓冲里的采样拉出来扔掉，比 seek 便宜得多(seek 可能要重开 range 请求)。
/// 通道里最多缓冲 5 秒(`BUFFER_SAMPLES`),丢 3 秒以内多半都在缓冲里。
pub const SKIP_MAX_NS: i64 = 3_000_000_000;

/// 这一块怎么放。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// 不出声，也不消耗媒体：暂停着，或者还没到起播。
    ///
    /// `lead_frames` 是离起播还差几帧，且起播那一刻就落在这一块里：先出这么多帧静音，
    /// 下一帧就是起播位置，此后按标称速率照放。
    Hold { lead_frames: Option<u64> },
    /// 往前丢这么多帧，然后照放(先静音，对齐了才出声)。
    Skip { frames: u64 },
    /// 跳到媒体的这一帧(先静音，对齐了才出声)。
    Seek { to_frames: f64 },
    /// 照放：每输出一帧，读指针走 `step` 帧。`muted` 表示还在对齐，照常消耗但不出声。
    Play { step: f64, muted: bool },
}

/// 跟随器的累计读数，给上报与日志用。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// 最近一块开头的误差：读指针减去该在的位置。正数表示放得比时间线靠后(超前)。
    pub last_err_ns: i64,
    pub corr: f64,
    pub skips: u64,
    pub seeks: u64,
}

/// 跟随器：记住自己是不是已经在跟、是不是还在对齐。
#[derive(Debug, Default)]
pub struct Follower {
    running: bool,
    aligning: bool,
    pub stats: Stats,
}

impl Follower {
    /// 这一块怎么放。
    ///
    /// - `present_ns`:这一块第一帧真正出声的本机时刻;
    /// - `ptr_frames`:手上下一帧是媒体的第几帧;
    /// - `rate`:媒体采样率;
    /// - `block_frames`:这一块大约多少帧，用来判断起播是不是落在这一块里。
    pub fn decide(
        &mut self,
        target: &Target,
        present_ns: i64,
        ptr_frames: f64,
        rate: f64,
        block_frames: u64,
    ) -> Decision {
        let _ = (target, present_ns, ptr_frames, rate, block_frames);
        todo!()
    }
}
