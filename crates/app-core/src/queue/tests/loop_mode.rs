use similar_asserts::assert_eq;

use super::super::*;
use super::{batch, id_of, track};

/// 新队列循环是关的:「放完即停」语义原样成立,不因加字段而漂移。
#[test]
fn loop_mode_defaults_to_off() {
    let queue = Queue::new(batch(3), 0);

    assert_eq!(queue.loop_mode(), LoopMode::Off);
}

/// 设置后读回一致:真相住在 Queue,界面与媒体控件都只是投影。
#[test]
fn set_loop_mode_round_trips() {
    let mut queue = Queue::new(batch(3), 0);

    queue.set_loop_mode(LoopMode::One);
    assert_eq!(queue.loop_mode(), LoopMode::One);
    queue.set_loop_mode(LoopMode::All);
    assert_eq!(queue.loop_mode(), LoopMode::All);
}

/// 换一批保留循环模式:随机是批的属性(换批清掉),循环是用户意图,
/// 跟人不跟批。
#[test]
fn replacing_the_batch_keeps_the_loop_mode() {
    let mut queue = Queue::new(batch(3), 0);
    queue.set_loop_mode(LoopMode::All);

    queue.replace(batch(5), 0);

    assert_eq!(queue.loop_mode(), LoopMode::All);
}

/// 列表循环:队尾 next 回卷到次序的第一首,不是 None。
#[test]
fn next_at_the_end_wraps_when_looping_all() {
    let mut queue = Queue::new(batch(3), 2);
    queue.set_loop_mode(LoopMode::All);

    assert_eq!(
        queue.next(0).map(|t| t.id.clone()),
        Some("0".to_owned())
    );
    assert_eq!(id_of(&queue), Some("0".to_owned()));
}

/// 单曲循环不改手动语义:队尾手动 next 仍是 None(单曲只管自动推进)。
#[test]
fn manual_next_at_the_end_is_none_when_looping_one() {
    let mut queue = Queue::new(batch(3), 2);
    queue.set_loop_mode(LoopMode::One);

    assert!(queue.next(0).is_none());
    assert_eq!(id_of(&queue), Some("2".to_owned()));
}

/// 单曲循环:自动播完留在本曲,游标不动。
#[test]
fn advance_auto_repeats_current_when_looping_one() {
    let mut queue = Queue::new(batch(3), 1);
    queue.set_loop_mode(LoopMode::One);

    assert_eq!(
        queue.advance_auto(0).map(|t| t.id.clone()),
        Some("1".to_owned())
    );
    assert_eq!(id_of(&queue), Some("1".to_owned()));
}

/// 单曲循环只锁自动:手动 next 照样前进到下一首。
#[test]
fn manual_next_advances_even_when_looping_one() {
    let mut queue = Queue::new(batch(3), 0);
    queue.set_loop_mode(LoopMode::One);

    assert_eq!(
        queue.next(0).map(|t| t.id.clone()),
        Some("1".to_owned())
    );
}

/// 循环关着时自动推进与 next 同判据:队尾即停,既有语义不被新入口绕过。
#[test]
fn advance_auto_stops_at_the_end_when_loop_off() {
    let mut queue = Queue::new(batch(2), 1);

    assert!(queue.advance_auto(0).is_none());
    assert_eq!(id_of(&queue), Some("1".to_owned()));
}

/// 随机+列表循环回卷:新一轮重洗(不同种子给不同排列),
/// 且每首恰好出现一次。
#[test]
fn wrap_reshuffles_the_next_round_when_shuffled() {
    let round2 = |wrap_seed: u64| -> Vec<String> {
        let mut queue = Queue::new(batch(20), 0);
        queue.shuffle(42);
        queue.set_loop_mode(LoopMode::All);
        // 走完第一轮:current 加 19 次 next。
        for _ in 0..19 {
            queue.next(0);
        }
        // 回卷,第二轮从这里开始,取整轮 20 首。
        let mut ids = vec![
            queue
                .next(wrap_seed)
                .map(|t| t.id.clone())
                .expect("回卷之后该有歌"),
        ];
        for _ in 0..19 {
            ids.push(
                queue
                    .next(0)
                    .expect("第二轮未走完不该停")
                    .id
                    .clone(),
            );
        }
        ids
    };

    let a = round2(1);
    let b = round2(2);

    let mut sorted = a.clone();
    sorted.sort();
    let mut expected: Vec<String> =
        (0..20).map(|i| i.to_string()).collect();
    expected.sort();
    assert_eq!(sorted, expected, "第二轮是完整的一轮");
    assert_ne!(
        a, b,
        "不同回卷种子该给不同排列 —— 多半是 seed 没进重洗"
    );
}

/// 不随机时回卷回到批序第一首,顺序完整走第二轮。
/// 种子照传也不该把未洗过的队列搅乱。
#[test]
fn wrap_keeps_batch_order_when_not_shuffled() {
    let mut queue = Queue::new(batch(3), 2);
    queue.set_loop_mode(LoopMode::All);

    let walked: Vec<String> = (0..3)
        .map(|_| {
            queue.next(7).expect("循环着不该停").id.clone()
        })
        .collect();

    assert_eq!(walked, ["0", "1", "2"]);
}

/// 预取镜像自动推进:单曲循环预取本曲;列表循环+未随机队尾预取第一首;
/// 列表循环+随机队尾是 None —— 下一轮次序回卷时才洗出来,预取不假装知道。
#[test]
fn peek_next_mirrors_the_auto_advance_rule() {
    let mut one = Queue::new(batch(3), 1);
    one.set_loop_mode(LoopMode::One);
    assert_eq!(
        one.peek_next().map(|t| t.id.as_str()),
        Some("1"),
        "单曲循环预取本曲"
    );

    let mut all = Queue::new(batch(3), 2);
    all.set_loop_mode(LoopMode::All);
    assert_eq!(
        all.peek_next().map(|t| t.id.as_str()),
        Some("0"),
        "列表循环队尾预取第一首"
    );

    let mut shuffled = Queue::new(batch(5), 4);
    shuffled.shuffle(42);
    shuffled.set_loop_mode(LoopMode::All);
    assert!(
        shuffled.peek_next().is_none(),
        "随机的下一轮还没洗出来,预取不该假装知道"
    );
}

/// 边界:空批开循环不 panic 也不给歌;单曲批+列表循环队尾回卷到自己。
#[test]
fn loop_edge_cases_on_empty_and_single_track_batches() {
    let mut empty = Queue::new(Vec::new(), 0);
    empty.set_loop_mode(LoopMode::All);
    assert!(empty.next(0).is_none());
    assert!(empty.advance_auto(0).is_none());
    empty.set_loop_mode(LoopMode::One);
    assert!(empty.advance_auto(0).is_none());

    let mut single = Queue::new(batch(1), 0);
    single.set_loop_mode(LoopMode::All);
    assert_eq!(
        single.next(0).map(|t| t.id.clone()),
        Some("0".to_owned()),
        "单曲批的列表循环回卷到自己"
    );
}
