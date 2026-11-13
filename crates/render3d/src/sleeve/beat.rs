//! 封面实物跟着音乐的那几样:鼓点那一下、随响度放大的摆幅、高频的一闪。
//!
//! 三样各管一段频率,互不重叠:低频的**起音**(不是电平本身)给封套一下往前的推力与
//! 一点点头;中频平滑过好几秒的响度只放大呼吸的摆幅;高频的起音只碰光,不碰几何。
//! 没有一样是逐帧跟着电平走的 —— 那会让一张实物封套持续地抖,看着像坏了。
//! 起音是离散的事件,事件之间封套是静的(除了呼吸)。
//!
//! 同 [`super::motion`]:纯状态机,输入是 (状态, 电平, dt),时间按播放页时钟走,
//! 不碰 ECS。

// ── 起音检测 ──────────────────────────────────────────────────────────

/// 一路起音检测的参数。
#[derive(Clone, Copy, Debug)]
pub(crate) struct OnsetTuning {
    /// 「最近的平均电平」的时间常数,秒。起音是相对它说的:同样 0.5 的低频,
    /// 从安静里冒出来是一下鼓,在一段持续的低音里只是背景。
    pub average_secs: f32,
    /// 电平要高过平均多少倍才算起音。
    pub ratio: f32,
    /// 并且要高过平均至少这么多(电平单位)。平均接近 0 时倍数说明不了什么,
    /// 底噪从 0.01 跳到 0.03 也是三倍。
    pub rise: f32,
    /// 电平本身的下限:再安静的段落里冒出来的一点点不算鼓点。
    pub floor: f32,
    /// 两次起音至少隔多久,秒。一下鼓的能量会在几帧里连涨,只该算一次。
    pub refractory: f32,
    /// 高出平均这么多算满强度(电平单位),强度按它线性折算并封顶为 1。
    pub full_rise: f32,
}

/// 低频起音:一下底鼓。平均的时间常数 0.4s —— 短了一段持续的低音也会被当成一串鼓,
/// 长了一首歌从前奏进主歌时整段都是「起音」。不应期 0.12s 够分开十六分音符以外的一切。
pub(crate) const BASS_ONSET: OnsetTuning = OnsetTuning {
    average_secs: 0.4,
    ratio: 1.4,
    rise: 0.06,
    floor: 0.12,
    refractory: 0.12,
    full_rise: 0.30,
};

/// 高频起音:镲片、拍手。高频的电平整体比低频低(均值摊在三百多个 bin 上),
/// 门槛也低一档;平均跟得更快,镲片的尾巴不会把下一下压住。
pub(crate) const TREBLE_ONSET: OnsetTuning = OnsetTuning {
    average_secs: 0.25,
    ratio: 1.5,
    rise: 0.04,
    floor: 0.05,
    refractory: 0.08,
    full_rise: 0.20,
};

/// 一路电平的起音检测:电平明显高过它自己最近的平均,就是一次起音。
///
/// 边沿触发:触发之后要等电平先落回门槛以下才重新上膛。一段持续的低音会把平均
/// 抬上来,自己就落回门槛以下了;但如果没有这一步,电平缓缓爬坡时每过一个不应期
/// 就会再触发一次。
pub(crate) struct Onset {
    tuning: OnsetTuning,
    average: f32,
    /// 距上次起音的秒数。
    since: f32,
    /// 落回过门槛以下,可以再触发。
    armed: bool,
    /// 平均已经有过第一份样本。重置之后的第一帧只用来立平均,不触发 ——
    /// 否则从 0 起算的平均会让任何一点声音都算起音。
    primed: bool,
}

impl Onset {
    pub(crate) fn new(tuning: OnsetTuning) -> Self {
        Self {
            tuning,
            average: 0.0,
            since: 0.0,
            armed: true,
            primed: false,
        }
    }

    /// 回到刚建好的样子。封套不在显示时调:藏着的那段时间平均没在跟,
    /// 切回来拿一份过期的平均去比,第一帧就是一次假起音。
    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.tuning);
    }

    /// 喂这一帧的电平(0..=1),过了 `dt` 秒。返回这一帧起音的强度 0..=1,没有就是 0。
    ///
    /// 坏 `dt`(含 0:首帧、时钟定格)与坏电平当作没来,状态不动。
    pub(crate) fn detect(
        &mut self,
        level: f32,
        dt: f32,
    ) -> f32 {
        if !dt.is_finite()
            || dt <= 0.0
            || !level.is_finite()
        {
            return 0.0;
        }
        let level = level.clamp(0.0, 1.0);
        let tuning = self.tuning;
        if !self.primed {
            self.primed = true;
            self.average = level;
            self.since = tuning.refractory;
            return 0.0;
        }
        self.since += dt;

        // 先跟「这一帧之前」的平均比,再把这一帧并进去 —— 反过来的话,
        // 一下突起有一部分先被自己抬高的平均吃掉。
        let rise = level - self.average;
        let above = level > tuning.floor
            && rise > tuning.rise
            && level > self.average * tuning.ratio;
        let strength = if above
            && self.armed
            && self.since >= tuning.refractory
        {
            self.armed = false;
            self.since = 0.0;
            (rise / tuning.full_rise).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if !above {
            self.armed = true;
        }
        self.average += (level - self.average)
            * (1.0 - (-dt / tuning.average_secs).exp());
        strength
    }
}

// ── 鼓点那一下 ────────────────────────────────────────────────────────

/// 鼓点弹簧的固有角频率,rad/s。临界阻尼下峰值落在 1/ω ≈ 38ms,
/// 200ms 时只剩峰值的 8% 左右 —— 「咚」的一下,不是一段晃。
const KICK_OMEGA: f32 = 26.0;

/// 满强度那一下在峰值处把封套放大多少(2.5%)。在相机的距离上,放大 2.5% 就是
/// 往镜头靠近 2.5% 的距离 —— 读出来是「往前顶了一下」。用缩放而不是平移:平移量
/// 换算到画面要乘台面缩放,而台面缩放随视口变,同一下鼓在手机和桌面上就不一样大了。
pub(crate) const KICK_GROW: f32 = 0.025;

/// 满强度那一下的点头,弧度(约 1.3°)。正值是顶边朝人,与往下拖同向:
/// 往前顶时顶边先过来,像被从下面推了一把。
pub(crate) const KICK_NOD: f32 = 0.022;

/// 叠起来的推量上限,以满强度为 1。不应期内鼓点密的时候,前一下还没回去
/// 后一下又来了,不封顶就会越叠越大。
const KICK_CAP: f32 = 1.4;

/// 鼓点那一下:起音给一份冲量,临界阻尼的弹簧把它拉回原位。
///
/// 临界阻尼而不是拖动回弹那种欠阻尼:冲过头就是往后缩一下,一下鼓变成「咚 —— 嗒」,
/// 在密的鼓点上就是持续地抖。
///
/// 按解析解推进,不像 [`super::motion::Tilt`] 那样数值积分:这根弹簧硬(ω = 26),
/// 半隐式欧拉在 120Hz 的步长上会把峰值吃掉两三成,而且吃多少随帧率变 ——
/// 同一下鼓在 60 帧和 144 帧的屏上就不一样大了。临界阻尼有闭式解,直接用。
#[derive(Default)]
pub(crate) struct Kick {
    push: f32,
    rate: f32,
}

impl Kick {
    /// 来了一下强度 `strength`(0..=1)的鼓点。冲量按 ω·e 折算:临界阻尼的弹簧从 0
    /// 起跳,峰值是 冲量 / (ω·e),于是满强度那一下的峰值正好是 1。
    pub(crate) fn strike(&mut self, strength: f32) {
        if !strength.is_finite() || strength <= 0.0 {
            return;
        }
        self.rate += strength.min(1.0)
            * KICK_OMEGA
            * std::f32::consts::E;
    }

    /// 推进 `dt` 秒。坏 `dt` 当作没走。
    ///
    /// 临界阻尼 x'' = −ω²x − 2ωx' 的解:x(t) = (x₀ + (v₀ + ωx₀)t)·e^(−ωt),
    /// v(t) = (v₀ − ω(v₀ + ωx₀)t)·e^(−ωt)。步长多大都精确,不必切小步。
    pub(crate) fn step(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let w = KICK_OMEGA;
        let carry = self.rate + w * self.push;
        let decay = (-w * dt).exp();
        self.push = (self.push + carry * dt) * decay;
        self.rate = (self.rate - w * carry * dt) * decay;
        if self.push > KICK_CAP {
            self.push = KICK_CAP;
            self.rate = self.rate.min(0.0);
        }
        // 回到看不出的范围就归零,免得一份永远非零的残差每帧标脏。
        if self.push.abs() < 1e-4 && self.rate.abs() < 1e-3
        {
            self.push = 0.0;
            self.rate = 0.0;
        }
    }

    /// 当前推量,满强度一下的峰值为 1。乘 [`KICK_GROW`] / [`KICK_NOD`] 用。
    pub(crate) fn push(&self) -> f32 {
        self.push
    }
}

// ── 响度放大摆幅 ──────────────────────────────────────────────────────

/// 中频响度跟上来与落下去的时间常数,秒。起得慢、落得更慢:一段副歌要唱上一两秒
/// 摆幅才放开,唱完了再慢慢收回去 —— 摆幅跟的是段落,不是某一个字。
const SWAY_ATTACK_SECS: f32 = 0.8;
const SWAY_RELEASE_SECS: f32 = 2.5;

/// 平滑后的中频在这一段里从「安静」走到「响」,摆幅随之从 1 倍放到封顶。
const SWAY_QUIET: f32 = 0.10;
const SWAY_LOUD: f32 = 0.45;

/// 响的段落比呼吸多摆多少倍:封顶 2.2 倍,即左右约 ±6.6°、前后约 ±4.4°。
/// 再大周期 11s 的摆动就快到看得出在「晃」了。
pub(crate) const SWAY_BOOST: f32 = 1.2;

/// 平滑过的中频响度,放大呼吸的摆幅(见 [`super::motion::idle`])。
#[derive(Default)]
pub(crate) struct Sway {
    energy: f32,
}

impl Sway {
    /// 跟这一帧的中频电平。坏输入当作没来。
    pub(crate) fn follow(&mut self, mid: f32, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 || !mid.is_finite()
        {
            return;
        }
        let target = mid.clamp(0.0, 1.0);
        let secs = if target > self.energy {
            SWAY_ATTACK_SECS
        } else {
            SWAY_RELEASE_SECS
        };
        self.energy += (target - self.energy)
            * (1.0 - (-dt / secs).exp());
    }

    /// 摆幅倍数:安静时 1(就是原来的呼吸),响时最多 `1 + SWAY_BOOST`。
    pub(crate) fn amplitude(&self) -> f32 {
        let t = ((self.energy - SWAY_QUIET)
            / (SWAY_LOUD - SWAY_QUIET))
            .clamp(0.0, 1.0);
        1.0 + SWAY_BOOST * t * t * (3.0 - 2.0 * t)
    }
}

// ── 高频的一闪 ────────────────────────────────────────────────────────

/// 闪光衰减的时间常数,秒。起手是瞬时的,落下去约 0.3s 看不见 —— 「闪一下」,
/// 不是「亮一阵」。
const SPARK_DECAY_SECS: f32 = 0.10;

/// 高频起音点亮的那一闪,只进着色器的光泽(`sleeve.wgsl` 的 `spark`),不碰几何。
#[derive(Default)]
pub(crate) struct Spark {
    level: f32,
}

impl Spark {
    /// 来了一下强度 `strength` 的高频起音:亮到它,已经更亮就不动。
    pub(crate) fn strike(&mut self, strength: f32) {
        if strength.is_finite() {
            self.level =
                self.level.max(strength.clamp(0.0, 1.0));
        }
    }

    /// 推进 `dt` 秒。坏 `dt` 当作没走。
    pub(crate) fn step(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.level *= (-dt / SPARK_DECAY_SECS).exp();
        if self.level < 1e-3 {
            self.level = 0.0;
        }
    }

    pub(crate) fn level(&self) -> f32 {
        self.level
    }
}

#[cfg(test)]
mod tests;
