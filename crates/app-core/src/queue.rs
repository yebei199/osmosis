//! 播放队列:当前那一批歌与其中的位置(见 `CONTEXT.md`「队列」)。
//!
//! 批就是音乐页最近一次装进列表的东西,换一批就整个换队列。
//! 随机是对整批洗一次牌,一轮内不重复,放完即停 —— 不自作主张再来一轮。
//!
//! 洗牌的随机种子由调用方传入:本 crate 要能编到 wasm 且保持确定性可测,
//! 不引 `rand` —— `ui` 那侧用 `RandomState` 造种子,一行的事。

use contract::TrackDto;

/// 当前播放队列。
///
/// `order` 是"播放次序":顺序模式下是恒等排列,随机模式下是洗过的排列。
/// 曲目本身永远按批的原始顺序存在 `tracks` 里 —— 关随机要能回到原序,
/// 原序就不能在洗牌时被破坏。
#[derive(Debug, Default)]
pub struct Queue {
    tracks: Vec<TrackDto>,
    /// 播放次序,存 `tracks` 的下标。
    order: Vec<usize>,
    /// 在 `order` 里的位置。`order` 非空时恒有效。
    cursor: usize,
    /// 这一批洗过没有。
    ///
    /// 真相住在这儿而不是界面上:接进系统媒体控件之后,拨开关的人可能在锁屏、
    /// 在桌面外壳、也可能在 app 自己的控制条上,而它们都只是这一位的投影。
    /// 存两份的话,总有一份会先变。
    shuffled: bool,
}

impl Queue {
    /// 以一批歌建队列,从第 `start` 首开始放。
    ///
    /// `start` 越界时压到最后一首:它来自界面上的一次点击,越界只可能是
    /// 列表刚被替换的竞态,放最后一首总比 panic 强。
    pub fn new(
        tracks: Vec<TrackDto>,
        start: usize,
    ) -> Self {
        let order: Vec<usize> = (0..tracks.len()).collect();
        let cursor =
            start.min(tracks.len().saturating_sub(1));
        Self {
            tracks,
            order,
            cursor,
            shuffled: false,
        }
    }

    /// 换一批:整个队列被替换,旧批与旧次序消失,**随机也跟着清掉**。
    ///
    /// 新批还没洗过,说它是随机的就是撒谎。开着随机换批的话,调用方在
    /// `replace` 之后再补一次 [`Self::shuffle`] —— 那一下顺带把标志立回去。
    pub fn replace(
        &mut self,
        tracks: Vec<TrackDto>,
        start: usize,
    ) {
        *self = Self::new(tracks, start);
    }

    /// 正在放的那首。空批时是 `None`。
    pub fn current(&self) -> Option<&TrackDto> {
        self.tracks.get(*self.order.get(self.cursor)?)
    }

    /// 推进到下一首。**放完即停**:队尾之后是 `None`,位置不动。
    ///
    /// 名字就叫 `next`:它是控制条上那个「下一首」,领域词优先。
    /// 不实现 `Iterator` —— 队列可以 `previous` 回头,迭代器语义反而是误导。
    #[expect(
        clippy::should_implement_trait,
        reason = "领域动作「下一首」,非迭代器;可回头的队列不该长得像 Iterator"
    )]
    pub fn next(&mut self) -> Option<&TrackDto> {
        if self.cursor + 1 >= self.order.len() {
            return None;
        }
        self.cursor += 1;
        self.current()
    }

    /// 下一首是谁,但**不推进**队列。
    ///
    /// 预取要用:备下一首的时候当前这首还在放,游标一动 `current()` 就跟着变,
    /// 界面和播放器都会以为已经换歌了。判据与 [`Self::next`] 一致 ——
    /// 队尾之后同样是 `None`,那时没有可预取的东西。
    pub fn peek_next(&self) -> Option<&TrackDto> {
        let index = self.order.get(self.cursor + 1)?;
        self.tracks.get(*index)
    }

    /// 回到刚才放过的那首。随机模式下也成立 —— 走的是 `order`,
    /// 不是批序减一。队首之前是 `None`,位置不动。
    pub fn previous(&mut self) -> Option<&TrackDto> {
        if self.cursor == 0 || self.order.is_empty() {
            return None;
        }
        self.cursor -= 1;
        self.current()
    }

    /// 开随机:洗**未放过**的那一段。
    ///
    /// 已放过的(含当前这首)留在原位 —— 这同时保证了两件事:
    /// 当前曲目不被打断,已放过的这一轮不再出现。
    pub fn shuffle(&mut self, seed: u64) {
        // 先立标志再看洗不洗得动:一首歌的批洗起来是空操作,而"用户开着随机"
        // 是另一回事。报成关的,界面上的开关会自己弹回去。
        self.shuffled = true;
        if self.order.len() < 2 {
            return;
        }
        let unplayed = &mut self.order[self.cursor + 1..];

        // Fisher-Yates + splitmix64。手写而不引 `rand`:本 crate 要编到 wasm,
        // 而这里要的只是"一个由 seed 完全决定的排列",两个函数就够。
        let mut state = seed;
        for i in (1..unplayed.len()).rev() {
            let j =
                (splitmix(&mut state) as usize) % (i + 1);
            unplayed.swap(i, j);
        }
    }

    /// 关随机:回到批的原始顺序,从当前曲目所在处继续。
    pub fn unshuffle(&mut self) {
        self.shuffled = false;
        let Some(&current) = self.order.get(self.cursor)
        else {
            return;
        };
        self.order = (0..self.tracks.len()).collect();
        self.cursor = current;
    }

    /// 这一批洗过没有。界面上那个开关与系统媒体控件上的都读它。
    pub fn is_shuffled(&self) -> bool {
        self.shuffled
    }
}

/// splitmix64:够小、够散、完全确定。不是密码学随机,洗歌单也不需要是。
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

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

        let walked: Vec<String> =
            core::iter::from_fn(|| {
                queue.next().map(|t| t.id.clone())
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
        queue.next();
        queue.next();

        assert_eq!(
            queue.previous().map(|t| t.id.clone()),
            Some("1".to_owned())
        );
    }

    /// **一轮内不重复,放完即停**:最后一首之后 next() 给 None,状态不变。
    #[test]
    fn next_at_the_end_returns_none_and_stays() {
        let mut queue = Queue::new(batch(3), 2);

        assert!(queue.next().is_none());
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
            queue.next().map(|t| t.id.clone()),
            Some("12".to_owned()),
            "next 该走新批,不是旧批"
        );
    }

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
            queue.next().map(|t| t.id.clone())
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
        queue.next(); // 放过 0、1,正在放 1
        queue.shuffle(42);

        let rest: Vec<String> = core::iter::from_fn(|| {
            queue.next().map(|t| t.id.clone())
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
        queue.next(); // 随机走到某一首

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
            queue.next().map(|t| t.id.clone()),
            Some(expected_next.to_string()),
            "接下来按批序走"
        );
    }

    /// 边界:空批。什么都放不了,但也不 panic。
    #[test]
    fn empty_batch_yields_nothing() {
        let mut queue = Queue::new(Vec::new(), 0);

        assert!(queue.current().is_none());
        assert!(queue.next().is_none());
        assert!(queue.previous().is_none());
        queue.shuffle(1); // 不该 panic
        queue.unshuffle();
    }

    /// 边界:单曲批。next 即结束,shuffle 是无操作。
    #[test]
    fn single_track_batch_ends_after_one() {
        let mut queue = Queue::new(batch(1), 0);

        assert!(queue.next().is_none());
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
                queue.next().map(|t| t.id.clone())
            })
            .collect()
        };

        assert_ne!(
            walk(1),
            walk(2),
            "20 首的批,两个种子洗出同一个排列几乎不可能 —— 多半是 seed 没被用上"
        );
    }
}
