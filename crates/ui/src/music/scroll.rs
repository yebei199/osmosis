//! 曲目列表的滚动位置:换模型时把眼前那一行钉住,打开日推时滚到在放的那一行。
//!
//! 列表模型是整个换掉的(见 `list::push_rows`)。上面的行少了一首、多了一首,
//! 视口的像素偏移不变,眼前的歌就整体跳一行 —— 刷新一次日推,正看着的那首
//! 被挤走。这里只算「该滚到哪」,滚由列表自己做(`tracklist.slint` 的 `scroll-seq`)。
//!
//! 位置一律用「内容顶端往下多少逻辑像素」,与列表的 `content-y` 差一个负号。

use slint::ComponentHandle;

use crate::{Player, TrackRow};

use super::MainWindow;

/// 一行的身份与高度。堆头没有曲目 id,按堆名认。
pub(super) type Line = (String, f32);

/// 列表的每一行:身份与高度,高度照 `Player` 里那两个数。
pub(super) fn lines(
    ui: &MainWindow,
    rows: impl IntoIterator<Item = TrackRow>,
) -> Vec<Line> {
    let player = ui.global::<Player>();
    let (track, pile) = (
        player.get_track_row_height(),
        player.get_pile_row_height(),
    );
    rows.into_iter()
        .map(|row| {
            if row.header {
                (format!("\u{1}{}", row.title), pile)
            } else {
                (row.id.to_string(), track)
            }
        })
        .collect()
}

/// 换模型之后该滚到哪,让眼前那一行留在屏幕上原来的位置。
///
/// 认的是旧列表里视口顶上那一行;它在新列表里没了,就往下找第一行还在的。
/// 都没了(换了一批完全不同的歌)是 `None`:不动。
pub(super) fn anchored(
    old: &[Line],
    scroll: f32,
    new: &[Line],
) -> Option<f32> {
    let old_tops = tops(old);
    let first = old_tops
        .iter()
        .zip(old)
        .position(|(top, (_, height))| top + height > scroll)?;
    let new_tops = tops(new);
    old.iter().zip(&old_tops).skip(first).find_map(
        |((key, _), old_top)| {
            let at = new.iter().position(|(id, _)| id == key)?;
            Some((new_tops[at] - (old_top - scroll)).max(0.0))
        },
    )
}

/// 每一行顶边离内容顶端多远。
fn tops(lines: &[Line]) -> Vec<f32> {
    lines
        .iter()
        .scan(0.0, |top, (_, height)| {
            let this = *top;
            *top += height;
            Some(this)
        })
        .collect()
}

/// 此刻滚到了哪。
pub(super) fn current(ui: &MainWindow) -> f32 {
    -ui.global::<Player>().get_list_scroll_y()
}

/// 叫列表滚到 `to`。读数当场跟着改:同一轮里接着算的人读到的是新位置。
pub(super) fn scroll_to(ui: &MainWindow, to: f32) {
    let player = ui.global::<Player>();
    player.set_list_scroll_to(to);
    player.set_list_scroll_seq(
        player.get_list_scroll_seq().wrapping_add(1),
    );
    player.set_list_scroll_y(-to);
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn rows(ids: &[&str]) -> Vec<Line> {
        ids.iter().map(|id| ((*id).to_owned(), 10.0)).collect()
    }

    /// 上面少了一首:眼前那一行不跟着往上跳。
    #[test]
    fn a_row_removed_above_does_not_move_the_visible_rows() {
        let old = rows(&["a", "b", "c", "d", "e"]);
        let new = rows(&["a", "c", "d", "e"]);

        // 视口顶在 c 往下 4 像素处
        assert_eq!(anchored(&old, 24.0, &new), Some(14.0));
    }

    /// 顶上那一行自己没了:钉住它下面第一行还在的。
    #[test]
    fn a_vanished_top_row_hands_over_to_the_next_one() {
        let old = rows(&["a", "b", "c", "d"]);
        let new = rows(&["x", "a", "d"]);

        // 视口顶在 b 往下 5 像素;c 也没了,d 原来在屏幕上 15 像素处
        assert_eq!(anchored(&old, 15.0, &new), Some(5.0));
    }

    /// 换了一批完全不同的歌:不动。
    #[test]
    fn a_different_batch_is_not_anchored() {
        assert_eq!(
            anchored(&rows(&["a", "b"]), 5.0, &rows(&["c"])),
            None
        );
    }
}
