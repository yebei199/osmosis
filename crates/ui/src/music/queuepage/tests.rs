//! 队列页:没同步上去时那套现编的条目号,以及每秒那一趟的开销(#137 ⑥)。
//!
//! 条目号那条单独测:它是整页里唯一一处「两个 id 空间共用一个字段」,而撞上的
//! 后果是点一行放出另一首歌 —— 只在「本机队列没同步、用户又打开了队列页」
//! 这个组合下出现,平时一次都撞不见。

use super::*;

/// 组队列的缓存按新规则隐藏与恢复,关闭页面仍更新总数,重开复用同一模型。
#[test]
fn block_refresh_reprojects_the_group_queue_cache() {
    use crate::music::fixtures::*;
    use similar_asserts::assert_eq;
    use slint::Model as _;

    let (ui, deck) = deck_window();
    let current = track_with_id("a");
    deck.group.assume(Some(app_core::GroupStateDto {
        version: 1,
        members: vec!["me".into(), "pc".into()],
        outputs: vec!["pc".into()],
        clock_epoch: 1,
        now: Some(app_core::GroupNowDto {
            queue_id: 7,
            revision: 2,
            entry_id: 12,
            track: current.clone(),
            anchor_us: 0,
            position_us: 0,
            playing: true,
            next: None,
            at_end: false,
            shuffled: false,
            loop_mode: app_core::LoopModeDto::Off,
        }),
    }));
    deck.queue_mirror.put(
        7,
        2,
        vec![(12, current), (13, track_with_id("b"))],
    );
    ui.global::<Viz>().set_queue_page_open(true);
    refresh(&ui, &deck);
    assert_eq!(ui.global::<Viz>().get_queue_total(), 2);

    deck.blocks.borrow_mut().push(app_core::BlockRuleDto {
        id: "blocked-b".into(),
        kind: app_core::BlockKind::Track,
        value: "b".into(),
        label: "歌 b".into(),
    });
    crate::music::apply_blocks(&ui, &deck);
    assert_eq!(ui.global::<Viz>().get_queue_total(), 1);
    let model = ui.global::<Viz>().get_queue_rows();
    assert_eq!(model.row_count(), 1);
    assert_eq!(
        model
            .row_data(0)
            .expect("未屏蔽的当前条目")
            .id
            .as_str(),
        "12"
    );

    ui.global::<Viz>().set_queue_page_open(false);
    deck.blocks.borrow_mut().clear();
    crate::music::apply_blocks(&ui, &deck);
    assert_eq!(ui.global::<Viz>().get_queue_total(), 2);
    assert_eq!(model.row_count(), 1, "页面关闭时不重建行");
    ui.global::<Viz>().set_queue_page_open(true);
    refresh(&ui, &deck);
    assert_eq!(model.row_count(), 2);
    let mut first =
        model.row_data(0).expect("恢复后的当前条目");
    first.title = "保留模型".into();
    model.set_row_data(0, first);
    refresh(&ui, &deck);
    assert_eq!(
        model.row_data(0).expect("同版条目").title.as_str(),
        "保留模型"
    );
    assert!(!ui.global::<Viz>().get_queue_loading());
    assert!(deck.group.intents().is_empty());
}

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

/// 队列页开着时每秒一趟:队列没变就不重建行,只更新标量(#137 ⑥)。
///
/// 五千首的模型每秒整份重建,滚着看队列时每一秒都卡一下,而那一秒里什么都没变。
#[test]
fn an_unchanged_local_queue_is_not_rebuilt_every_second() {
    use slint::{ComponentHandle as _, Model as _};

    use crate::music::fixtures::*;

    let (ui, deck) = deck_window();
    deck.queue.borrow_mut().replace(
        vec![
            track_with_id("a"),
            track_with_id("b"),
            track_with_id("c"),
        ],
        0,
    );
    ui.global::<Viz>().set_queue_page_open(true);
    refresh(&ui, &deck);

    // 在第一行做个记号:整份重建的话,记号就没了
    let model = ui.global::<Viz>().get_queue_rows();
    let mut first = model.row_data(0).expect("第一行该在");
    first.title = "记号".into();
    model.set_row_data(0, first);

    let _ = deck.queue.borrow_mut().jump_to(2);
    refresh(&ui, &deck);

    assert_eq!(
        model.row_data(0).expect("第一行该在").title,
        "记号",
        "队列没变,行却整份重建了"
    );
    assert_eq!(
        ui.global::<Viz>().get_queue_current(),
        entry_id_at(&deck, 2).to_string().as_str(),
        "当前是哪一条照样每秒跟上"
    );

    // 换了一批才重建
    deck.queue.borrow_mut().replace(
        vec![track_with_id("x"), track_with_id("y")],
        0,
    );
    refresh(&ui, &deck);
    let titles: Vec<String> = ui
        .global::<Viz>()
        .get_queue_rows()
        .iter()
        .map(|row| row.title.to_string())
        .collect();
    assert_eq!(titles, ["歌 x", "歌 y"]);
}

/// 遥控时同一版队列还在路上,每秒那一趟不再发第二次(#137 ⑥)。
#[test]
fn a_queue_fetch_in_flight_is_not_sent_again() {
    let mirror = QueueMirror::default();

    assert!(mirror.begin_fetch(7, 1), "第一次照发");
    assert!(
        !mirror.begin_fetch(7, 1),
        "同一版还在路上,不再发一次"
    );
    assert!(mirror.begin_fetch(7, 2), "新的一版照发");

    mirror.put(7, 2, Vec::new());
    assert!(
        !mirror.begin_fetch(7, 2),
        "已经拿到的这一版不再取"
    );
    assert!(mirror.begin_fetch(7, 3));
    mirror.fetch_failed(7, 3);
    assert!(
        mirror.begin_fetch(7, 3),
        "取失败了下一秒可以重来"
    );
}
