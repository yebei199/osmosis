//! 换歌时的封面过渡:颜色渐变与一次脉冲,各自独立计时。

/// 颜色从旧封面走到新封面要多久,秒。
const COLOR_FADE_SECS: f32 = 0.9;

/// 换歌脉冲从满到零要多久,秒。比颜色渐变短:脉冲是「一下」,渐变是「一段」。
const BURST_SECS: f32 = 0.55;

/// 换歌后的过渡:同一个计时器派生出「颜色渐变」与「burst 脉冲」两条曲线。
///
/// 原版是两个各自衰减的 uniform(`uColorMixT` / `uBurstAmt`),这里合成一个 ——
/// 它们永远同时开始,分开存只是多一个会走散的状态。
pub(crate) struct TrackTransition {
    /// 距上次换歌的秒数。初值取一个已经跑完的值:首曲没有「上一首」可渐变。
    elapsed: f32,
}

/// 初始状态 = 过渡早已结束:`color_mix` 为 1、`burst` 为 0。
impl Default for TrackTransition {
    fn default() -> Self {
        Self {
            elapsed: COLOR_FADE_SECS,
        }
    }
}

impl TrackTransition {
    /// 换歌:计时归零,旧封面开始向新封面过渡,同时起一发 burst。
    pub(crate) fn start(&mut self) {
        self.elapsed = 0.0;
    }

    /// 按播放页时钟推进 `dt` 秒。
    ///
    /// `dt` 是两帧时间戳相减来的,非有限或为负时当作没走 —— 时钟可以被门冻结、
    /// 可以被系统改,但过渡不能因此倒着走。计满即停,不让计数器一直涨。
    pub(crate) fn advance(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.elapsed =
            (self.elapsed + dt).min(COLOR_FADE_SECS);
    }

    /// 新旧封面的混合进度:0 = 全旧,1 = 全新。
    pub(crate) fn color_mix(&self) -> f32 {
        (self.elapsed / COLOR_FADE_SECS).clamp(0.0, 1.0)
    }

    /// burst 脉冲强度:换歌瞬间 1,随后衰减到 0。
    ///
    /// 平方衰减而不是线性 —— 线性的尾巴拖着不走,观感上像粒子回不到位。
    pub(crate) fn burst(&self) -> f32 {
        let left = (1.0 - self.elapsed / BURST_SECS)
            .clamp(0.0, 1.0);
        left * left
    }
}

#[cfg(test)]
mod tests;
