//! 播放队列:当前那一批歌与其中的位置(见 `CONTEXT.md`「队列」)。
//!
//! 批就是音乐页最近一次装进列表的东西,换一批就整个换队列。
//! 随机是对整批洗一次牌,一轮内不重复。循环关着时放完即停,不自作主张
//! 再来一轮;列表循环在队尾回卷(随机开着就重洗一轮),单曲循环只管
//! 播完时的自动推进,手动「下一首」照样前进。
//!
//! 洗牌的随机种子由调用方传入:本 crate 要能编到 wasm 且保持确定性可测,
//! 不引 `rand` —— `ui` 那侧用 `RandomState` 造种子,一行的事。

use contract::TrackDto;

/// 循环模式。三态与 MPRIS 的 `LoopStatus`(None/Playlist/Track)
/// 及安卓 `REPEAT_MODE` 一一对应,上报时无损。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopMode {
    /// 放完即停,队列的原始语义。
    #[default]
    Off,
    /// 列表循环:队尾回卷再来一轮。
    All,
    /// 单曲循环:只管自动推进,手动「下一首」照样前进。
    One,
}

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
    /// 循环模式。与 `shuffled` 不同,它是用户意图而不是批的属性:
    /// 换批([`Self::replace`])保留它,跟人不跟批。
    loop_mode: LoopMode,
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
            loop_mode: LoopMode::Off,
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
        // 循环模式跟人不跟批:它是用户意图,换批不该把它拨回去。
        let loop_mode = self.loop_mode;
        *self = Self::new(tracks, start);
        self.loop_mode = loop_mode;
    }

    /// 正在放的那首。空批时是 `None`。
    pub fn current(&self) -> Option<&TrackDto> {
        self.tracks.get(*self.order.get(self.cursor)?)
    }

    /// 手动「下一首」。循环关着或单曲循环时**放完即停**:队尾之后是
    /// `None`,位置不动;列表循环时队尾回卷再来一轮,`seed` 供回卷重洗
    /// (没开随机就用不上)。
    ///
    /// 名字就叫 `next`:它是控制条上那个「下一首」,领域词优先。
    /// 不实现 `Iterator` —— 队列可以 `previous` 回头,迭代器语义反而是误导。
    #[expect(
        clippy::should_implement_trait,
        reason = "领域动作「下一首」,非迭代器;可回头的队列不该长得像 Iterator"
    )]
    pub fn next(&mut self, seed: u64) -> Option<&TrackDto> {
        if self.cursor + 1 >= self.order.len() {
            if self.loop_mode != LoopMode::All
                || self.order.is_empty()
            {
                return None;
            }
            self.rewind(seed);
            return self.current();
        }
        self.cursor += 1;
        self.current()
    }

    /// 播完一首时的自动推进。与手动 [`Self::next`] 只差一处:
    /// 单曲循环时留在本曲重放,手动则照样前进。
    pub fn advance_auto(
        &mut self,
        seed: u64,
    ) -> Option<&TrackDto> {
        if self.loop_mode == LoopMode::One {
            return self.current();
        }
        self.next(seed)
    }

    /// 回卷:列表循环在队尾之后开新一轮。
    ///
    /// 随机开着就重洗**整批** —— 每一轮都是新排列,「随机」不退化成
    /// 固定的循环序。新一轮里整批都算未放过,所以不走 [`Self::shuffle`]
    /// 的"只洗未放段"。
    fn rewind(&mut self, seed: u64) {
        self.cursor = 0;
        if !self.shuffled {
            return;
        }
        self.order = (0..self.tracks.len()).collect();
        let mut state = seed;
        for i in (1..self.order.len()).rev() {
            let j =
                (splitmix(&mut state) as usize) % (i + 1);
            self.order.swap(i, j);
        }
    }

    /// 循环模式。真相住在这儿,界面与系统媒体控件都只是投影。
    pub fn loop_mode(&self) -> LoopMode {
        self.loop_mode
    }

    /// 设循环模式。
    pub fn set_loop_mode(&mut self, mode: LoopMode) {
        self.loop_mode = mode;
    }

    /// 下一首是谁,但**不推进**队列。
    ///
    /// 预取要用:备下一首的时候当前这首还在放,游标一动 `current()` 就跟着变,
    /// 界面和播放器都会以为已经换歌了。判据与 [`Self::advance_auto`] 一致:
    /// 单曲循环预取的就是本曲;队尾之后循环关着是 `None`;列表循环+未随机
    /// 是批序第一首;列表循环+随机也是 `None` —— 下一轮的次序回卷时才洗
    /// 出来,预取不假装知道。
    pub fn peek_next(&self) -> Option<&TrackDto> {
        if self.loop_mode == LoopMode::One {
            return self.current();
        }
        if let Some(&index) =
            self.order.get(self.cursor + 1)
        {
            return self.tracks.get(index);
        }
        if self.loop_mode == LoopMode::All && !self.shuffled
        {
            return self.tracks.get(*self.order.first()?);
        }
        None
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
mod tests;
