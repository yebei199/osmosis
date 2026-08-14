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
