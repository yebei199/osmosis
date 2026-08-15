use similar_asserts::assert_eq;

use super::*;

// ── 视觉预设 ──────────────────────────────────────────────────────

/// 每一档都保住自己的下标 —— 加档时着色器按下标分支,错一位就画成另一个预设。
/// 现在只有一档,这条守的是"默认档的下标必须是 0"。
#[test]
fn every_preset_keeps_its_index() {
    for i in 0..PRESET_COUNT {
        assert_eq!(
            preset_index(i),
            i as u32,
            "第 {i} 档下标被改了"
        );
    }
}

/// 越界的编号回默认档 —— 编号来自 `.slint`,是跨层的外部输入,
/// 给个不存在的档不该画出一片空白。
#[test]
fn an_out_of_range_preset_falls_back_to_the_default() {
    for i in [-1, -100, PRESET_COUNT, PRESET_COUNT + 7] {
        assert_eq!(
            preset_index(i),
            0,
            "编号 {i} 该回默认档"
        );
    }
}

// ── 窄视口 ────────────────────────────────────────────────────────

/// 横屏与方屏不收:物体类预设按相机距离取景,横向够宽就不必动。
#[test]
fn wide_viewports_keep_the_base_object_scale() {
    for (w, h) in [(1920u32, 1080u32), (1080, 1080)] {
        assert!(
            (object_scale(w, h) - OBJECT_SCALE).abs()
                < 1e-6,
            "{w}x{h} 不该收缩"
        );
    }
}

/// 竖屏按长宽比收:小米13 竖屏 aspect 0.45,球体不收就左右出画(真机实测)。
#[test]
fn portrait_viewports_shrink_by_the_aspect_ratio() {
    let scale = object_scale(1080, 2400);
    let aspect = 1080.0 / 2400.0;
    assert!(
        (scale - OBJECT_SCALE * aspect).abs() < 1e-6,
        "竖屏收缩量不对: {scale}"
    );
    assert!(scale < OBJECT_SCALE, "竖屏没收");
}

/// 0 尺寸(首帧/刚重建)退回基准倍数,不除零、不把物体缩没。
#[test]
fn a_zero_sized_viewport_falls_back_to_the_base_scale() {
    for (w, h) in [(0u32, 1080u32), (1080, 0), (0, 0)] {
        assert_eq!(object_scale(w, h), OBJECT_SCALE);
    }
}
