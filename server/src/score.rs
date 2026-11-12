//! 喜爱度(`docs/adr/0042`):一首歌在这个账号心里的位置,一条有界的轴,×1000 存成
//! `-1000..=1000` 的整数。
//!
//! 两层叠成:
//!
//! - **锚**:用户自己说的。踩 / 未表态 / 赞 / 特别喜欢,四档,取自 `track_feedback`。
//! - **修正**:听歌行为说的。每一次记下了听多久的播放按完播程度给一个 `[-1, 1]` 的值,
//!   按 90 天半衰期衰减后求和,再过一道 `tanh` 压进 `±0.2`。
//!
//! 修正的界严格小于锚的档距之半,所以**说过的话永远压得住听歌习惯**:赞过的歌怎么跳
//! 也掉不进「一般」,没表态的歌听多少遍也升不进「喜欢」—— 修正只在档内排先后。
//! 档的边界在客户端 `app_core::facets` 与服务端 `routes::catalog::radio::filter`,
//! 与这里的锚对齐(见 ADR)。
//!
//! 全是纯函数:数据从哪来归 [`crate::store::facets`],这里只管算。

/// 锚,×1000。踩比赞离零更远:踩是「别再给我放」,赞只是「不错」。
pub const LOVED: i16 = 800;
pub const LIKED: i16 = 400;
pub const DISLIKED: i16 = -600;

/// 修正的界,×1000。实际取值收在开区间里(见 [`correction`]),档与档不相接。
pub const CORRECTION_BOUND: i16 = 200;

/// 听不到这么久的一次播放不算信号:连按「下一首」找歌时路过的那几首,
/// 什么都没说明。
pub const NOISE_MS: i64 = 3_000;

/// 听到这一成算「放弃」(-1),听到这一成算「听完」(+1),中间线性。
/// 片尾常有几十秒的尾奏或静音,听到九成就当听完。
pub const ABANDONED_AT: f64 = 0.1;
pub const FINISHED_AT: f64 = 0.9;

/// 半衰期,天。一季之前的跳过只剩一半分量:口味会变,旧账不该永远算数。
pub const HALF_LIFE_DAYS: f64 = 90.0;

/// 饱和尺度:净完播(衰减后)达到这么多次时,修正到界的 `tanh(1) ≈ 76%`。
/// 播放次数再多也只是逼近界,不越过 —— 单曲循环一百遍不等于一个「特别喜欢」。
pub const SATURATION: f64 = 3.0;

/// 一次记下了听多久的播放。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listen {
    pub listened_ms: i64,
    pub duration_ms: i64,
    /// 起播离现在多少天。
    pub age_days: f64,
}

/// 用户表的态翻成锚。`track_feedback.verdict` 只有 `-1 / 1 / 2`,没有行是未表态。
#[must_use]
pub fn anchor(verdict: Option<i16>) -> i16 {
    match verdict {
        Some(v) if v >= 2 => LOVED,
        Some(v) if v > 0 => LIKED,
        Some(v) if v < 0 => DISLIKED,
        _ => 0,
    }
}

/// 一次播放说了什么:`[-1, 1]`。时长不明或听得太短的不算信号,是 `None`。
#[must_use]
pub fn listen_value(
    listened_ms: i64,
    duration_ms: i64,
) -> Option<f64> {
    if duration_ms <= 0 || listened_ms < NOISE_MS {
        return None;
    }
    // 往回拖、单曲里反复听时累计可能超过时长,按听完算
    let completion =
        (listened_ms as f64 / duration_ms as f64).min(1.0);
    let ramp = (completion - ABANDONED_AT)
        / (FINISHED_AT - ABANDONED_AT);
    Some((2.0 * ramp - 1.0).clamp(-1.0, 1.0))
}

/// 时间衰减的权重:今天是 1,90 天前是 ½。时钟偏差造成的负年龄按 0 算。
#[must_use]
pub fn decay(age_days: f64) -> f64 {
    0.5_f64.powf(age_days.max(0.0) / HALF_LIFE_DAYS)
}

/// 播放行为给的修正,×1000,落在 `(-200, 200)` 的开区间里。
///
/// 收进 `±199` 而不是 `±200`:`tanh` 本身碰不到 1,但取整会把 `199.6` 进成 200,
/// 那一下修正就和锚的档距之半相接了 —— 赞过的歌跌到正好 200,和没表态的顶上
/// 那首分不开。
#[must_use]
pub fn correction(
    listens: impl IntoIterator<Item = Listen>,
) -> i16 {
    let sum: f64 = listens
        .into_iter()
        .filter_map(|listen| {
            listen_value(
                listen.listened_ms,
                listen.duration_ms,
            )
            .map(|value| value * decay(listen.age_days))
        })
        .sum();
    let bound = f64::from(CORRECTION_BOUND);
    let raw = (bound * (sum / SATURATION).tanh()).round();
    let inner = bound - 1.0;
    // 已收进 ±199,转换不会截断
    raw.clamp(-inner, inner) as i16
}

/// 喜爱度,×1000:锚加修正,收进 `[-1000, 1000]`。
#[must_use]
pub fn score(
    verdict: Option<i16>,
    listens: impl IntoIterator<Item = Listen>,
) -> i16 {
    (anchor(verdict) + correction(listens))
        .clamp(-1000, 1000)
}

#[cfg(test)]
mod tests;
