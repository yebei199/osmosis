//! 歌词在时间轴上的定位:给一份行表与播放位置,回答「现在唱到第几行」。
//!
//! 放在客户端领域而不是 UI:它是规则,不是画法 —— 换个界面(桌面横排、
//! 手机竖排、将来的逐字卡拉 OK)对「当前是第几行」的答案不该有影响。

use crate::LyricLineDto;

/// 当前该显示第几行(下标)。没有可显示的行时给 `None`。
///
/// 判据是**下一行的开始时刻**,不是本行的 `end_ms`:平台给的 `end_ms` 常常
/// 缺席或不可靠,而行与行之间的空隙里(间奏)显示上一行,是各家播放器的通行做法。
pub fn current_line(
    lines: &[LyricLineDto],
    position_ms: i64,
) -> Option<usize> {
    // 线性扫描取**最后一个**已开始的行。不用二分:行表撑死几百行,而二分
    // 要求时刻单调,平台给的脏数据(乱序、重叠)会让它给出无意义的答案。
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.start_ms <= position_ms)
        .map(|(index, _)| index)
        .next_back()
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 造一份行表,只关心时刻。每行给 200ms 的 `end_ms`,好证明选行不看它。
    fn lines(starts: &[i64]) -> Vec<LyricLineDto> {
        starts
            .iter()
            .enumerate()
            .map(|(i, start)| LyricLineDto {
                start_ms: *start,
                end_ms: start + 200,
                text: format!("line {i}"),
                translation: None,
            })
            .collect()
    }

    /// 第一行开始之前(前奏):没有当前行,歌词区留白。
    #[test]
    fn before_first_line_selects_nothing() {
        let lines = lines(&[1_000, 5_000]);
        assert_eq!(current_line(&lines, 0), None);
        assert_eq!(current_line(&lines, 999), None);
    }

    /// 正好落在某行的开始时刻:选中那一行,不是上一行(边界归属钉死)。
    #[test]
    fn exact_start_selects_that_line() {
        let lines = lines(&[1_000, 5_000]);
        assert_eq!(current_line(&lines, 1_000), Some(0));
        assert_eq!(current_line(&lines, 5_000), Some(1));
    }

    /// 落在行间空隙(间奏)时保持上一行,不跳空 —— 判据是下一行的开始时刻,
    /// 而不是本行那个常常不可靠的 end_ms(这里是 1_200)。
    #[test]
    fn inside_a_gap_keeps_the_previous_line() {
        let lines = lines(&[1_000, 5_000]);
        assert_eq!(current_line(&lines, 1_500), Some(0));
        assert_eq!(current_line(&lines, 4_999), Some(0));
    }

    /// 末行之后(尾奏)保持末行,不清空。
    #[test]
    fn after_last_line_keeps_the_last() {
        let lines = lines(&[1_000, 5_000]);
        assert_eq!(current_line(&lines, 60_000), Some(1));
    }

    /// 空歌词(纯音乐/未收录):没有当前行,且不 panic。
    #[test]
    fn empty_lyric_selects_nothing() {
        assert_eq!(current_line(&[], 0), None);
        assert_eq!(current_line(&[], 10_000), None);
    }

    /// 上游给的时刻乱序或重叠:不 panic,给出一个确定的答案 ——
    /// 平台数据脏是常态,不是程序错误。
    #[test]
    fn unordered_lines_do_not_panic() {
        let messy = lines(&[5_000, 1_000, 5_000, -3]);
        for position in
            [-100i64, 0, 900, 1_000, 5_000, 99_999]
        {
            // 只要求给出一个合法下标或 None,不规定乱序时挑哪一个。
            if let Some(index) =
                current_line(&messy, position)
            {
                assert!(
                    index < messy.len(),
                    "位置 {position} 选出了越界下标 {index}"
                );
            }
        }
    }
}
