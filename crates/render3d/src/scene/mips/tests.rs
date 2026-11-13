use similar_asserts::assert_eq;

use super::*;

/// 一张 `w × h`、每个像素都是同一个颜色的图。
fn flat(w: u32, h: u32, pixel: [u8; 4]) -> Vec<u8> {
    pixel.repeat((w * h) as usize)
}

/// 级数与每级尺寸对得上 wgpu 的 mip 规则(逐级减半、至少 1),总字节数与之相符 ——
/// 差一个字节,`create_texture_with_data` 就会越界 panic。
#[test]
fn the_chain_has_the_sizes_wgpu_expects() {
    for (w, h) in
        [(512, 512), (500, 333), (1, 1), (7, 1), (1, 9)]
    {
        let (data, levels) =
            with_mips(w, h, &flat(w, h, [1, 2, 3, 4]));
        let longest = w.max(h);
        assert_eq!(
            levels,
            longest.ilog2() + 1,
            "{w}x{h} 的级数"
        );
        let expected: usize = (0..levels)
            .map(|level| {
                let side =
                    |s: u32| (s >> level).max(1) as usize;
                side(w) * side(h) * 4
            })
            .sum();
        assert_eq!(
            data.len(),
            expected,
            "{w}x{h} 的字节数"
        );
    }
}

/// 第 0 级就是原图,原样不动。
#[test]
fn level_zero_is_the_original() {
    let rgba: Vec<u8> =
        (0..4 * 4 * 4).map(|i| i as u8).collect();
    let (data, _) = with_mips(4, 4, &rgba);
    assert_eq!(&data[..rgba.len()], &rgba[..]);
}

/// 每一级是上一级 2×2 的平均:左黑右白的图,第 1 级左右各一半。
#[test]
fn each_level_averages_the_one_above() {
    let mut rgba = Vec::new();
    for _row in 0..2 {
        rgba.extend_from_slice(&[
            0, 0, 0, 255, 0, 0, 0, 255,
        ]);
        rgba.extend_from_slice(&[
            255, 255, 255, 255, 255, 255, 255, 255,
        ]);
    }
    // 4×2:左两列黑、右两列白。
    let (data, levels) = with_mips(4, 2, &rgba);
    assert_eq!(levels, 3);
    let level1 = &data[32..40];
    assert_eq!(
        level1,
        &[0u8, 0, 0, 255, 255, 255, 255, 255][..]
    );
    let level2 = &data[40..44];
    assert_eq!(level2, &[128u8, 128, 128, 255][..]);
}

/// 纯色图的每一级都还是那个颜色:平均不漂、不截断。
#[test]
fn a_flat_image_stays_flat_at_every_level() {
    let pixel = [200, 17, 99, 255];
    let (data, _) = with_mips(37, 21, &flat(37, 21, pixel));
    for chunk in data.chunks_exact(4) {
        assert_eq!(chunk, &pixel[..]);
    }
}
