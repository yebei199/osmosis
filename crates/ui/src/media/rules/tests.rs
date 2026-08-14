use similar_asserts::assert_eq;

use super::*;

/// 绝对跳转换成比例。
///
/// 外面给的是绝对毫秒,Slint 的 `seek` 收的是 0..1(见 `app.slint:74`)。
#[test]
fn a_seek_target_becomes_a_ratio() {
    assert_eq!(seek_ratio(60_000, 240_000), Some(0.25));
    // 越界的目标夹住而不是溢出:外面拖到条尾时给的值可能略超时长。
    assert_eq!(seek_ratio(999_000, 240_000), Some(1.0));
    assert_eq!(seek_ratio(-5, 240_000), Some(0.0));
}

/// 没有时长就不跳。
///
/// 还没装起来、或上游没给时长时,这次跳转没有意义 —— 该被丢掉,而不是除零。
#[test]
fn a_seek_without_a_duration_is_dropped() {
    assert_eq!(seek_ratio(60_000, 0), None);
    assert_eq!(seek_ratio(60_000, -1), None);
}

/// 相对跳转从当前位置起算。
///
/// MPRIS 的 `Seek` 是相对的(快进 10 秒),安卓的 `onSeekTo` 是绝对的。
/// 两者都在这里收敛成绝对位置,后端不必自己拿位置去加。
#[test]
fn a_relative_seek_starts_from_the_current_position() {
    let at = 30_000;

    assert_eq!(
        seek_target(MediaCommand::SeekTo(90_000), at),
        Some(90_000)
    );
    assert_eq!(
        seek_target(MediaCommand::SeekBy(10_000), at),
        Some(40_000)
    );
    // 往回跳过了头就落到开头,负的绝对位置没有意义。
    assert_eq!(
        seek_target(MediaCommand::SeekBy(-90_000), at),
        Some(0)
    );
    // 不是跳转的键在这里没有答案。
    assert_eq!(seek_target(MediaCommand::Next, at), None);
}
