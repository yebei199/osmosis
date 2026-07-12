//! 场景控制参数:把 Slint LineEdit 的原始文本,在信任边界解析成渲染器要的干净数值。
//!
//! 解析规则统一:非法输入(空串 / 非数字 / 越界)一律**退回上一个好值**并对合法值 clamp,
//! 绝不把垃圾喂给渲染器。这些是纯函数,是本 crate 里唯一需要单测的核心逻辑。

/// 传给渲染器的一帧场景控制量。全 POD,不含 bevy/slint 类型,由 apps/* 在 seam 处
/// 翻译成 `render3d::SceneParams`(见架构:ui 与 render3d 刻意解耦)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneControls {
    /// 0 = 形状画廊,1 = 实体阵列。
    pub scene_id: i32,
    /// 转盘朝向(弧度,Slint 侧拖动累加)。
    pub yaw: f32,
    pub pitch: f32,
    /// 实体数,已 clamp 到 [`COUNT_RANGE`]。
    pub count: u32,
    /// 基础色 0xRRGGBB。
    pub color_rgb: u32,
    /// 自转角速度(弧度/帧),已 clamp 到 [`SPEED_RANGE`]。
    pub spin_speed: f32,
    /// 间距/缩放,已 clamp 到 [`SPACING_RANGE`]。
    pub spacing: f32,
}

/// 实体数合法区间(含端点)。下限 1 保证场景非空,上限防阵列爆量卡死。
pub const COUNT_RANGE: (u32, u32) = (1, 512);
/// 自转角速度合法区间(弧度/帧)。允许 0(静止),上限防转成一团糊。
pub const SPEED_RANGE: (f32, f32) = (0.0, 0.5);
/// 间距/缩放合法区间。下限 >0 防实体重叠成一点。
pub const SPACING_RANGE: (f32, f32) = (0.2, 8.0);

/// 解析实体数:合法则 clamp 进 [`COUNT_RANGE`],否则退回 `prev`。
pub fn parse_count(text: &str, prev: u32) -> u32 {
    match text.trim().parse::<u32>() {
        Ok(n) => n.clamp(COUNT_RANGE.0, COUNT_RANGE.1),
        Err(_) => prev,
    }
}

/// 解析 `#rrggbb` / `rrggbb` 十六进制色为 0xRRGGBB,非法退回 `prev`。
pub fn parse_hex_rgb(text: &str, prev: u32) -> u32 {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if hex.len() != 6 {
        return prev;
    }
    u32::from_str_radix(hex, 16).unwrap_or(prev)
}

/// 解析自转角速度:合法则 clamp 进 [`SPEED_RANGE`],否则退回 `prev`。
pub fn parse_speed(text: &str, prev: f32) -> f32 {
    parse_f32_clamped(text, prev, SPEED_RANGE)
}

/// 解析间距/缩放:合法则 clamp 进 [`SPACING_RANGE`],否则退回 `prev`。
pub fn parse_spacing(text: &str, prev: f32) -> f32 {
    parse_f32_clamped(text, prev, SPACING_RANGE)
}

/// f32 解析 + clamp 的共用实现:非有限值(NaN/inf)也当非法退回 `prev`。
fn parse_f32_clamped(text: &str, prev: f32, range: (f32, f32)) -> f32 {
    match text.trim().parse::<f32>() {
        Ok(v) if v.is_finite() => v.clamp(range.0, range.1),
        _ => prev,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 实体数_正常值原样返回() {
        assert_eq!(parse_count("64", 8), 64);
        assert_eq!(parse_count("  100 ", 8), 100); // 容忍首尾空白
    }

    #[test]
    fn 实体数_空串或非数字退回上一个好值() {
        assert_eq!(parse_count("", 8), 8);
        assert_eq!(parse_count("abc", 8), 8);
        assert_eq!(parse_count("-3", 8), 8); // 负号非 u32,退回
    }

    #[test]
    fn 实体数_越界被clamp到区间端点() {
        assert_eq!(parse_count("0", 8), COUNT_RANGE.0); // 下限
        assert_eq!(parse_count("99999", 8), COUNT_RANGE.1); // 上限
    }

    #[test]
    fn 十六进制色_带井号与不带井号都能解析() {
        assert_eq!(parse_hex_rgb("#4a6bff", 0), 0x4a6bff);
        assert_eq!(parse_hex_rgb("4a6bff", 0), 0x4a6bff);
    }

    #[test]
    fn 十六进制色_大小写不敏感() {
        assert_eq!(parse_hex_rgb("#ABCDEF", 0), 0xABCDEF);
    }

    #[test]
    fn 十六进制色_长度错或含非法字符退回上一个好值() {
        let prev = 0x112233;
        assert_eq!(parse_hex_rgb("#fff", prev), prev); // 3 位短式不支持
        assert_eq!(parse_hex_rgb("#12345", prev), prev); // 长度错
        assert_eq!(parse_hex_rgb("#gggggg", prev), prev); // 非十六进制
        assert_eq!(parse_hex_rgb("", prev), prev);
    }

    #[test]
    fn 自转速度_负数与超上限都被clamp() {
        assert_eq!(parse_speed("-1", 0.1), SPEED_RANGE.0);
        assert_eq!(parse_speed("9.0", 0.1), SPEED_RANGE.1);
        assert_eq!(parse_speed("0.2", 0.1), 0.2);
    }

    #[test]
    fn 自转速度_非数字退回上一个好值() {
        assert_eq!(parse_speed("fast", 0.1), 0.1);
        assert_eq!(parse_speed("", 0.1), 0.1);
    }

    #[test]
    fn 间距_越界被clamp且非数字退回() {
        assert_eq!(parse_spacing("0.05", 1.0), SPACING_RANGE.0);
        assert_eq!(parse_spacing("99", 1.0), SPACING_RANGE.1);
        assert_eq!(parse_spacing("2.5", 1.0), 2.5);
        assert_eq!(parse_spacing("wide", 1.0), 1.0);
    }
}
