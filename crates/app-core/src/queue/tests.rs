use similar_asserts::assert_eq;

use super::*;

mod loop_mode;

mod shuffle;

fn track(id: usize) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_string(),
        title: format!("歌 {id}"),
        alias: None,
        artists: vec!["测试".to_owned()],
        cover: None,
        duration_ms: 1_000,
    }
}

fn batch(n: usize) -> Vec<TrackDto> {
    (0..n).map(track).collect()
}

/// 当前曲目的 id,断言里少写一层 Option 解包。
fn id_of(queue: &Queue) -> Option<String> {
    queue.current().map(|t| t.id.clone())
}

/// 从批里点第 k 首开始:当前曲目就是它,不从头放。
#[test]
fn queue_starts_at_the_chosen_track() {
    let queue = Queue::new(batch(5), 2);

    assert_eq!(id_of(&queue), Some("2".to_owned()));
}

/// 顺序模式:下一首按批的顺序走。
#[test]
fn next_walks_the_batch_in_order() {
    let mut queue = Queue::new(batch(4), 0);

    let walked: Vec<String> = core::iter::from_fn(|| {
        queue.next(0).map(|t| t.id.clone())
    })
    .collect();

    assert_eq!(walked, ["1", "2", "3"]);
}

/// **看一眼下一首,队列不能动。**
///
/// 预取正是在当前这首还在放的时候备下一首 —— 游标要是跟着动了,
/// `current()` 立刻变成下一首,界面和播放器都会以为已经换歌了。
#[test]
fn peek_next_names_the_next_track_without_moving() {
    let queue = Queue::new(batch(4), 1);

    assert_eq!(
        queue.peek_next().map(|t| t.id.as_str()),
        Some("2")
    );
    assert_eq!(
        queue.current().map(|t| t.id.as_str()),
        Some("1"),
        "看一眼不该把当前这首也换掉"
    );
    assert_eq!(
        queue.peek_next().map(|t| t.id.as_str()),
        Some("2"),
        "看两眼结果该一样"
    );
}

/// 队尾之后没有下一首 —— 那时不该预取任何东西,判据与 `next` 一致。
#[test]
fn peek_next_at_the_end_of_the_queue_is_none() {
    let queue = Queue::new(batch(2), 1);

    assert!(queue.peek_next().is_none());
}

/// 上一首回到**刚才放过的那首**。
#[test]
fn previous_returns_to_the_track_just_played() {
    let mut queue = Queue::new(batch(4), 0);
    queue.next(0);
    queue.next(0);

    assert_eq!(
        queue.previous().map(|t| t.id.clone()),
        Some("1".to_owned())
    );
}

/// **一轮内不重复,放完即停**:最后一首之后 next() 给 None,状态不变。
#[test]
fn next_at_the_end_returns_none_and_stays() {
    let mut queue = Queue::new(batch(3), 2);

    assert!(queue.next(0).is_none());
    assert_eq!(
        id_of(&queue),
        Some("2".to_owned()),
        "队尾的 next 不该挪动位置"
    );
}

/// 边界:第一首之前没有上一首。
#[test]
fn previous_at_the_start_returns_none() {
    let mut queue = Queue::new(batch(3), 0);

    assert!(queue.previous().is_none());
    assert_eq!(id_of(&queue), Some("0".to_owned()));
}

/// 换一批就整个换队列:cursor 重置,旧批消失。
#[test]
fn replacing_the_batch_resets_the_queue() {
    let mut queue = Queue::new(batch(3), 2);

    let new_batch: Vec<TrackDto> =
        (10..13).map(track).collect();
    queue.replace(new_batch, 1);

    assert_eq!(id_of(&queue), Some("11".to_owned()));
    assert_eq!(
        queue.next(0).map(|t| t.id.clone()),
        Some("12".to_owned()),
        "next 该走新批,不是旧批"
    );
}

/// 列表循环回卷一次,轮次加一。
///
/// 轮次与那一轮的排列要一起报给服务端:随机开着时每一轮重新洗
/// (`docs/adr/0031` 六),只报排列的话服务端分不清「又洗了一次」与
/// 「还没动」—— 两轮洗出同一个排列虽然少见,但不是不可能。
#[test]
fn rewinding_counts_a_new_round() {
    let mut queue = Queue::new(vec![track(1), track(2)], 0);
    queue.set_loop_mode(LoopMode::All);

    assert_eq!(queue.round(), 0, "还没回卷过");
    queue.next(1);
    queue.next(1);

    assert_eq!(queue.round(), 1, "回卷一次就是第二轮");
}

/// 换一批把轮次清零 —— 它是这一批的属性,不是用户意图。
#[test]
fn replacing_the_batch_resets_the_round() {
    let mut queue = Queue::new(vec![track(1), track(2)], 0);
    queue.set_loop_mode(LoopMode::All);
    queue.next(1);
    queue.next(1);

    queue.replace(vec![track(3)], 0);

    assert_eq!(queue.round(), 0);
}

/// 播放次序读得出来,而且**关随机时就是原序**。
///
/// 报给服务端的是这一份(`docs/adr/0031` 六:显式保存排列,不靠 seed 猜)。
#[test]
fn the_play_order_is_readable_and_starts_as_the_batch_order()
 {
    let queue =
        Queue::new(vec![track(1), track(2), track(3)], 0);

    assert_eq!(queue.order(), [0, 1, 2]);
}

/// 从队列页点一行:跳过去,而**排列一动不动**。
///
/// 走 `replace` 的话会把随机清掉再重洗,于是点一行顺带换掉了播放次序 ——
/// 用户点的是「放这一首」,不是「重洗一次」。
#[test]
fn jumping_keeps_the_shuffled_order() {
    let mut queue = Queue::new(
        vec![track(1), track(2), track(3), track(4)],
        0,
    );
    queue.shuffle(7);
    let before: Vec<usize> = queue.order().to_vec();

    let landed = queue.jump_to(2).map(|t| t.id.clone());

    assert_eq!(landed, Some(track(3).id));
    assert_eq!(queue.order(), &before[..], "排列不该被动");
    assert!(queue.is_shuffled(), "随机也不该被关掉");
}

/// 随机开着时也跳得准:找的是它在排列里的位置,不是批序下标。
#[test]
fn jumping_finds_the_track_inside_the_shuffled_order() {
    let mut queue =
        Queue::new(vec![track(1), track(2), track(3)], 0);
    queue.shuffle(42);

    queue.jump_to(1);

    assert_eq!(queue.index(), 1, "跳到的就是批里第 1 首");
}

/// 越界不动,也不 panic —— 那一下来自界面上的一次点击,而列表随时会被换掉。
#[test]
fn jumping_past_the_end_does_nothing() {
    let mut queue = Queue::new(vec![track(1)], 0);

    assert!(queue.jump_to(9).is_none());
    assert_eq!(queue.index(), 0);
}

/// 换批才算新的一批;洗牌、跳转、推进都还是这一批(#137 ⑥:队列页凭它决定要不要重建行)。
#[test]
fn only_replacing_starts_a_new_batch() {
    let mut queue = Queue::new(batch(3), 0);
    let first = queue.batch();

    queue.shuffle(7);
    let _ = queue.jump_to(2);
    let _ = queue.next(7);
    assert_eq!(queue.batch(), first, "同一批里挪动不算换批");

    queue.replace(batch(3), 0);
    assert_ne!(queue.batch(), first, "换一批(哪怕内容一样)就是新的一批");
}
