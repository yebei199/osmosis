//! 封面取色喂极光(docs/design/handoff-shaders.md §4)。
//!
//! 封面缩样后取三个主色,喂给玻璃层极光的三团光斑:副色调的丰富度由歌
//! 决定,不用手配色板;绿仍是唯一强调色(docs/adr/0023),这里只动装饰层
//! 的背景,不产出任何语义色。换歌的 400ms 插值在 `glass.slint` 的
//! `animate` 里,这里只负责给结论。
//!
//! 取色不是 k-means:12 个色相桶的直方图挑三个最大桶,各桶取平均色。
//! 对「找三团能当光斑的颜色」这个用途,它与 k-means 给出的结果肉眼
//! 无差,而没有迭代、没有随机初值。饱和度过低的像素直接丢 —— 不丢的话
//! 一堆脏灰会把桶全占了。

use crate::MainWindow;
use crate::viz::CoverPixels;

/// 色相桶数。12 桶 = 每桶 30°,再细分辨不出光斑的差别。
const HUE_BUCKETS: usize = 12;
/// 饱和度阈值,0..=1。低于它的像素是灰,不参与选色。
const MIN_SATURATION: f32 = 0.25;
/// 参与统计的像素至少要占总数的这个比例,不然封面基本是灰的,
/// 硬选出来的三团颜色只是噪声 —— 退回主题绿更诚实。
const MIN_COLORFUL_RATIO: f32 = 0.05;

/// 新封面到了:取色并点亮覆盖。取不出三团像样的颜色就回主题绿。
pub(crate) fn feed(ui: &MainWindow, cover: &CoverPixels) {
    match dominant_colors(
        cover.width,
        cover.height,
        &cover.rgba,
    ) {
        Some([warm, deep, soft]) => {
            ui.set_aurora_cover_warm(color(warm));
            ui.set_aurora_cover_deep(color(deep));
            ui.set_aurora_cover_soft(color(soft));
            ui.set_aurora_cover_active(true);
        }
        None => reset(ui),
    }
}

/// 换歌先清:与封面卡、点云同一条原则,旧色配新歌比主题绿更误导。
pub(crate) fn reset(ui: &MainWindow) {
    ui.set_aurora_cover_active(false);
}

fn color(rgb: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
}

/// 三个主色,按桶大小从大到小。凑不齐三个像样的桶给 `None`。
fn dominant_colors(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Option<[[u8; 3]; 3]> {
    let (width, height) = (width as usize, height as usize);
    if width == 0
        || height == 0
        || rgba.len() < width * height * 4
    {
        return None;
    }

    // 每桶累计 (r, g, b, 数量)。缩样到最多 16×16:光斑要的是大色块,
    // 逐像素统计只是花时间。
    let mut buckets = [[0u64; 4]; HUE_BUCKETS];
    let step_x = (width / 16).max(1);
    let step_y = (height / 16).max(1);
    let mut sampled = 0u64;
    for y in (0..height).step_by(step_y) {
        for x in (0..width).step_by(step_x) {
            sampled += 1;
            let i = (y * width + x) * 4;
            let (r, g, b) =
                (rgba[i], rgba[i + 1], rgba[i + 2]);
            let Some(hue) = saturated_hue(r, g, b) else {
                continue;
            };
            let bucket = &mut buckets[hue];
            bucket[0] += u64::from(r);
            bucket[1] += u64::from(g);
            bucket[2] += u64::from(b);
            bucket[3] += 1;
        }
    }

    let colorful: u64 = buckets.iter().map(|b| b[3]).sum();
    if sampled == 0
        || (colorful as f32 / sampled as f32)
            < MIN_COLORFUL_RATIO
    {
        return None;
    }

    let mut order: Vec<&[u64; 4]> = buckets
        .iter()
        .filter(|bucket| bucket[3] > 0)
        .collect();
    order
        .sort_by_key(|bucket| std::cmp::Reverse(bucket[3]));

    let average = |bucket: &[u64; 4]| -> [u8; 3] {
        let n = bucket[3].max(1);
        [
            (bucket[0] / n) as u8,
            (bucket[1] / n) as u8,
            (bucket[2] / n) as u8,
        ]
    };

    // 不足三桶就循环补齐:单色封面的三团光斑同色相,观感仍然成立。
    let first = order.first()?;
    Some([
        average(first),
        average(order.get(1).copied().unwrap_or(first)),
        average(order.get(2).copied().unwrap_or(first)),
    ])
}

/// 这个像素的色相桶;饱和度不够给 `None`。
fn saturated_hue(r: u8, g: u8, b: u8) -> Option<usize> {
    let (rf, gf, bf) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    if max <= 0.0 || delta / max < MIN_SATURATION {
        return None;
    }

    let hue = if (max - rf).abs() < f32::EPSILON {
        ((gf - bf) / delta).rem_euclid(6.0)
    } else if (max - gf).abs() < f32::EPSILON {
        (bf - rf) / delta + 2.0
    } else {
        (rf - gf) / delta + 4.0
    } / 6.0;

    Some(
        ((hue * HUE_BUCKETS as f32) as usize)
            .min(HUE_BUCKETS - 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(
        width: u32,
        height: u32,
        rgb: [u8; 3],
    ) -> Vec<u8> {
        (0..width * height)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    /// 双色封面:两个真桶 + 循环补齐第三团,大桶在前。
    #[test]
    fn two_hues_rank_by_area_and_pad_the_third() {
        let mut rgba = solid(16, 12, [200, 40, 40]);
        rgba.extend(solid(16, 4, [40, 60, 220]));
        let [a, b, c] = dominant_colors(16, 16, &rgba)
            .expect("该取出主色");

        assert!(
            a[0] > a[2],
            "面积大的红该排第一,实得 {a:?}"
        );
        assert!(b[2] > b[0], "蓝该排第二,实得 {b:?}");
        assert_eq!(c, a, "不足三桶时循环补齐");
    }

    /// 灰度封面取不出像样的颜色,退回主题绿(None)。
    #[test]
    fn a_grey_cover_yields_nothing() {
        let rgba = solid(16, 16, [128, 128, 130]);
        assert!(dominant_colors(16, 16, &rgba).is_none());
    }

    /// 尺寸对不上像素长度时不 panic,给 None。
    #[test]
    fn a_short_buffer_is_rejected() {
        assert!(
            dominant_colors(16, 16, &[0; 12]).is_none()
        );
    }
}
