//! 播放页粒子场的**纯计算**:频段拆分与轨道几何。
//!
//! 刻意与 bevy 的 ECS 解耦:这里只回答「第 i 个粒子此刻在哪、多大」,把答案写进
//! Transform 的事留在 lib.rs 的 viz 模式里。纯函数才测得动 —— 视觉观感与穿卡效果
//! 走真实像素验收,数值行为(频段、呼吸、有界)在这里钉死。
//!
//! 轨道设计:粒子按金角序列铺在绕 [`crate::CARD_ANCHOR`] 的三层轨道壳上,轨道面
//! 大致水平(XZ 平面附近),相机从 +Z 侧看向原点 —— 每个粒子转一圈就从封面卡
//! 前面掠到后面再回来,这正是深度遮挡要的运动。低频撑轨道半径(呼吸),各壳
//! 绑定各自频段的电平撑缩放脉动,时间只推方位角与纵向浮动。

use bevy::math::Vec3;

/// 粒子总数。三层壳均分;数值取「桌面满帧无压力、android 真机可承受」的量级,
/// 真机发热读数出来后再调。
pub(crate) const PARTICLE_COUNT: usize = 219;

/// 频谱行按低/中/高拆出的三段电平,各自归一到 0..=1。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Levels {
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
}

/// 静音电平:三段全零。
impl Default for Levels {
    fn default() -> Self {
        Self {
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
        }
    }
}

/// 金角(弧度)。相邻下标的方位角差取它,序列自然铺满圆周不聚团 ——
/// 向日葵种子的那套排布。
const GOLDEN_ANGLE: f32 = 2.399_963;

/// 把 512 字节的频谱行拆成三段电平(各段取均值 / 255)。
///
/// 段界与 `audio::spectrum` 的 bin 布局对齐:低频挤在行首。长度不足 512 视为
/// 坏载荷,给静音电平 —— 载荷长度是跨 crate 的外部输入,坏了不 panic。
pub(crate) fn band_levels(spectrum: &[u8]) -> Levels {
    if spectrum.len() < 512 {
        return Levels::default();
    }
    let average = |range: core::ops::Range<usize>| {
        let len = range.len() as f32;
        spectrum[range]
            .iter()
            .map(|v| f32::from(*v))
            .sum::<f32>()
            / (len * 255.0)
    };
    Levels {
        bass: average(0..32),
        mid: average(32..160),
        treble: average(160..512),
    }
}

/// 第 `i` 个粒子在 `time` 时刻的 (位置, 缩放)。
///
/// 电平在入口消毒(非有限按 0、越界 clamp);半径/纵向/缩放各有硬上限,
/// 坏一帧数据也不许把场景炸飞。壳的角速度内快外慢,借开普勒的观感。
pub(crate) fn particle_pose(
    i: usize,
    time: f32,
    levels: &Levels,
) -> (Vec3, f32) {
    let shell = i % 3;
    let bass = sanitize(levels.bass);
    let level = sanitize(match shell {
        0 => levels.bass,
        1 => levels.mid,
        _ => levels.treble,
    });

    // 每壳的基准半径 / 角速度 / 基准缩放。
    let base_r = [1.7f32, 2.4, 3.1][shell];
    let omega = [0.55f32, 0.38, 0.26][shell];
    let base_s = [0.055f32, 0.045, 0.035][shell];

    // 低频呼吸:半径最多外扩 30%。
    let r = base_r * (1.0 + 0.30 * bass);
    // 金角铺开的初始方位角,时间只往前推角度。
    let az = i as f32 * GOLDEN_ANGLE + time * omega;
    // 纵向:每粒子一条确定性的基准高度(伪随机自下标),叠慢速浮动。
    let lane = ((i * 53 % 100) as f32 / 100.0 - 0.5) * 1.4;
    let bob = 0.18 * (time * 0.7 + i as f32 * 0.61).sin();

    let pos =
        Vec3::new(r * az.cos(), lane + bob, r * az.sin());
    // 分段脉动:静音停在基准值,电平顶满时放大约 2.2 倍。
    let scale = base_s * (1.0 + 1.2 * level);
    (pos, scale)
}

/// 电平消毒:非有限值按 0,越界 clamp 进 0..=1 —— 输入是跨 crate 的外部数据,
/// `clamp` 对 NaN 无效,得单独拦。
fn sanitize(level: f32) -> f32 {
    if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    // .xz() 这类分量选择是 Vec3Swizzles 的 trait 方法,须在作用域内。
    use bevy::math::Vec3Swizzles as _;
    use similar_asserts::assert_eq;

    use super::*;

    /// 频段拆分:低段全 255、中段全 128、高段全 0 的合成行,三段均值各归其位。
    #[test]
    fn band_levels_average_the_three_ranges() {
        let mut spectrum = [0u8; 512];
        spectrum[..32].fill(255);
        spectrum[32..160].fill(128);
        let l = band_levels(&spectrum);
        assert!(
            (l.bass - 1.0).abs() < 1e-3,
            "低段 {}",
            l.bass
        );
        assert!(
            (l.mid - 128.0 / 255.0).abs() < 1e-3,
            "中段 {}",
            l.mid
        );
        assert_eq!(l.treble, 0.0);
    }

    /// 频谱行长度不足(空/短):三段全给 0,不 panic —— 载荷长度是外部输入。
    #[test]
    fn short_spectrum_yields_zero_levels_not_panic() {
        assert_eq!(band_levels(&[]), Levels::default());
        assert_eq!(
            band_levels(&[255u8; 10]),
            Levels::default()
        );
    }

    /// 恒定电平下,横向轨道半径不随时间变:时间只推方位角与纵向浮动。
    #[test]
    fn time_moves_azimuth_not_radius() {
        let levels = Levels {
            bass: 0.6,
            mid: 0.3,
            treble: 0.9,
        };
        for i in [0usize, 7, 100, PARTICLE_COUNT - 1] {
            let (p0, _) = particle_pose(i, 0.0, &levels);
            let (p1, _) = particle_pose(i, 5.0, &levels);
            let r0 = p0.xz().length();
            let r1 = p1.xz().length();
            assert!(
                (r0 - r1).abs() < 1e-3,
                "粒子 {i} 半径漂了: {r0} -> {r1}"
            );
            assert!(
                p0.xz() != p1.xz(),
                "粒子 {i} 的方位角没在走"
            );
        }
    }

    /// 低频 0→1,轨道半径单调外扩 ——「呼吸」的最小断言。
    #[test]
    fn bass_expands_the_orbit() {
        let quiet = Levels::default();
        let loud = Levels {
            bass: 1.0,
            ..Levels::default()
        };
        for i in [0usize, 42, PARTICLE_COUNT - 1] {
            let (pq, _) = particle_pose(i, 1.0, &quiet);
            let (pl, _) = particle_pose(i, 1.0, &loud);
            assert!(
                pl.xz().length() > pq.xz().length(),
                "粒子 {i} 没随低频外扩"
            );
        }
    }

    /// 静音时缩放停在基准值,粒子不消失。
    #[test]
    fn silence_keeps_particles_visible() {
        for i in 0..PARTICLE_COUNT {
            let (_, s) =
                particle_pose(i, 3.0, &Levels::default());
            assert!(
                s > 0.01,
                "粒子 {i} 静音时缩放 {s} 近乎消失"
            );
        }
    }

    /// 任意电平∈[0,1]、任意时间(含很大的值):位置与缩放有限且有界。
    #[test]
    fn positions_and_scales_stay_bounded() {
        let extremes = [
            Levels::default(),
            Levels {
                bass: 1.0,
                mid: 1.0,
                treble: 1.0,
            },
        ];
        for levels in &extremes {
            for time in [0.0f32, 1.0, 3600.0, 86_400.0] {
                for i in 0..PARTICLE_COUNT {
                    let (p, s) =
                        particle_pose(i, time, levels);
                    assert!(
                        p.is_finite() && s.is_finite(),
                        "粒子 {i} 出了 NaN/Inf"
                    );
                    assert!(
                        p.length() < 6.0,
                        "粒子 {i} 飞出场景: {p}"
                    );
                    assert!(
                        (0.01..0.5).contains(&s),
                        "粒子 {i} 缩放越界: {s}"
                    );
                }
            }
        }
    }
}
