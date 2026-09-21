//! 没同步上去时那套现编的条目号。
//!
//! 只测这一条:它是整页里唯一一处「两个 id 空间共用一个字段」,而撞上的
//! 后果是点一行放出另一首歌 —— 只在「本机队列没同步、用户又打开了队列页」
//! 这个组合下出现,平时一次都撞不见。

use super::*;

/// 现编的号**永远是负数**,而服务端发的是 `BIGSERIAL`,永远为正。
#[test]
fn synthetic_ids_never_collide_with_real_ones() {
    for at in 0..1_000 {
        assert!(
            synthetic_entry_id(at) < 0,
            "第 {at} 首编出来的号该是负的"
        );
    }
}

/// 不同位置编出来的号互不相同 —— 否则点第二行会跳到第一行。
#[test]
fn synthetic_ids_are_distinct_per_position() {
    let ids: std::collections::HashSet<i64> =
        (0..1_000).map(synthetic_entry_id).collect();

    assert_eq!(ids.len(), 1_000);
}
