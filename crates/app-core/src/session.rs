//! 播放组会话,遥控器这一侧:组现在的成员、主端、进行中的那一次换输出，以及每台成员每一步的确认。
//!
//! **选设备等于迁移**(#137 ③):把当前队列、曲目与进度交给目标，本机或原来那台
//! 停止实际出声。它不是改一个路由标志 —— 改标志正是从前「选完设备本机那首接着
//! 响、控制条消失、被控端过很久才出声」的根。
//!
//! 一次换输出用同一个 `operation_id` 贯穿，最多三步:
//!
//! 1. **新来的准备**:取下执行副本、把那一条备到起点，不出声;
//! 2. **走的停止实际输出**,主端报停在哪一毫秒 —— 那一毫秒就是续播的锚点;
//! 3. **新来的开始**:整组换人时新主端从锚点开始、其余跟上;组里有留下的成员时组一直在响，
//!    新来的直接跟上共同计划(#137 ⑤);
//!
//! 然后**确认**:服务端把成员集合正式换过去。三个动作都落在这里(`docs/adr/0031`):
//! 「改在这些设备播放」是 [`Session::change`] 换一个集合,「加入一起播放」「移出」是在
//! 当前集合上加一台、减一台。集合差的语义：两边都有的成员继续播，新增的先准备，被移除的停止
//! 实际输出;移出最后一台之后没有任何输出，不自动回到本机，也不恢复被换掉的旧歌。
//!
//! 主端(持有组时间线、决定下一首的那一台)被排除在新集合之外时，交给留下的第一台 ——
//! 这是正常操作里的**显式交接**,不是故障容错:主端失联时这里什么都不做。
//!
//! 每一步只认**对的那台**报来的、**这一次**操作的回话。等不到回话时停在
//! 「待确认」上，由用户处理，不替用户猜：走的那台停没停不知道就不启动新来的(可能多一台
//! 在响),新主端起没起不知道就不恢复旧的(同样可能)。迟到的确认照收 ——
//! 收下它正是解开「待确认」的正路 —— 但同一步只推进一次，重试与迟到都不会让
//! 目标起播两遍。普通成员等不到开始确认的，照样算在集合里(它是被授权的那台),按成员逐台
//! 列在 [`Session::unconfirmed`] 里，不引入集合外的新输出，也不拖住别的成员。
//!
//! 这里只有规则，不发网络、不碰播放器：每个动作以 [`Effect`] 交回调用方执行，
//! 时间由调用方传进来(`docs/adr/0002`)。本机的「准备 / 停止 / 开始」与远端的
//! 一样走回话，只是那份回话由本机自己马上报回来。

use contract::{OperationAckDto, OperationPhase, TrackDto};

use crate::Output;

/// 目标准备最多等多久。
///
/// 准备 = 按标识分页取下整份执行副本(五千首是十页)+ 取直链、开流、预读。
/// 真机上取直链加开流一两秒(#121),十页队列再加几秒;二十秒还没好就当它
/// 准备不了 —— 这时候什么都还没动，放弃是安全的，源照旧在放。
pub const PREPARE_TIMEOUT_MS: u64 = 20_000;

/// 源停止最多等多久。
///
/// 停止只是按下播放器、报一个位置，一个来回的事。等不到就进「待确认」,而**不是**
/// 失败：停没停只有源自己知道，此刻替它下结论就会在两台一起响与一台都不响之间
/// 猜一个。
pub const STOP_TIMEOUT_MS: u64 = 5_000;

/// 目标开始最多等多久。
///
/// 开始 = 把备好的那一份交给播放器、跳到锚点。跳到还没下到的位置要重开一个
/// range 请求(`docs/adr/0019`),给它比停止宽一些的余量。
pub const START_TIMEOUT_MS: u64 = 8_000;

/// 整组换人时，新主端备好之后再等其余新成员多久(#137 ⑤)。
///
/// 产品规则：准备好的成员在有界等待后开始，慢的之后加入，不无限期等最慢那台。三秒盖得住
/// 同一局域网里几台设备取直链、开流的先后差(真机一两秒,#121),再长就是让已经备好的干等。
pub const READY_GRACE_MS: u64 = 3_000;

/// 要迁过去的那份播放：服务端哪个队列的哪一版、哪一条、从哪一毫秒、在不在放。
///
/// 队列只以标识过去(`docs/adr/0031`),目标自己按标识取整份执行副本。`track`
/// 是给界面画的 —— 迁移那几秒控制条上该一直是这一首，不该变成空白或者目标
/// 手上原来那一首。
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    /// 发起迁移那一刻源的位置。目标按它**预备**;真正的起点是源停下时报的
    /// 那一毫秒(见 [`Move::anchor_ms`])。
    pub position_ms: u64,
    /// 源在不在放。暂停着迁过去，目标也该停在起点等人按播放。
    pub playing: bool,
    pub track: TrackDto,
}

/// 这一次正在做哪一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Preparing,
    Stopping,
    Starting,
}

/// 等不到回话的是哪一步。两种分开，因为该拦的东西相反。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doubt {
    /// 走的那台停没停不知道 —— 所以**不启动新来的**。
    SourceStop,
    /// 新主端起没起不知道 —— 所以**不恢复旧的**。
    TargetStart,
}

/// 一次换输出此刻的状况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running(Step),
    /// 等过了头，停在这里由用户处理(重试或放弃)。
    Unconfirmed(Doubt),
}

/// 一台设备在这一次里是新来的还是要走的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Join,
    Leave,
}

/// 一台设备在这一次里走到哪了。界面据此逐台列出(AC-5.2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// 还没轮到它：要走的那台在等新来的备好。
    Waiting,
    Preparing,
    Prepared,
    Stopping,
    Stopped,
    Starting,
    /// 开始了(没有东西可放时：加入了)。
    Started,
    /// 确定的失败：它没出声(或者停不下来)。
    Failed(String),
    /// 等过了头：它出没出声、停没停，不知道。
    Unconfirmed,
}

impl Progress {
    /// 新来的这台有没有下文了 —— 开始了、失败了、或者等过了头。
    fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::Started
                | Self::Failed(_)
                | Self::Unconfirmed
        )
    }
}

/// 一台设备在这一次里的角色与进度。
#[derive(Debug, Clone, PartialEq)]
pub struct Party {
    pub output: Output,
    pub role: Role,
    pub progress: Progress,
    /// 它这一步等到几点为止。
    deadline_ms: u64,
}

/// 一次进行中的换输出。
#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub operation_id: String,
    /// 原来的主端。整组换人时锚点取它停下的位置。
    pub from: Output,
    /// 换过去之后的主端。
    pub to: Output,
    /// 什么都没在放时是 `None`:没有东西可迁，只停走的、换集合。
    pub plan: Option<Plan>,
    pub phase: Phase,
    /// 新来的与要走的，每台一行。
    pub parties: Vec<Party>,
    /// 换过去之后的集合，按用户选的顺序。
    set: Vec<Output>,
    /// 原本就在、这一次之后仍在的成员。非空时组一直在响：新来的直接跟上，不必定锚。
    kept: Vec<Output>,
    /// 这一步等到几点为止。
    deadline_ms: u64,
    /// 新来的最晚备到几点。
    prepare_by_ms: u64,
    /// 新主端备好的时刻 —— 其余新成员从这一刻起最多再等 [`READY_GRACE_MS`]。
    ready_at_ms: Option<u64>,
    /// 原主端停下时报的位置 —— 整组换人时新主端从这里开始。
    stopped_at_ms: Option<u64>,
}

impl Move {
    /// 新主端从哪一毫秒开始：原主端停下那一刻报的位置;没报位置就按发起时的。
    ///
    /// 锚点取「源停下的那一刻」而不是「按下选设备的那一刻」:准备要几秒，这几秒
    /// 里源一直在放，按发起时的位置开始就等于让用户把这几秒再听一遍。
    pub fn anchor_ms(&self) -> Option<u64> {
        let plan = self.plan.as_ref()?;
        Some(self.stopped_at_ms.unwrap_or(plan.position_ms))
    }

    /// 组里原本就有成员留下：组一直在响，这一次只是加人减人(界面据此换一种说法)。
    pub fn keeps_playing(&self) -> bool {
        !self.kept.is_empty()
    }

    /// 控制命令该不该先压着：新主端还没确认开始(整组换人)、或者主端正在交接。
    ///
    /// 组里有留下的主端、只是加人减人时不压 —— 它一直在响，暂停、切歌照常发给它。
    pub fn holds_transport(&self) -> bool {
        if self.kept.is_empty() {
            return self.essential().is_some_and(|party| {
                party.progress != Progress::Started
            });
        }
        !same(&self.from, &self.to)
    }
}

/// 交给调用方去执行的一个动作。
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// 告诉服务端：这一次操作之后成员集合是 `outputs`,主端是 `master`(设备 id;
    /// 只剩本机时不经服务端，是空集合)。
    Begin {
        operation_id: String,
        outputs: Vec<String>,
        master: Option<String>,
    },
    Prepare {
        operation_id: String,
        to: Output,
        plan: Plan,
    },
    Stop {
        operation_id: String,
        from: Output,
    },
    Start {
        operation_id: String,
        to: Output,
        position_ms: u64,
        playing: bool,
    },
    Cancel {
        operation_id: String,
        to: Output,
    },
    /// 告诉服务端：确认完了，成员集合正式换过去。`outputs` 是 `None` 就是登记时那一份;
    /// 有新来的准备不了、开始失败时，是去掉它们之后的那一份。
    Commit {
        operation_id: String,
        outputs: Option<Vec<String>>,
    },
    /// 告诉服务端：这一次作罢，成员集合不变。
    Abort {
        operation_id: String,
    },
}

/// 这一下为什么没开始一次换输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// 选的就是现在的集合。
    AlreadyThere,
    /// 上一次已经过了准备那一步(有设备在停、在起，或者在等用户处理),
    /// 半路换目标会让「谁在响」彻底说不清。
    Busy,
}

/// 遥控器手上的播放组会话。
#[derive(Debug)]
pub struct Session {
    /// 本机的设备 id。本机也是组员时要以它登记。
    me: Option<String>,
    /// 已经确认的成员。换输出确认之前**不**换 —— 那几秒里命令仍按它路由。
    /// 只有本机时是 `[Local]`;移出最后一台之后是空的。
    members: Vec<Output>,
    /// 已经确认的主端。命令发给它。
    master: Output,
    /// 服务端给的主端任期。成员集合每换一次加一。
    term: u64,
    moving: Option<Move>,
    /// 上一次换输出为什么没成(或者哪几台没跟上),给界面说一句。下一次开始时清掉。
    failure: Option<String>,
    /// 最后一次发出 `Commit` 的操作 —— 服务端回的任期只认它。
    committing: Option<String>,
    /// 提交时开始还没确认的新成员，以及是哪一次。迟到的开始确认把它划掉。
    unconfirmed: Option<(String, Vec<Output>)>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            me: None,
            members: vec![Output::Local],
            master: Output::Local,
            term: 0,
            moving: None,
            failure: None,
            committing: None,
            unconfirmed: None,
        }
    }
}

/// 两个输出是不是同一台：按设备 id 认，名字不算(远端的回话只带 id)。
fn same(a: &Output, b: &Output) -> bool {
    a.target() == b.target()
}

fn contains(set: &[Output], output: &Output) -> bool {
    set.iter().any(|member| same(member, output))
}

fn same_set(a: &[Output], b: &[Output]) -> bool {
    a.len() == b.len()
        && a.iter().all(|output| contains(b, output))
}

impl Session {
    /// 知道本机 id 的会话。本机要和别的设备一起出声，就得以这个 id 登记进组。
    pub fn with_me(me: impl Into<String>) -> Self {
        Self {
            me: Some(me.into()),
            ..Self::default()
        }
    }

    /// 已经确认的主端 —— 命令发给它。只剩本机、或者一台都没有时是本机。
    pub fn output(&self) -> &Output {
        &self.master
    }

    /// 已经确认的成员。
    pub fn members(&self) -> &[Output] {
        &self.members
    }

    pub fn moving(&self) -> Option<&Move> {
        self.moving.as_ref()
    }

    pub fn term(&self) -> u64 {
        self.term
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// 已经在集合里、开始却还没确认的成员(AC-5.2 逐台列出)。
    pub fn unconfirmed(&self) -> &[Output] {
        self.unconfirmed
            .as_ref()
            .map_or(&[], |(_, outputs)| outputs.as_slice())
    }

    /// 控制命令该不该先压着(见 [`Move::holds_transport`])。
    pub fn holds_transport(&self) -> bool {
        self.moving
            .as_ref()
            .is_some_and(Move::holds_transport)
    }

    /// 改在 `to` 这一台播放(③ 的选设备)。
    pub fn begin(
        &mut self,
        operation_id: String,
        to: Output,
        plan: Option<Plan>,
        now_ms: u64,
    ) -> Result<Vec<Effect>, Refused> {
        self.change(operation_id, vec![to], plan, now_ms)
    }

    /// 把成员集合换成 `set`:改在这些设备播放、加入一台、移出一台，都是它。
    ///
    /// 还在准备的上一次会被这一次顶掉：它的新成员收到取消，服务端那一次作罢 ——
    /// 用户改主意是正常操作。过了准备那一步就不许换了，见 [`Refused::Busy`]。
    pub fn change(
        &mut self,
        operation_id: String,
        set: Vec<Output>,
        plan: Option<Plan>,
        now_ms: u64,
    ) -> Result<Vec<Effect>, Refused> {
        let mut effects = Vec::new();
        match self.moving.take() {
            Some(old)
                if old.phase
                    == Phase::Running(Step::Preparing) =>
            {
                effects.extend(withdraw(&old));
            }
            Some(old) => {
                self.moving = Some(old);
                return Err(Refused::Busy);
            }
            None if same_set(&set, &self.members) => {
                return Err(Refused::AlreadyThere);
            }
            None => {}
        }
        // 顶掉之后又选回了原来的集合：上一次已经撤干净，这一下什么都不必换。
        if same_set(&set, &self.members) {
            return Ok(effects);
        }

        self.failure = None;
        let kept: Vec<Output> = set
            .iter()
            .filter(|output| {
                contains(&self.members, output)
            })
            .cloned()
            .collect();
        let master = if contains(&set, &self.master) {
            self.master.clone()
        } else {
            kept.first()
                .or(set.first())
                .cloned()
                .unwrap_or(Output::Local)
        };
        let joined = if plan.is_some() {
            Progress::Preparing
        } else {
            // 没有东西可放：新来的不必准备，直接算加入。
            Progress::Started
        };
        let mut parties: Vec<Party> = set
            .iter()
            .filter(|output| {
                !contains(&self.members, output)
            })
            .map(|output| Party {
                output: output.clone(),
                role: Role::Join,
                progress: joined.clone(),
                deadline_ms: now_ms + PREPARE_TIMEOUT_MS,
            })
            .collect();
        parties.extend(
            self.members
                .iter()
                .filter(|output| !contains(&set, output))
                .map(|output| Party {
                    output: output.clone(),
                    role: Role::Leave,
                    progress: Progress::Waiting,
                    deadline_ms: 0,
                }),
        );

        effects.push(Effect::Begin {
            operation_id: operation_id.clone(),
            outputs: self.encode(&set),
            master: self.encode_master(&set, &master),
        });
        let moving = Move {
            operation_id,
            from: self.master.clone(),
            to: master,
            plan,
            phase: Phase::Running(Step::Preparing),
            parties,
            set,
            kept,
            deadline_ms: now_ms + PREPARE_TIMEOUT_MS,
            prepare_by_ms: now_ms + PREPARE_TIMEOUT_MS,
            ready_at_ms: None,
            stopped_at_ms: None,
        };
        if let Some(plan) = &moving.plan {
            for party in &moving.parties {
                if party.role == Role::Join {
                    effects.push(Effect::Prepare {
                        operation_id: moving
                            .operation_id
                            .clone(),
                        to: party.output.clone(),
                        plan: plan.clone(),
                    });
                }
            }
        }
        self.moving = Some(moving);
        effects.extend(self.advance(now_ms));
        Ok(effects)
    }

    /// 收下一台设备对某次操作的回话。本机的回话由调用方以 `Output::Local` 报进来。
    ///
    /// 不是这一次的、不是该回这一步的那台报的、或者这一步已经过去了的，一概
    /// 不理 —— 迟到与重复都落在这里，所以同一步只会推进一次。
    pub fn on_ack(
        &mut self,
        from: &Output,
        ack: &OperationAckDto,
        now_ms: u64,
    ) -> Vec<Effect> {
        self.settle_unconfirmed(from, ack);
        let Some(moving) = self.moving.as_mut() else {
            return Vec::new();
        };
        if moving.operation_id != ack.operation_id {
            return Vec::new();
        }
        let is_old_master = same(from, &moving.from);
        let Some(party) = moving
            .parties
            .iter_mut()
            .find(|party| same(&party.output, from))
        else {
            return Vec::new();
        };
        let reason = || {
            ack.reason
                .clone()
                .unwrap_or_else(|| "没说原因".to_owned())
        };
        let next = match (
            party.role,
            &party.progress,
            ack.phase,
        ) {
            (
                Role::Join,
                Progress::Preparing,
                OperationPhase::Prepared,
            ) => Progress::Prepared,
            (
                Role::Join,
                Progress::Preparing,
                OperationPhase::Failed,
            )
            | (
                Role::Join,
                Progress::Starting | Progress::Unconfirmed,
                OperationPhase::Failed,
            )
            | (
                Role::Leave,
                Progress::Stopping | Progress::Unconfirmed,
                OperationPhase::Failed,
            ) => Progress::Failed(reason()),
            (
                Role::Join,
                Progress::Starting | Progress::Unconfirmed,
                OperationPhase::Started,
            ) => Progress::Started,
            (
                Role::Leave,
                Progress::Stopping | Progress::Unconfirmed,
                OperationPhase::Stopped,
            ) => {
                if is_old_master {
                    moving.stopped_at_ms = ack.position_ms;
                }
                Progress::Stopped
            }
            _ => return Vec::new(),
        };
        party.progress = next;
        self.advance(now_ms)
    }

    /// 时间到了没有：准备超时放弃，停止与开始超时进「待确认」。
    pub fn tick(&mut self, now_ms: u64) -> Vec<Effect> {
        self.advance(now_ms)
    }

    /// 用户在「待确认」上按了重试：同一个操作号把那一步再发一遍。
    ///
    /// 同一个操作号是要点：两端都按它去重，重发的停止不会再报一个新位置，
    /// 重发的开始不会让已经在响的目标从锚点再起一遍。
    pub fn retry(&mut self, now_ms: u64) -> Vec<Effect> {
        let Some(moving) = self.moving.as_mut() else {
            return Vec::new();
        };
        let Phase::Unconfirmed(doubt) = moving.phase else {
            return Vec::new();
        };
        let (role, doubted, step, wait) = match doubt {
            Doubt::SourceStop => (
                Role::Leave,
                Progress::Stopping,
                Step::Stopping,
                STOP_TIMEOUT_MS,
            ),
            Doubt::TargetStart => (
                Role::Join,
                Progress::Starting,
                Step::Starting,
                START_TIMEOUT_MS,
            ),
        };
        moving.phase = Phase::Running(step);
        moving.deadline_ms = now_ms + wait;
        let mut effects = Vec::new();
        let anchor = moving.anchor_ms().unwrap_or(0);
        let playing = moving
            .plan
            .as_ref()
            .is_some_and(|plan| plan.playing);
        for party in &mut moving.parties {
            if party.role != role
                || !matches!(
                    party.progress,
                    Progress::Unconfirmed
                        | Progress::Stopping
                        | Progress::Starting
                )
            {
                continue;
            }
            party.progress = doubted.clone();
            party.deadline_ms = now_ms + wait;
            effects.push(match role {
                Role::Leave => Effect::Stop {
                    operation_id: moving
                        .operation_id
                        .clone(),
                    from: party.output.clone(),
                },
                Role::Join => Effect::Start {
                    operation_id: moving
                        .operation_id
                        .clone(),
                    to: party.output.clone(),
                    position_ms: anchor,
                    playing,
                },
            });
        }
        effects
    }

    /// 用户在「待确认」上按了放弃。
    ///
    /// 两种放弃都**不让任何一台自动开始**。走的那台停止没确认时撤掉新来的准备，
    /// 集合不变;新主端开始没确认时叫新来的都停 —— 它若断着网就收不到，所以
    /// 界面上不能把这说成「已经静音」。
    pub fn abandon(&mut self) -> Vec<Effect> {
        let Some(moving) = self.moving.as_ref() else {
            return Vec::new();
        };
        let Phase::Unconfirmed(doubt) = moving.phase else {
            return Vec::new();
        };
        let (effects, why) = match doubt {
            Doubt::SourceStop => (
                withdraw(moving),
                "已放弃切换;原来那台是否停了不能确认",
            ),
            Doubt::TargetStart => {
                let mut effects: Vec<Effect> = moving
                    .parties
                    .iter()
                    .filter(|party| {
                        party.role == Role::Join
                            && matches!(
                                party.progress,
                                Progress::Starting
                                    | Progress::Started
                                    | Progress::Unconfirmed
                            )
                    })
                    .map(|party| Effect::Stop {
                        operation_id: moving
                            .operation_id
                            .clone(),
                        from: party.output.clone(),
                    })
                    .collect();
                effects.push(moving.abort());
                (
                    effects,
                    "已放弃切换;已叫目标停止,但它是否还在出声不能确认",
                )
            }
        };
        self.fail(why.to_owned());
        effects
    }

    /// 服务端确认成员集合换好了，带回新任期。
    pub fn committed(
        &mut self,
        operation_id: &str,
        term: u64,
    ) {
        if self.committing.as_deref() == Some(operation_id)
        {
            self.term = term;
        }
    }

    /// 服务端没登记下这一次(目标不在线之类):作废，记下原因。
    ///
    /// 这时什么都还没动 —— 目标没被锁上，源照旧在放 —— 所以不必撤任何东西。
    pub fn rejected(
        &mut self,
        operation_id: &str,
        reason: &str,
    ) {
        if self.moving.as_ref().is_some_and(|moving| {
            moving.operation_id == operation_id
        }) {
            self.fail(format!("换不过去: {reason}"));
        }
    }

    /// 这台设备在进行中的那一次里是哪一方(新来的或要走的),按设备 id 认。
    ///
    /// 远端的回话只带发信人的 id;拿它现造一个 `Output` 的话名字对不上，
    /// 界面上也就说不出是哪一台。
    pub fn party(&self, device_id: &str) -> Option<Output> {
        let moving = self.moving.as_ref()?;
        moving
            .parties
            .iter()
            .map(|party| &party.output)
            .find(|output| {
                output.target() == Some(device_id)
            })
            .cloned()
    }

    /// 这台设备是会话里的哪一台，按设备 id 认：进行中那一次的各方、提交时开始没确认的、
    /// 已经确认的成员，依次找。
    ///
    /// 回话随每条上报一直带着，提交之后才到的也有 —— 那正是解开逐台「待确认」的正路,
    /// 所以不能只在进行中那一次里找(见 [`Self::party`])。
    pub fn output_of(
        &self,
        device_id: &str,
    ) -> Option<Output> {
        self.party(device_id).or_else(|| {
            self.unconfirmed()
                .iter()
                .chain(&self.members)
                .find(|output| {
                    output.target() == Some(device_id)
                })
                .cloned()
        })
    }

    /// 输出被收回本机(失权、失联、接管失败):进行中的那一次一并作废。
    ///
    /// 返回本机此前是不是**不在**组里 —— 不在的话本机播放器是空的、状态机却还停在
    /// 进遥控之前,调用方要把它按停;本来就在组里一起出声的,不许按停(#142)。
    #[must_use]
    pub fn come_home(&mut self) -> bool {
        let was_silent =
            !self.members.contains(&Output::Local);
        self.members = vec![Output::Local];
        self.master = Output::Local;
        self.moving = None;
        self.unconfirmed = None;
        was_silent
    }

    /// 集合的登记形式：远端按 id,本机按本机 id;只剩本机(或者一台不剩)时不经服务端。
    fn encode(&self, set: &[Output]) -> Vec<String> {
        if set
            .iter()
            .all(|output| output.target().is_none())
        {
            return Vec::new();
        }
        set.iter()
            .map(|output| self.id_of(output))
            .collect()
    }

    fn encode_master(
        &self,
        set: &[Output],
        master: &Output,
    ) -> Option<String> {
        (!self.encode(set).is_empty())
            .then(|| self.id_of(master))
    }

    fn id_of(&self, output: &Output) -> String {
        output
            .target()
            .map(str::to_owned)
            .or_else(|| self.me.clone())
            .unwrap_or_default()
    }

    /// 迟到的开始确认把提交时没确认的那台划掉。
    fn settle_unconfirmed(
        &mut self,
        from: &Output,
        ack: &OperationAckDto,
    ) {
        let Some((operation_id, outputs)) =
            self.unconfirmed.as_mut()
        else {
            return;
        };
        if *operation_id != ack.operation_id {
            return;
        }
        if matches!(
            ack.phase,
            OperationPhase::Started
                | OperationPhase::Failed
        ) {
            outputs.retain(|output| !same(output, from));
        }
        if outputs.is_empty() {
            self.unconfirmed = None;
        }
    }

    /// 按此刻的回话与时间往前推：能进下一步就进，到点的判超时，推完为止。
    fn advance(&mut self, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        loop {
            let Some(moving) = self.moving.as_mut() else {
                return effects;
            };
            let before = moving.phase;
            let outcome = match before {
                Phase::Running(Step::Preparing) => {
                    moving.preparing(now_ms)
                }
                Phase::Running(Step::Stopping)
                | Phase::Unconfirmed(Doubt::SourceStop) => {
                    moving.stopping(now_ms)
                }
                Phase::Running(Step::Starting)
                | Phase::Unconfirmed(Doubt::TargetStart) => {
                    moving.starting(now_ms)
                }
            };
            let after = moving.phase;
            effects.extend(outcome.effects);
            match outcome.done {
                Some(Done::Commit) => {
                    effects.extend(self.commit());
                    return effects;
                }
                Some(Done::Fail(why)) => {
                    self.fail(why);
                    return effects;
                }
                None => {}
            }
            if after == before && !outcome.moved {
                return effects;
            }
        }
    }

    /// 新主端确认了(或者整组都有了下文):集合换过去，告诉服务端。
    fn commit(&mut self) -> Vec<Effect> {
        let Some(moving) = self.moving.take() else {
            return Vec::new();
        };
        let committed: Vec<Output> = moving
            .set
            .iter()
            .filter(|output| {
                contains(&moving.kept, output)
                    || moving.parties.iter().any(|party| {
                        same(&party.output, output)
                            && matches!(
                                party.progress,
                                Progress::Started
                                    | Progress::Unconfirmed
                            )
                    })
            })
            .cloned()
            .collect();
        let dropped: Vec<String> = moving
            .parties
            .iter()
            .filter_map(|party| match &party.progress {
                Progress::Failed(why)
                    if party.role == Role::Join =>
                {
                    Some(format!(
                        "{} 没能加入: {why}",
                        party
                            .output
                            .name()
                            .unwrap_or("本机")
                    ))
                }
                _ => None,
            })
            .collect();
        if !dropped.is_empty() {
            self.failure = Some(dropped.join(";"));
        }
        let doubted: Vec<Output> = moving
            .parties
            .iter()
            .filter(|party| {
                party.role == Role::Join
                    && party.progress
                        == Progress::Unconfirmed
            })
            .map(|party| party.output.clone())
            .collect();
        self.unconfirmed =
            (!doubted.is_empty()).then(|| {
                (moving.operation_id.clone(), doubted)
            });
        let outputs = (committed.len() != moving.set.len())
            .then(|| self.encode(&committed));
        self.master = if committed.is_empty() {
            Output::Local
        } else {
            moving.to.clone()
        };
        self.members = committed;
        self.committing = Some(moving.operation_id.clone());
        vec![Effect::Commit {
            operation_id: moving.operation_id,
            outputs,
        }]
    }

    /// 这一次没成：记一句为什么，进行中的那一次清掉，集合不动。
    fn fail(&mut self, why: String) {
        self.moving = None;
        self.failure = Some(why);
    }
}

/// 往前推一次的结果。
#[derive(Default)]
struct Outcome {
    effects: Vec<Effect>,
    done: Option<Done>,
    /// 有设备的进度变了 —— 同一步里也可能还能再推一次。
    moved: bool,
}

enum Done {
    Commit,
    Fail(String),
}

impl Move {
    /// 整组换人时的新主端：它备不好、起不来，这一次就不成。组里有留下的成员时没有这样一台。
    fn essential(&self) -> Option<&Party> {
        if !self.kept.is_empty() || self.plan.is_none() {
            return None;
        }
        self.parties.iter().find(|party| {
            party.role == Role::Join
                && same(&party.output, &self.to)
        })
    }

    fn joiners(&self) -> impl Iterator<Item = &Party> {
        self.parties
            .iter()
            .filter(|party| party.role == Role::Join)
    }

    /// 准备：新主端备好后最多再等一会儿其余的;组里有人在响时，谁备好谁就可以往下走。
    fn preparing(&mut self, now_ms: u64) -> Outcome {
        if self.plan.is_none() {
            return self.enter_stopping(now_ms);
        }
        if let Some(essential) = self.essential() {
            let name = essential
                .output
                .name()
                .unwrap_or("本机")
                .to_owned();
            match &essential.progress {
                Progress::Failed(why) => {
                    let why =
                        format!("{name} 准备不了: {why}");
                    return Outcome {
                        effects: withdraw(self),
                        done: Some(Done::Fail(why)),
                        moved: true,
                    };
                }
                Progress::Prepared => {}
                _ if now_ms > self.prepare_by_ms => {
                    return Outcome {
                        effects: withdraw(self),
                        done: Some(Done::Fail(
                            "目标没能及时准备好,原来那台照常在放"
                                .to_owned(),
                        )),
                        moved: true,
                    };
                }
                _ => return Outcome::default(),
            }
            let ready_at =
                *self.ready_at_ms.get_or_insert(now_ms);
            let everyone = self.joiners().all(|party| {
                matches!(
                    party.progress,
                    Progress::Prepared
                        | Progress::Failed(_)
                )
            });
            if everyone
                || now_ms >= ready_at + READY_GRACE_MS
            {
                return self.enter_stopping(now_ms);
            }
            return Outcome::default();
        }
        let mut outcome = self.expire_preparing(now_ms);
        let any_ready = self.joiners().any(|party| {
            party.progress == Progress::Prepared
        });
        let everyone = self.joiners().all(|party| {
            matches!(party.progress, Progress::Prepared)
                || party.progress.is_settled()
        });
        if any_ready || everyone {
            let next = self.enter_stopping(now_ms);
            outcome.effects.extend(next.effects);
            outcome.moved = true;
            outcome.done = next.done;
        }
        outcome
    }

    /// 过了准备期限还没备好的新来者：取消，记为失败。
    fn expire_preparing(&mut self, now_ms: u64) -> Outcome {
        let mut outcome = Outcome::default();
        if now_ms <= self.prepare_by_ms {
            return outcome;
        }
        for party in &mut self.parties {
            if party.role == Role::Join
                && party.progress == Progress::Preparing
            {
                party.progress = Progress::Failed(
                    "没能及时准备好".to_owned(),
                );
                outcome.effects.push(Effect::Cancel {
                    operation_id: self.operation_id.clone(),
                    to: party.output.clone(),
                });
                outcome.moved = true;
            }
        }
        outcome
    }

    fn enter_stopping(&mut self, now_ms: u64) -> Outcome {
        let mut effects = Vec::new();
        for party in &mut self.parties {
            if party.role == Role::Leave {
                party.progress = Progress::Stopping;
                party.deadline_ms =
                    now_ms + STOP_TIMEOUT_MS;
                effects.push(Effect::Stop {
                    operation_id: self.operation_id.clone(),
                    from: party.output.clone(),
                });
            }
        }
        self.phase = Phase::Running(Step::Stopping);
        self.deadline_ms = now_ms + STOP_TIMEOUT_MS;
        Outcome {
            effects,
            done: None,
            moved: true,
        }
    }

    /// 停止：走的都停了才开始新来的;有一台明说停不下来就整次撤回;等过了头进「待确认」。
    fn stopping(&mut self, now_ms: u64) -> Outcome {
        let failed =
            self.parties.iter().find_map(|party| {
                match &party.progress {
                    Progress::Failed(why)
                        if party.role == Role::Leave =>
                    {
                        Some(format!(
                            "{} 停不下来: {why}",
                            party
                                .output
                                .name()
                                .unwrap_or("本机")
                        ))
                    }
                    _ => None,
                }
            });
        if let Some(why) = failed {
            return Outcome {
                effects: withdraw(self),
                done: Some(Done::Fail(why)),
                moved: true,
            };
        }
        let all_stopped = self
            .parties
            .iter()
            .filter(|party| party.role == Role::Leave)
            .all(|party| {
                party.progress == Progress::Stopped
            });
        if all_stopped {
            return self.enter_starting(now_ms);
        }
        if self.phase == Phase::Running(Step::Stopping)
            && now_ms > self.deadline_ms
        {
            self.phase =
                Phase::Unconfirmed(Doubt::SourceStop);
            for party in &mut self.parties {
                if party.role == Role::Leave
                    && party.progress == Progress::Stopping
                {
                    party.progress = Progress::Unconfirmed;
                }
            }
            return Outcome {
                moved: true,
                ..Outcome::default()
            };
        }
        Outcome::default()
    }

    fn enter_starting(&mut self, now_ms: u64) -> Outcome {
        self.phase = Phase::Running(Step::Starting);
        self.deadline_ms = now_ms + START_TIMEOUT_MS;
        let effects = self.start_prepared(now_ms);
        Outcome {
            effects,
            done: None,
            moved: true,
        }
    }

    /// 叫已经备好、还没叫过的新来者开始。
    fn start_prepared(
        &mut self,
        now_ms: u64,
    ) -> Vec<Effect> {
        let position_ms = self.anchor_ms().unwrap_or(0);
        let playing = self
            .plan
            .as_ref()
            .is_some_and(|plan| plan.playing);
        let mut effects = Vec::new();
        for party in &mut self.parties {
            if party.role == Role::Join
                && party.progress == Progress::Prepared
            {
                party.progress = Progress::Starting;
                party.deadline_ms =
                    now_ms + START_TIMEOUT_MS;
                effects.push(Effect::Start {
                    operation_id: self.operation_id.clone(),
                    to: party.output.clone(),
                    position_ms,
                    playing,
                });
            }
        }
        effects
    }

    /// 开始：新主端必须确认;普通新成员各算各的，失败的不进集合，等过了头的逐台列出。
    fn starting(&mut self, now_ms: u64) -> Outcome {
        let mut outcome = self.expire_preparing(now_ms);
        let late = self.start_prepared(now_ms);
        outcome.moved |= !late.is_empty();
        outcome.effects.extend(late);

        let essential = self.essential().map(|party| {
            (party.output.clone(), party.progress.clone())
        });
        for party in &mut self.parties {
            let is_essential = essential
                .as_ref()
                .is_some_and(|(output, _)| {
                    same(output, &party.output)
                });
            if party.role == Role::Join
                && party.progress == Progress::Starting
                && !is_essential
                && now_ms > party.deadline_ms
            {
                party.progress = Progress::Unconfirmed;
                outcome.moved = true;
            }
        }

        if let Some((output, progress)) = essential {
            match progress {
                Progress::Failed(why) => {
                    // 确定的失败：新主端没响。走的已经停了，**不**自动恢复 —— 用户按一下
                    // 播放就能在原来那台上接着听，那一下是他自己按的。
                    let mut effects: Vec<Effect> = self
                        .parties
                        .iter()
                        .filter(|party| {
                            party.role == Role::Join
                                && !same(&party.output, &output)
                                && matches!(
                                    party.progress,
                                    Progress::Starting
                                        | Progress::Started
                                        | Progress::Unconfirmed
                                )
                        })
                        .map(|party| Effect::Stop {
                            operation_id: self.operation_id.clone(),
                            from: party.output.clone(),
                        })
                        .collect();
                    effects.push(self.abort());
                    outcome.effects.extend(effects);
                    outcome.done = Some(Done::Fail(
                        format!(
                            "目标没能开始播放: {why};原来那台已经停在原处"
                        ),
                    ));
                    return outcome;
                }
                Progress::Started => {}
                _ => {
                    if self.phase
                        == Phase::Running(Step::Starting)
                        && now_ms > self.deadline_ms
                    {
                        self.phase = Phase::Unconfirmed(
                            Doubt::TargetStart,
                        );
                        if let Some(party) = self
                            .parties
                            .iter_mut()
                            .find(|party| {
                                same(&party.output, &output)
                            })
                        {
                            party.progress =
                                Progress::Unconfirmed;
                        }
                        outcome.moved = true;
                    }
                    return outcome;
                }
            }
        }
        if self
            .joiners()
            .all(|party| party.progress.is_settled())
        {
            outcome.done = Some(Done::Commit);
        }
        outcome
    }

    fn abort(&self) -> Effect {
        Effect::Abort {
            operation_id: self.operation_id.clone(),
        }
    }
}

/// 撤掉一次还没开始出声的换输出：新来的丢掉备好的那一份，服务端作罢。
///
/// 没东西可迁的那种从没叫谁准备过，只作罢。
fn withdraw(moving: &Move) -> Vec<Effect> {
    let mut effects: Vec<Effect> = moving
        .parties
        .iter()
        .filter(|party| {
            party.role == Role::Join
                && matches!(
                    party.progress,
                    Progress::Preparing
                        | Progress::Prepared
                )
        })
        .map(|party| Effect::Cancel {
            operation_id: moving.operation_id.clone(),
            to: party.output.clone(),
        })
        .collect();
    effects.push(moving.abort());
    effects
}

#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod tests;
