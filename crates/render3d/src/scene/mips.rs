//! 封面纹理的 mip 链:在 CPU 上逐级 2×2 求平均,随原图一起上传。
//!
//! 点云封面外那一圈要一份「糊过的封面」(`cloud.wgsl` 的 `haze`):取高几级的 mip,
//! 一次采样就是一大片的平均。GPU 上生成 mip 要另起一条 blit pass,bevy 也不替上传的
//! 图生成;封面一首歌只传一次,长边不过 512(`ui::imagery::cover` 收过),整条链
//! 只比原图多算三分之一的像素,CPU 上走一遍不值得为它开一条 pass。
//!
//! 平均是直接对 sRGB 编码的字节做的,不先换到线性:严格说偏暗一点,但用它的那一圈
//! 本来就压暗了,差别看不出;换线性要一张查找表,省下的只是一个看不见的误差。

/// 原图后面接上整条 mip 链,返回 (全部字节, 级数)。级数含原图那一级。
///
/// 每级宽高是上一级的一半、至少 1(与 wgpu 的 `mip_level_size` 一致),各级首尾相接、
/// 每行紧排 —— 正是 `create_texture_with_data` 按 mip 顺序读的布局。奇数边的最后
/// 一列 / 一行与它前一个像素一起平均(下标夹在边内),不丢内容。
///
/// 调用方已核过 `rgba.len() == width × height × 4`。
pub(crate) fn with_mips(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> (Vec<u8>, u32) {
    let levels =
        32 - width.max(height).max(1).leading_zeros();
    let mut data =
        Vec::with_capacity(rgba.len() * 4 / 3 + 4);
    data.extend_from_slice(rgba);

    let (mut w, mut h) = (width as usize, height as usize);
    let mut from = 0;
    for _ in 1..levels {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let at = data.len();
        data.resize(at + nw * nh * 4, 0);
        for y in 0..nh {
            let rows = [
                (2 * y).min(h - 1),
                (2 * y + 1).min(h - 1),
            ];
            for x in 0..nw {
                let cols = [
                    (2 * x).min(w - 1),
                    (2 * x + 1).min(w - 1),
                ];
                for channel in 0..4 {
                    let mut sum = 0u32;
                    for row in rows {
                        for col in cols {
                            sum += u32::from(
                                data[from
                                    + (row * w + col) * 4
                                    + channel],
                            );
                        }
                    }
                    // 四个 u8 之和除 4 再四舍五入,落回 0..=255。
                    data[at + (y * nw + x) * 4 + channel] =
                        ((sum + 2) / 4) as u8;
                }
            }
        }
        from = at;
        (w, h) = (nw, nh);
    }
    (data, levels)
}

#[cfg(test)]
mod tests;
