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

/// 焦点行上下各取几行。歌词页一屏放得下的行数,再多就挤成一片灰。
pub const RADIUS: usize = 3;

/// 歌词页当前该画的那一段行:焦点行 + 上下各 `RADIUS` 行,已按行表截断。
///
/// 只描述**取哪几行**,不管怎么画 —— 透明度与字号那条衰减曲线属于界面,
/// 而「窗口停在哪」是规则,换个界面(桌面横排、手机竖排)答案都一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LyricWindow {
    /// 窗口第一行的下标。空窗口时与 `focus` 同为 0,靠 `len` 判空。
    pub first: usize,
    /// 焦点行的下标。跟随时是当前唱到的行,拖动浏览时是拖到的行。
    pub focus: usize,
    /// 窗口含多少行。0 表示没有可画的行。
    pub len: usize,
}

impl LyricWindow {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 窗口末行的下标。空窗口时无意义,调用方先判空。
    pub fn last(&self) -> usize {
        self.first + self.len.saturating_sub(1)
    }

    /// 某一行相对焦点的距离:0 是焦点行,负数在它上面。
    /// 界面拿它算衰减,不在窗口里的行给 `None`。
    pub fn offset_of(&self, index: usize) -> Option<i32> {
        (index >= self.first
            && index < self.first + self.len)
            .then(|| index as i32 - self.focus as i32)
    }
}

/// 取歌词页该画的那一段。`browse` 是拖动浏览叠在当前行上的偏移(行数)。
///
/// 两端**截断**而不回环:回环会把末尾几行画在第一行上方,读起来是一首
/// 倒着接上的歌。拖过头就停在端点,与滚到底的列表同一种手感。
pub fn window(
    lines: &[LyricLineDto],
    current: usize,
    browse: i32,
) -> LyricWindow {
    if lines.is_empty() {
        return LyricWindow {
            first: 0,
            focus: 0,
            len: 0,
        };
    }

    let last = lines.len() - 1;
    let focus = (current as i64 + browse as i64)
        .clamp(0, last as i64) as usize;
    let first = focus.saturating_sub(RADIUS);
    let end = focus.saturating_add(RADIUS).min(last);

    LyricWindow {
        first,
        focus,
        len: end - first + 1,
    }
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

    // ---- 窗口选行(歌词页 #73)----

    /// 窗口以焦点行为中心,上下各取 `RADIUS` 行。
    #[test]
    fn the_window_centers_on_the_focus_line() {
        let lines =
            lines(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let window = window(&lines, 5, 0);

        assert_eq!(window.focus, 5);
        assert_eq!(window.first, 5 - RADIUS);
        assert_eq!(window.len(), RADIUS * 2 + 1);
        assert_eq!(window.offset_of(5), Some(0));
        assert_eq!(window.offset_of(4), Some(-1));
        assert_eq!(window.offset_of(8), Some(3));
    }

    /// 开头与结尾处窗口截断而不回环:第 0 行上面没有第 -1 行。
    ///
    /// 回环的话开头会把末尾几行画在当前行上方,读起来是一首倒着接上的歌。
    #[test]
    fn the_window_clamps_at_both_ends() {
        let lines = lines(&[0, 1, 2, 3, 4]);

        let head = window(&lines, 0, 0);
        assert_eq!(head.first, 0);
        assert_eq!(head.last(), RADIUS.min(4));
        assert_eq!(head.offset_of(0), Some(0));

        let tail = window(&lines, 4, 0);
        assert_eq!(tail.last(), 4);
        assert_eq!(tail.offset_of(4), Some(0));
    }

    /// 行数少于一整窗时窗口就是全表,不补空行。
    #[test]
    fn a_short_lyric_fills_the_window_exactly_once() {
        let lines = lines(&[0, 1]);
        let window = window(&lines, 0, 0);

        assert_eq!(window.first, 0);
        assert_eq!(window.len(), 2);
    }

    /// 空歌词没有窗口,调用方据此让位而不是画一片空行。
    #[test]
    fn an_empty_lyric_has_no_window() {
        assert!(window(&[], 0, 0).is_empty());
    }

    /// 浏览偏移叠在焦点行上再取窗,偏移越界同样截断。
    #[test]
    fn a_browse_offset_shifts_the_window_within_bounds() {
        let lines =
            lines(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

        let moved = window(&lines, 5, 2);
        assert_eq!(moved.focus, 7);
        assert_eq!(moved.first, 7 - RADIUS);

        // 拖过头:焦点钳在末行,窗口跟着停,不越界也不回环。
        let past_end = window(&lines, 5, 99);
        assert_eq!(past_end.focus, 10);
        assert_eq!(past_end.last(), 10);

        let past_start = window(&lines, 5, -99);
        assert_eq!(past_start.focus, 0);
        assert_eq!(past_start.first, 0);
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
