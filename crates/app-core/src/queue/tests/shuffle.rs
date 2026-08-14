use similar_asserts::assert_eq;

use super::super::*;
use super::{batch, id_of, track};

/// 队列自己记得洗没洗过 —— 界面上那个开关只是它的投影。
#[test]
fn a_shuffled_queue_says_it_is_shuffled() {
    let mut queue = Queue::new(batch(4), 0);
    assert!(!queue.is_shuffled(), "刚建的批是原序");

    queue.shuffle(42);

    assert!(queue.is_shuffled());
}

/// 关随机之后不再是随机的。
#[test]
fn unshuffle_clears_the_flag() {
    let mut queue = Queue::new(batch(4), 0);
    queue.shuffle(42);

    queue.unshuffle();

    assert!(!queue.is_shuffled());
}

/// **边界:一首歌的批洗不动,但开关照样是开的。**
///
/// 洗牌在那时是空操作,而"用户开着随机"是另一回事。报成关的,
/// 界面上的开关会自己弹回去。
#[test]
fn a_single_track_batch_is_still_marked_shuffled() {
    let mut queue = Queue::new(batch(1), 0);

    queue.shuffle(42);

    assert!(
        queue.is_shuffled(),
        "洗不动是这一批的事,不是开关的事"
    );
}

/// 换一批把随机清掉:新批的次序是原序,调用方要重新洗。
#[test]
fn replacing_the_batch_clears_the_flag() {
    let mut queue = Queue::new(batch(4), 0);
    queue.shuffle(42);

    queue.replace(batch(4), 0);

    assert!(
        !queue.is_shuffled(),
        "新批还没洗过 —— 说它是随机的就是撒谎"
    );
}

/// 洗牌是**排列**:每首恰好出现一次,谁也不缺、谁也不重。
#[test]
fn shuffle_covers_every_track_exactly_once() {
    let mut queue = Queue::new(batch(10), 0);
    queue.shuffle(42);

    let mut heard =
        vec![id_of(&queue).expect("有当前曲目")];
    heard.extend(core::iter::from_fn(|| {
        queue.next(0).map(|t| t.id.clone())
    }));

    heard.sort();
    let mut expected: Vec<String> =
        (0..10).map(|i| i.to_string()).collect();
    expected.sort();
    assert_eq!(heard, expected);
}

/// 开随机不打断当前这首。
#[test]
fn shuffle_keeps_the_current_track_playing() {
    let mut queue = Queue::new(batch(10), 3);

    queue.shuffle(42);

    assert_eq!(id_of(&queue), Some("3".to_owned()));
}

/// **已放过的不再播放**:开随机之前放过的歌,这一轮里不会再出现。
#[test]
fn shuffle_skips_tracks_already_played() {
    let mut queue = Queue::new(batch(10), 0);
    queue.next(0); // 放过 0、1,正在放 1
    queue.shuffle(42);

    let rest: Vec<String> = core::iter::from_fn(|| {
        queue.next(0).map(|t| t.id.clone())
    })
    .collect();

    assert!(
        !rest.contains(&"0".to_owned())
            && !rest.contains(&"1".to_owned()),
        "已放过的 0、1 不该再出现,实得 {rest:?}"
    );
    assert_eq!(rest.len(), 8, "剩下的 8 首一首不少");
}

/// 关随机回到批的原始顺序,从当前曲目所在处继续。
#[test]
fn unshuffle_resumes_batch_order_after_current() {
    let mut queue = Queue::new(batch(10), 0);
    queue.shuffle(42);
    queue.next(0); // 随机走到某一首

    let current = id_of(&queue).expect("有当前曲目");
    queue.unshuffle();

    assert_eq!(
        id_of(&queue),
        Some(current.clone()),
        "关随机不打断当前曲目"
    );
    let expected_next: usize =
        current.parse::<usize>().unwrap() + 1;
    assert_eq!(
        queue.next(0).map(|t| t.id.clone()),
        Some(expected_next.to_string()),
        "接下来按批序走"
    );
}

/// 边界:空批。什么都放不了,但也不 panic。
#[test]
fn empty_batch_yields_nothing() {
    let mut queue = Queue::new(Vec::new(), 0);

    assert!(queue.current().is_none());
    assert!(queue.next(0).is_none());
    assert!(queue.previous().is_none());
    queue.shuffle(1); // 不该 panic
    queue.unshuffle();
}

/// 边界:单曲批。next 即结束,shuffle 是无操作。
#[test]
fn single_track_batch_ends_after_one() {
    let mut queue = Queue::new(batch(1), 0);

    assert!(queue.next(0).is_none());
    queue.shuffle(7);
    assert_eq!(id_of(&queue), Some("0".to_owned()));
}

/// 不同种子给出不同排列(批足够大时)。守住"seed 真的进了洗牌",
/// 防止实现里把 seed 忘在一边 —— 那样"随机"永远是同一个顺序。
#[test]
fn different_seeds_give_different_orders() {
    let walk = |seed: u64| -> Vec<String> {
        let mut queue = Queue::new(batch(20), 0);
        queue.shuffle(seed);
        core::iter::from_fn(|| {
            queue.next(0).map(|t| t.id.clone())
        })
        .collect()
    };

    assert_ne!(
        walk(1),
        walk(2),
        "20 首的批,两个种子洗出同一个排列几乎不可能 —— 多半是 seed 没被用上"
    );
}
