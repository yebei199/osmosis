//! 组的全局播放状态,纯规则(#142)。
//!
//! 时间一律由调用方传进来(挂钟微秒),不读钟、不碰库,所以每条规则都能不睡觉地测
//! (测试在 `super::tests`)。持久化在 `crate::store::group`,接线在 `super`。
//!
//! 时间线只有一种写法:「挂钟 `anchor_wall_us` 这一刻,媒体在 `position_us`」。播放时
//! 锚点可以落在未来 —— 起播、切歌、跳转、继续都锚在「现在 + [`LEAD_US`]」,
//! 让状态先到各台出声设备手上,大家在那一刻一起出声。

use contract::LoopModeDto;

/// 起播、切歌、跳转、继续时,锚点定在「现在」之后多久(与 #137 ⑤ 的 `app_core::LEAD_US` 同值)。
pub const LEAD_US: i64 = 500_000;

/// 「上一首」在这一首放过多久之后改成回到开头。
pub const RESTART_AFTER_US: u64 = 3_000_000;

/// 组里此刻放的是什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Now {
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    pub playing: bool,
    /// 挂钟 `anchor_wall_us` 这一刻,媒体在第 `position_us` 微秒。
    pub position_us: u64,
    pub anchor_wall_us: i64,
    pub shuffled: bool,
    pub loop_mode: LoopModeDto,
    /// 随机时的播放次序(条目号)。不随机时是空的,次序就是队列原序。
    pub order: Vec<i64>,
}

/// 组的全局状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Group {
    /// 每应用一条意图加一。组散了也不清零 —— 下一个组接着往上数。
    pub version: i64,
    pub members: Vec<String>,
    pub outputs: Vec<String>,
    pub now: Option<Now>,
}

/// 组队列那一版:条目号与时长(微秒),按队列原序。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playlist {
    pub entries: Vec<(i64, u64)>,
}

impl Playlist {
    pub fn duration_of(
        &self,
        entry_id: i64,
    ) -> Option<u64> {
        self.entries
            .iter()
            .find(|(id, _)| *id == entry_id)
            .map(|(_, duration)| *duration)
    }

    fn contains(&self, entry_id: i64) -> bool {
        self.duration_of(entry_id).is_some()
    }

    fn natural(&self) -> Vec<i64> {
        self.entries.iter().map(|(id, _)| *id).collect()
    }
}

/// 意图被拒的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// 发意图的那台不在组里。
    NotMember,
    /// 组里还没有歌,控制条没什么可控。
    Idle,
    /// 要切到的那一条不在组队列这一版里。
    NoSuchEntry,
}

impl Now {
    /// 挂钟 `now` 这一刻媒体在哪。锚点之前与暂停时停在锚点那个位置;不超过这一首的长度。
    pub fn position_at(
        &self,
        now: i64,
        duration_us: u64,
    ) -> u64 {
        if !self.playing || now <= self.anchor_wall_us {
            return self.position_us.min(duration_us);
        }
        (self.position_us
            + (now - self.anchor_wall_us) as u64)
            .min(duration_us)
    }

    /// 这一首放完的那一刻(挂钟)。暂停时没有。
    pub fn boundary(
        &self,
        duration_us: u64,
    ) -> Option<i64> {
        self.playing.then(|| {
            self.anchor_wall_us
                + duration_us
                    .saturating_sub(self.position_us)
                    as i64
        })
    }

    /// 从这一刻起,放第 `entry_id` 条的开头。
    fn start(&mut self, entry_id: i64, at: i64) {
        self.entry_id = entry_id;
        self.position_us = 0;
        self.anchor_wall_us = at;
        self.playing = true;
    }

    /// 播放次序:随机时是记下的那份,否则是队列原序。
    fn sequence(&self, list: &Playlist) -> Vec<i64> {
        if self.shuffled && !self.order.is_empty() {
            self.order.clone()
        } else {
            list.natural()
        }
    }

    /// 次序里相邻的那一条。`step` 为 1 是下一首、-1 是上一首;到头时列表循环就绕回去,
    /// 否则没有。
    fn neighbour(
        &self,
        list: &Playlist,
        step: isize,
    ) -> Option<i64> {
        let sequence = self.sequence(list);
        let at = sequence
            .iter()
            .position(|id| *id == self.entry_id)?
            as isize;
        let len = sequence.len() as isize;
        let next = at + step;
        let index = if (0..len).contains(&next) {
            next
        } else if self.loop_mode == LoopModeDto::All {
            next.rem_euclid(len)
        } else {
            return None;
        };
        sequence.get(index as usize).copied()
    }

    /// 自然放完之后接哪一条:单曲循环是它自己,其余照次序。
    pub fn follower(&self, list: &Playlist) -> Option<i64> {
        if self.loop_mode == LoopModeDto::One {
            return Some(self.entry_id);
        }
        self.neighbour(list, 1)
    }
}

impl Group {
    pub fn includes(&self, device: &str) -> bool {
        self.members.iter().any(|id| id == device)
    }

    /// 组散了:没有成员。
    pub fn is_vacant(&self) -> bool {
        self.members.is_empty()
    }

    fn member(&self, device: &str) -> Result<(), Refusal> {
        if self.includes(device) {
            Ok(())
        } else {
            Err(Refusal::NotMember)
        }
    }

    fn now_mut(&mut self) -> Result<&mut Now, Refusal> {
        self.now.as_mut().ok_or(Refusal::Idle)
    }

    /// 暂停,位置停在挂钟 `at` 那一刻。
    pub fn pause(
        &mut self,
        device: &str,
        list: &Playlist,
        at: i64,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        let now = self.now_mut()?;
        halt(now, list, at);
        Ok(())
    }

    /// 从停着的位置接着放,锚在 `at` 之后 [`LEAD_US`]。放到队尾停下的,从这一首开头重放。
    pub fn resume(
        &mut self,
        device: &str,
        list: &Playlist,
        at: i64,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        let now = self.now_mut()?;
        if now.playing {
            return Ok(());
        }
        let duration =
            list.duration_of(now.entry_id).unwrap_or(0);
        if now.position_us >= duration {
            now.position_us = 0;
        }
        now.playing = true;
        now.anchor_wall_us = at + LEAD_US;
        Ok(())
    }

    /// 跳到 `position_us`。
    pub fn seek(
        &mut self,
        device: &str,
        position_us: u64,
        at: i64,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        let now = self.now_mut()?;
        now.position_us = position_us;
        now.anchor_wall_us =
            if now.playing { at + LEAD_US } else { at };
        Ok(())
    }

    /// 下一首(`step` = 1)或上一首(-1)。到头又不循环时:下一首不动,上一首回到开头。
    /// 「上一首」在这一首已经放过 [`RESTART_AFTER_US`] 时也是回到开头。
    pub fn step(
        &mut self,
        device: &str,
        list: &Playlist,
        step: isize,
        at: i64,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        let now = self.now_mut()?;
        let duration =
            list.duration_of(now.entry_id).unwrap_or(0);
        let played = now.position_at(at, duration);
        let target =
            if step < 0 && played > RESTART_AFTER_US {
                Some(now.entry_id)
            } else {
                now.neighbour(list, step)
            };
        match target {
            Some(entry_id) => {
                now.start(entry_id, at + LEAD_US)
            }
            None if step < 0 => {
                let entry_id = now.entry_id;
                now.start(entry_id, at + LEAD_US);
            }
            None => {}
        }
        Ok(())
    }

    /// 切到组队列 `queue` 那一版(`list`)的第 `entry_id` 条,从头放。
    pub fn jump(
        &mut self,
        device: &str,
        queue: (i64, i64),
        list: &Playlist,
        entry_id: i64,
        at: i64,
        seed: u64,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        if !list.contains(entry_id) {
            return Err(Refusal::NoSuchEntry);
        }
        self.place(
            queue,
            list,
            entry_id,
            (0, true),
            at + LEAD_US,
            seed,
        );
        Ok(())
    }

    /// 组从发起那台本机正在放的那一份接着放(`position_us`、在不在放),锚在 `at`。
    pub fn seed(
        &mut self,
        queue: (i64, i64),
        list: &Playlist,
        entry_id: i64,
        from: (u64, bool),
        at: i64,
    ) -> Result<(), Refusal> {
        if !list.contains(entry_id) {
            return Err(Refusal::NoSuchEntry);
        }
        self.place(queue, list, entry_id, from, at, 0);
        Ok(())
    }

    /// 摆上一条:沿用手上的随机与循环开关;换了一批就重洗次序。
    fn place(
        &mut self,
        queue: (i64, i64),
        list: &Playlist,
        entry_id: i64,
        (position_us, playing): (u64, bool),
        anchor_wall_us: i64,
        seed: u64,
    ) {
        let (queue_id, revision) = queue;
        let held = self.now.take();
        let (shuffled, loop_mode) = held
            .as_ref()
            .map_or((false, LoopModeDto::Off), |now| {
                (now.shuffled, now.loop_mode)
            });
        let same_batch = held.as_ref().is_some_and(|now| {
            (now.queue_id, now.revision)
                == (queue_id, revision)
        });
        let order = match held {
            Some(now) if same_batch => now.order,
            _ if shuffled => {
                shuffled_from(list, entry_id, seed)
            }
            _ => Vec::new(),
        };
        self.now = Some(Now {
            queue_id,
            revision,
            entry_id,
            playing,
            position_us,
            anchor_wall_us,
            shuffled,
            loop_mode,
            order,
        });
    }

    /// 随机开关。打开时从当前这一首起重洗,关上就回到原序。
    pub fn shuffle(
        &mut self,
        device: &str,
        list: &Playlist,
        on: bool,
        seed: u64,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        let now = self.now_mut()?;
        now.shuffled = on;
        now.order = if on {
            shuffled_from(list, now.entry_id, seed)
        } else {
            Vec::new()
        };
        Ok(())
    }

    pub fn set_loop(
        &mut self,
        device: &str,
        mode: LoopModeDto,
    ) -> Result<(), Refusal> {
        self.member(device)?;
        self.now_mut()?.loop_mode = mode;
        Ok(())
    }

    /// 改在 `outputs` 这几台出声。发的那台与新的出声设备都成为成员。
    pub fn set_outputs(
        &mut self,
        device: &str,
        outputs: Vec<String>,
    ) {
        let mut outputs = outputs;
        dedup(&mut outputs);
        for id in std::iter::once(device.to_owned())
            .chain(outputs.iter().cloned())
        {
            if !self.includes(&id) {
                self.members.push(id);
            }
        }
        self.outputs = outputs;
    }

    /// 本机退出组。它自己停;其余出声设备照放(掉线规则在 [`Self::pause_if_silent`])。
    pub fn leave(&mut self, device: &str) {
        self.members.retain(|id| id != device);
        self.outputs.retain(|id| id != device);
        if self.members.is_empty() {
            self.outputs.clear();
            self.now = None;
        }
    }

    /// 在放却没有一台出声设备在线:立刻暂停,位置停在 `at`(用户:「设备退出了首先
    /// 自动暂停,不要一直放」)。返回有没有改。
    pub fn pause_if_silent(
        &mut self,
        list: &Playlist,
        online: impl Fn(&str) -> bool,
        at: i64,
    ) -> bool {
        let sounding =
            self.outputs.iter().any(|id| online(id));
        match self.now.as_mut() {
            Some(now) if now.playing && !sounding => {
                halt(now, list, at);
                true
            }
            _ => false,
        }
    }

    /// 放完的往下推:挂钟 `at` 之前该换的每一首都换过去,换到的那一首锚在它真正开始的
    /// 那一刻。不循环又放到队尾就停在最后一首的末尾。返回有没有改。
    pub fn roll(
        &mut self,
        list: &Playlist,
        at: i64,
    ) -> bool {
        let Some(now) = self.now.as_mut() else {
            return false;
        };
        let mut rolled = false;
        // 一首时长为 0 的歌会让这个循环原地打转,次数上限兜住它。
        for _ in 0..list.entries.len().max(1) {
            let duration =
                list.duration_of(now.entry_id).unwrap_or(0);
            let Some(end) = now.boundary(duration) else {
                break;
            };
            if at < end {
                break;
            }
            rolled = true;
            match now.follower(list) {
                Some(entry_id) => now.start(entry_id, end),
                None => {
                    now.playing = false;
                    now.position_us = duration;
                    now.anchor_wall_us = end;
                    break;
                }
            }
        }
        rolled
    }
}

/// 停下,位置取 `at` 那一刻。
fn halt(now: &mut Now, list: &Playlist, at: i64) {
    let duration =
        list.duration_of(now.entry_id).unwrap_or(u64::MAX);
    now.position_us = now.position_at(at, duration);
    now.anchor_wall_us = at;
    now.playing = false;
}

fn dedup(ids: &mut Vec<String>) {
    let mut seen = Vec::new();
    ids.retain(|id| {
        if seen.contains(id) {
            false
        } else {
            seen.push(id.clone());
            true
        }
    });
}

/// 从 `first` 起的一份随机次序:它排第一,其余打乱。
///
/// 不引随机数 crate:这里只要「每次洗得不一样」,调用方给的种子(挂钟纳秒)已经够乱,
/// xorshift 把它摊开。
fn shuffled_from(
    list: &Playlist,
    first: i64,
    seed: u64,
) -> Vec<i64> {
    let mut rest: Vec<i64> = list
        .natural()
        .into_iter()
        .filter(|id| *id != first)
        .collect();
    let mut state = seed | 1;
    for index in (1..rest.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let pick = (state % (index as u64 + 1)) as usize;
        rest.swap(index, pick);
    }
    std::iter::once(first).chain(rest).collect()
}
