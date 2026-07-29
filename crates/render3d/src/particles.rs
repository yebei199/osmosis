//! 播放页粒子场的**纯计算**:频段拆分与轨道几何。
//!
//! 刻意与 bevy 的 ECS 解耦:这里只回答「第 i 个粒子此刻在哪、多大」,把答案写进
//! Transform 的事留在 lib.rs 的 viz 模式里。纯函数才测得动 —— 视觉观感与穿卡效果
//! 走真实像素验收,数值行为(频段、呼吸、有界)在这里钉死。
//!
//! 行为模型照抄 Mineradio 的浮空粒子层(源码
//! `public/js/modules/02-visual/01-float-skull-backcover.js` 的 `createFloatLayer`,
//! 观感对照 `docs/reference/play-page/mineradio-particles.png`),不是轨道系统:
//!
//! - **分布**:76% 铺在绕封面的压扁椭圆晕圈(r = 0.62 + u^0.72·2.75,y 压 0.54,
//!   带 lane 抖动),其余 24% 散射在大盒子里填满边角;z 纵深横跨卡片平面,
//!   近侧/远侧粒子由遮挡层分拣 —— 深度效果的来源。
//! - **运动**:极慢的屏面整体旋转(0.030+rand·0.034 rad/s)+ 呼吸缩放 ±4.5% +
//!   每粒子三轴正弦漂移(幅度 0.15~0.45)。是悬浮的尘埃,不是绕圈的星球。
//! - **音频**:克制 —— 低频只轻推纵深(bass·0.10·sin(rand·12)),外加一点
//!   频段缩放脉动;主要的「活」来自闪烁。
//! - **闪烁**:原版是 alpha 正弦振荡;逐帧改上千份材质不划算,这里折进缩放。

use bevy::math::Vec3;

/// 粒子总数,同原版浮空层。真机发热读数出来后再调。
pub(crate) const PARTICLE_COUNT: usize = 1300;

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
/// 电平在入口消毒(非有限按 0、越界 clamp);分布/漂移/缩放各有硬上限,
/// 坏一帧数据也不许把场景炸飞。运动学常数照抄原版浮空层(见模块注释)。
pub(crate) fn particle_pose(
    i: usize,
    time: f32,
    levels: &Levels,
) -> (Vec3, f32) {
    const TAU: f32 = core::f32::consts::TAU;
    let bass = sanitize(levels.bass);
    let level = sanitize(match i % 3 {
        0 => levels.bass,
        1 => levels.mid,
        _ => levels.treble,
    });

    // 基准位:晕圈或散射,两段分布的数值 1:1 照抄原版。
    let halo = i < PARTICLE_COUNT * 76 / 100;
    let (bx, by, bz) = if halo {
        let a = hash01(i, 1) * TAU;
        let r = 0.62 + hash01(i, 2).powf(0.72) * 2.75;
        let lane = (hash01(i, 3) - 0.5) * 0.62;
        (
            a.cos() * r,
            a.sin() * r * 0.54 + lane,
            (hash01(i, 4) - 0.5) * 2.4 - 0.25,
        )
    } else {
        (
            (hash01(i, 5) - 0.5) * 8.4,
            (hash01(i, 6) - 0.5) * 5.8,
            (hash01(i, 7) - 0.5) * 5.6,
        )
    };

    // 每粒子的相位、漂移幅度与「个性」随机数。
    let px = hash01(i, 8) * TAU;
    let py = hash01(i, 9) * TAU;
    let pz = hash01(i, 10) * TAU;
    let amp = 0.15 + hash01(i, 11) * 0.35;
    let rand = hash01(i, 12);

    // 极慢的屏面整体旋转 + 呼吸缩放。
    let orbit = time * (0.030 + rand * 0.034);
    let (sn, cs) = orbit.sin_cos();
    let breathe = 1.0 + (time * 0.34 + px).sin() * 0.045;
    let rx = (bx * cs - by * sn) * breathe;
    let ry = (bx * sn + by * cs) * breathe;

    // 三轴正弦漂移;低频只轻推纵深,方向按粒子个性有正有负。
    let pos = Vec3::new(
        rx + (time * (0.18 + rand * 0.05) + px).sin()
            * amp
            * 0.34,
        ry + (time * (0.15 + rand * 0.06) + py).cos()
            * amp
            * 0.30,
        bz + (time * (0.11 + rand * 0.04) + pz).sin()
            * amp
            * 0.68
            + bass * 0.10 * (rand * 12.0).sin(),
    );

    // 闪烁折进缩放(原版是 alpha 振荡),叠一点克制的频段脉动。
    let twinkle = 0.725
        + 0.275 * (time * (0.42 + rand * 0.34) + pz).sin();
    let base_s = 0.025 + 0.05 * hash01(i, 13);
    let scale = base_s * twinkle * (1.0 + 0.4 * level);
    (pos, scale)
}

/// 下标 → [0,1) 的确定性散列(乘法混淆 + 移位),`salt` 区分不同用途的通道。
/// 不引 rand:同一下标每帧必须得到同一条轨道,粒子才是「在运动」而不是「在闪烁」。
fn hash01(i: usize, salt: u32) -> f32 {
    let mut x = (i as u32)
        .wrapping_add(salt.wrapping_mul(0x9E37_79B9));
    x = x.wrapping_mul(0xA076_1D65);
    x ^= x >> 16;
    x = x.wrapping_mul(0x8EBC_6AF1);
    x ^= x >> 13;
    (x & 0x00FF_FFFF) as f32 / 16_777_216.0
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

    /// 晕圈/散射两段分布各就各位(对照 Mineradio 浮空层):前 76% 在绕封面的
    /// 压扁椭圆晕圈里(t=0 时旋转为恒等、漂移有界,留松量断言),其余在大盒子里。
    #[test]
    fn halo_and_scatter_bases_land_in_their_regions() {
        let silence = Levels::default();
        let halo_count = PARTICLE_COUNT * 76 / 100;
        for i in 0..PARTICLE_COUNT {
            let (p, _) = particle_pose(i, 0.0, &silence);
            if i < halo_count {
                // 只设上界:y 压扁 + lane 抖动允许内圈粒子落到封面中心附近,
                // 原版就是这样(参考图里卡上有点)。
                let r = p.xy().length();
                assert!(
                    r <= 3.9,
                    "晕圈粒子 {i} 半径越界: {r}"
                );
                // 包络:基准 ±1.2,漂移 ≤ 0.5×0.68 ≈ 0.34。
                assert!(
                    (p.z + 0.25).abs() <= 1.6,
                    "晕圈粒子 {i} 纵深越界: {}",
                    p.z
                );
            } else {
                assert!(
                    p.x.abs() <= 4.8
                        && p.y.abs() <= 3.4
                        && p.z.abs() <= 3.3,
                    "散射粒子 {i} 出盒: {p}"
                );
            }
        }
    }

    /// 漂移有界(旋转不改到原点的距离):任意时刻的 |p| 相对 t=0 只允许
    /// 漂移/呼吸量级的偏移,不许越漂越远。
    #[test]
    fn drift_stays_near_the_base() {
        let levels = Levels {
            bass: 0.6,
            mid: 0.3,
            treble: 0.9,
        };
        for i in [0usize, 7, 500, PARTICLE_COUNT - 1] {
            let (p0, _) = particle_pose(i, 0.0, &levels);
            for t in [5.0f32, 60.0, 3600.0] {
                let (p, _) = particle_pose(i, t, &levels);
                assert!(
                    (p.length() - p0.length()).abs() <= 1.3,
                    "粒子 {i} 在 t={t} 漂离基准: {} -> {}",
                    p0.length(),
                    p.length()
                );
            }
        }
    }

    /// 低频推的是纵深 z(原版:`bass * 0.10 * sin(rand*12)`),不改屏面半径。
    #[test]
    fn bass_pushes_depth_not_radius() {
        let quiet = Levels::default();
        let loud = Levels {
            bass: 1.0,
            ..Levels::default()
        };
        let mut any_depth_moved = false;
        for i in 0..PARTICLE_COUNT {
            let (pq, _) = particle_pose(i, 1.0, &quiet);
            let (pl, _) = particle_pose(i, 1.0, &loud);
            assert!(
                (pq.xy().length() - pl.xy().length()).abs()
                    < 1e-3,
                "粒子 {i} 的屏面半径被低频改了"
            );
            if (pq.z - pl.z).abs() > 0.01 {
                any_depth_moved = true;
            }
        }
        assert!(
            any_depth_moved,
            "没有任何粒子的纵深随低频动"
        );
    }

    /// 闪烁:缩放随时间振荡(取两个相位对比),且始终在可见有界区间里。
    #[test]
    fn twinkle_varies_scale_over_time() {
        let silence = Levels::default();
        let mut varied = 0usize;
        for i in 0..PARTICLE_COUNT {
            let (_, s0) = particle_pose(i, 0.0, &silence);
            let (_, s1) = particle_pose(i, 1.3, &silence);
            if (s0 - s1).abs() / s0.max(1e-6) > 0.05 {
                varied += 1;
            }
        }
        assert!(
            varied > PARTICLE_COUNT / 2,
            "闪烁的粒子太少: {varied}/{PARTICLE_COUNT}"
        );
    }

    /// 静音时缩放停在可见下限之上,粒子不消失。
    #[test]
    fn silence_keeps_particles_visible() {
        for i in 0..PARTICLE_COUNT {
            for t in [0.0f32, 1.0, 2.7] {
                let (_, s) =
                    particle_pose(i, t, &Levels::default());
                assert!(
                    s > 0.01,
                    "粒子 {i} 在 t={t} 静音时缩放 {s} 近乎消失"
                );
            }
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
                    // 上限跟随分布包络:散射盒对角 ≈ 5.9,加漂移/呼吸余量取 8。
                    assert!(
                        p.length() < 8.0,
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
