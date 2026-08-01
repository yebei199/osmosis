//! 播放进度的格式化。
//!
//! 与列表里的时长同一条规矩:算在 Rust 侧、测在 Rust 侧,`.slint` 里只负责摆
//! (见 crates/ui/slint/types.slint)。
//!
//! 位置由 `audio::Player::position()` 给,总长由 `TrackDto.duration_ms` 给 ——
//! 两个来源,所以它们会对不上:解码器回读时位置可能略微越过总长,而平台偶尔
//! 干脆不给总长。两种情况都在这里收干净,不留给界面去判。

/// 一个时间点写成 `分:秒`,秒补零。
///
/// 不写小时:超过一小时的单曲不是这个应用要处理的东西,而为它多一段
/// 条件分支,会让 99.9% 的歌都带着一个恒为 "0:" 的前缀。
pub fn clock(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".to_owned();
    }

    let total = seconds as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// 进度那一行:`已放 / 总长`。
///
/// 总长不知道(平台没给,`duration_ms` 为 0)时**只给已放的那一半** ——
/// 写成 "1:23 / 0:00" 是在说一句假话,而那句假话看起来像个 bug。
pub fn progress_text(
    position_secs: f64,
    duration_ms: i64,
) -> String {
    if duration_ms <= 0 {
        return clock(position_secs);
    }

    format!(
        "{} / {}",
        clock(position_secs),
        clock(duration_ms as f64 / 1000.0)
    )
}

/// 进度条填到几分之几,0.0 到 1.0。
///
/// 夹住两端:位置与总长是两个来源,解码器回读时位置可能略微越过总长,
/// 不夹的话进度条会画出槽外。总长不知道时给 0 —— 没有分母就没有比例,
/// 画一条随便什么长度的填充不如不画。
pub fn ratio(position_secs: f64, duration_ms: i64) -> f32 {
    if duration_ms <= 0 || !position_secs.is_finite() {
        return 0.0;
    }

    let total = duration_ms as f64 / 1000.0;
    ((position_secs / total) as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进度文案是 已放/总长,分:秒,秒补零。
    ///
    /// 补零不是好看:"3:7" 会被读成三分七秒还是三分七十秒,取决于读的人。
    #[test]
    fn progress_text_reads_as_minutes_and_seconds() {
        assert_eq!(
            progress_text(7.0, 221_000),
            "0:07 / 3:41"
        );
        assert_eq!(
            progress_text(221.0, 221_000),
            "3:41 / 3:41"
        );
        assert_eq!(
            progress_text(0.0, 60_000),
            "0:00 / 1:00"
        );
    }

    /// 平台没给总长时只显示已放的那一半。
    ///
    /// "1:23 / 0:00" 是在说一句假话,而那句假话看起来像个 bug。
    #[test]
    fn an_unknown_duration_shows_only_the_elapsed_side() {
        assert_eq!(progress_text(83.0, 0), "1:23");
        assert_eq!(progress_text(83.0, -1), "1:23");
    }

    /// 比例夹在 0..=1。
    ///
    /// 位置与总长是两个来源:解码器回读时位置可能略微越过总长,
    /// 不夹的话进度条会画出槽外。
    #[test]
    fn the_ratio_is_clamped() {
        assert!((ratio(0.0, 200_000) - 0.0).abs() < 1e-6);
        assert!((ratio(100.0, 200_000) - 0.5).abs() < 1e-6);
        assert!(
            (ratio(999.0, 200_000) - 1.0).abs() < 1e-6,
            "越过总长要收到 1.0"
        );
        assert!(
            (ratio(-5.0, 200_000)).abs() < 1e-6,
            "负数收到 0.0"
        );
        assert!(
            (ratio(50.0, 0)).abs() < 1e-6,
            "没有分母就没有比例"
        );
    }

    /// 非有限的秒数不能把界面弄崩 —— 它一路来自浮点除法。
    #[test]
    fn a_nonfinite_position_is_not_rendered() {
        assert_eq!(clock(f64::NAN), "0:00");
        assert_eq!(clock(f64::INFINITY), "0:00");
        assert!((ratio(f64::NAN, 200_000)).abs() < 1e-6);
    }
}
