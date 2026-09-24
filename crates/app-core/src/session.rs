//! 播放组会话,遥控器这一侧:组现在的输出、进行中的迁移,以及迁移每一步的确认。
//!
//! **选设备等于迁移**(#137 ③):把当前队列、曲目与进度交给目标,本机或原来那台
//! 停止实际出声。它不是改一个路由标志 —— 改标志正是从前「选完设备本机那首接着
//! 响、控制条消失、被控端过很久才出声」的根。
//!
//! 一次迁移四步,用同一个 `operation_id` 贯穿:
//!
//! 1. **目标准备**:取下执行副本、把那一条备到起点,不出声;
//! 2. **源停止实际输出**,报停在哪一毫秒 —— 那一毫秒就是续播的锚点;
//! 3. **目标从锚点开始**;
//! 4. **确认**:目标报「开始了」,服务端把输出集合正式换过去。
//!
//! 每一步只认**对的那台**报来的、**这一次**操作的回话。等不到回话时停在
//! 「待确认」上,由用户处理,不替用户猜:源停止没确认就不启动目标(可能两台一起
//! 响),目标开始没确认就不恢复源(同样可能两台一起响)。迟到的确认照收 ——
//! 收下它正是解开「待确认」的正路 —— 但同一步只推进一次,重试与迟到都不会让
//! 目标起播两遍。
//!
//! 这里只有规则,不发网络、不碰播放器:每个动作以 [`Effect`] 交回调用方执行,
//! 时间由调用方传进来(`docs/adr/0002`)。本机的「准备 / 停止 / 开始」与远端的
//! 一样走回话,只是那份回话由本机自己马上报回来。
//!
//! 成员集合这一轮最多一台,字段从第一天按集合建(`Effect::Begin` 的 `outputs`),
//! 多成员(#137 ⑤)在这个形状上加,不改它。

use contract::{OperationAckDto, OperationPhase, TrackDto};

use crate::Output;

/// 目标准备最多等多久。
///
/// 准备 = 按标识分页取下整份执行副本(五千首是十页)+ 取直链、开流、预读。
/// 真机上取直链加开流一两秒(#121),十页队列再加几秒;二十秒还没好就当它
/// 准备不了 —— 这时候什么都还没动,放弃是安全的,源照旧在放。
pub const PREPARE_TIMEOUT_MS: u64 = 20_000;

/// 源停止最多等多久。
///
/// 停止只是按下播放器、报一个位置,一个来回的事。等不到就进「待确认」,而**不是**
/// 失败:停没停只有源自己知道,此刻替它下结论就会在两台一起响与一台都不响之间
/// 猜一个。
pub const STOP_TIMEOUT_MS: u64 = 5_000;

/// 目标开始最多等多久。
///
/// 开始 = 把备好的那一份交给播放器、跳到锚点。跳到还没下到的位置要重开一个
/// range 请求(`docs/adr/0019`),给它比停止宽一些的余量。
pub const START_TIMEOUT_MS: u64 = 8_000;

/// 要迁过去的那份播放:服务端哪个队列的哪一版、哪一条、从哪一毫秒、在不在放。
///
/// 队列只以标识过去(`docs/adr/0031`),目标自己按标识取整份执行副本。`track`
/// 是给界面画的 —— 迁移那几秒控制条上该一直是这一首,不该变成空白或者目标
/// 手上原来那一首。
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    /// 发起迁移那一刻源的位置。目标按它**预备**;真正的起点是源停下时报的
    /// 那一毫秒(见 [`Move::anchor_ms`])。
    pub position_ms: u64,
    /// 源在不在放。暂停着迁过去,目标也该停在起点等人按播放。
    pub playing: bool,
    pub track: TrackDto,
}

/// 迁移正在做哪一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Preparing,
    Stopping,
    Starting,
}

/// 等不到回话的是哪一步。两种分开,因为该拦的东西相反。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doubt {
    /// 源停没停不知道 —— 所以**不启动目标**。
    SourceStop,
    /// 目标起没起不知道 —— 所以**不恢复源**。
    TargetStart,
}

/// 一次迁移此刻的状况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running(Step),
    /// 等过了头,停在这里由用户处理(重试或放弃)。
    Unconfirmed(Doubt),
}

/// 一次进行中的迁移。
#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub operation_id: String,
    pub from: Output,
    pub to: Output,
    /// 什么都没在放时是 `None`:没有东西可迁,只停源、换输出。
    pub plan: Option<Plan>,
    pub phase: Phase,
    /// 这一步等到几点为止。
    deadline_ms: u64,
    /// 源停下时报的位置 —— 目标从这里开始。
    stopped_at_ms: Option<u64>,
}

impl Move {
    /// 目标从哪一毫秒开始:源停下那一刻报的位置;源没报位置就按发起时的。
    ///
    /// 锚点取「源停下的那一刻」而不是「按下选设备的那一刻」:准备要几秒,这几秒
    /// 里源一直在放,按发起时的位置开始就等于让用户把这几秒再听一遍。
    pub fn anchor_ms(&self) -> Option<u64> {
        let plan = self.plan.as_ref()?;
        Some(self.stopped_at_ms.unwrap_or(plan.position_ms))
    }
}

/// 交给调用方去执行的一个动作。
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// 告诉服务端:这一次操作之后输出集合是 `outputs`(远端设备 id;本机不经
    /// 服务端,所以改回本机时是空集合)。
    Begin {
        operation_id: String,
        outputs: Vec<String>,
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
    /// 告诉服务端:确认完了,输出集合正式换过去。
    Commit {
        operation_id: String,
    },
    /// 告诉服务端:这一次作罢,成员集合不变。
    Abort {
        operation_id: String,
    },
}

/// 这一下为什么没开始一次迁移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// 选的就是现在的输出。
    AlreadyThere,
    /// 上一次已经过了准备那一步(源在停、目标在起,或者在等用户处理),
    /// 半路换目标会让「谁在响」彻底说不清。
    Busy,
}

/// 遥控器手上的播放组会话。
#[derive(Debug, Default)]
pub struct Session {
    /// 已经确认的输出。迁移确认之前**不**换 —— 那几秒里命令仍按它路由。
    output: Output,
    /// 服务端给的主端任期。成员集合每换一次加一。
    term: u64,
    moving: Option<Move>,
    /// 上一次迁移为什么没成,给界面说一句。下一次开始时清掉。
    failure: Option<String>,
    /// 最后一次发出 `Commit` 的操作 —— 服务端回的任期只认它。
    committing: Option<String>,
}

impl Session {
    /// 已经确认的输出。
    pub fn output(&self) -> &Output {
        &self.output
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

    /// 开始把当前播放迁到 `to`。
    ///
    /// 还在准备的上一次会被这一次顶掉:它的目标收到取消,服务端那一次作罢 ——
    /// 用户改主意是正常操作。过了准备那一步就不许换了,见 [`Refused::Busy`]。
    pub fn begin(
        &mut self,
        operation_id: String,
        to: Output,
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
            None if to == self.output => {
                return Err(Refused::AlreadyThere);
            }
            None => {}
        }
        // 顶掉之后又选回了原来的输出:上一次已经撤干净,这一下什么都不必迁。
        if to == self.output {
            return Ok(effects);
        }

        self.failure = None;
        effects.push(Effect::Begin {
            operation_id: operation_id.clone(),
            outputs: to
                .target()
                .map(str::to_owned)
                .into_iter()
                .collect(),
        });
        let mut moving = Move {
            operation_id,
            from: self.output.clone(),
            to,
            plan,
            phase: Phase::Running(Step::Preparing),
            deadline_ms: now_ms + PREPARE_TIMEOUT_MS,
            stopped_at_ms: None,
        };
        effects.push(match &moving.plan {
            Some(plan) => Effect::Prepare {
                operation_id: moving.operation_id.clone(),
                to: moving.to.clone(),
                plan: plan.clone(),
            },
            // 没有东西可迁:跳过准备,直接停源。
            None => {
                moving.enter(Step::Stopping, now_ms);
                moving.stop()
            }
        });
        self.moving = Some(moving);
        Ok(effects)
    }

    /// 收下一台设备对某次操作的回话。本机的回话由调用方以 `Output::Local` 报进来。
    ///
    /// 不是这一次的、不是该回这一步的那台报的、或者这一步已经过去了的,一概
    /// 不理 —— 迟到与重复都落在这里,所以同一步只会推进一次。
    pub fn on_ack(
        &mut self,
        from: &Output,
        ack: &OperationAckDto,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(moving) = self.moving.as_mut() else {
            return Vec::new();
        };
        if moving.operation_id != ack.operation_id {
            return Vec::new();
        }
        let reason = || {
            ack.reason
                .clone()
                .unwrap_or_else(|| "没说原因".to_owned())
        };

        match (moving.waiting_for(), ack.phase) {
            (
                Some(Step::Preparing),
                OperationPhase::Prepared,
            ) if *from == moving.to => {
                moving.enter(Step::Stopping, now_ms);
                vec![moving.stop()]
            }
            (
                Some(Step::Preparing),
                OperationPhase::Failed,
            ) if *from == moving.to => {
                let effects = vec![moving.abort()];
                self.fail(format!(
                    "目标准备不了: {}",
                    reason()
                ));
                effects
            }
            (
                Some(Step::Stopping),
                OperationPhase::Stopped,
            ) if *from == moving.from => {
                moving.stopped_at_ms = ack.position_ms;
                if moving.plan.is_none() {
                    return self.commit();
                }
                moving.enter(Step::Starting, now_ms);
                vec![moving.start()]
            }
            (
                Some(Step::Stopping),
                OperationPhase::Failed,
            ) if *from == moving.from => {
                let effects = withdraw(moving);
                self.fail(format!(
                    "源停不下来: {}",
                    reason()
                ));
                effects
            }
            (
                Some(Step::Starting),
                OperationPhase::Started,
            ) if *from == moving.to => self.commit(),
            (
                Some(Step::Starting),
                OperationPhase::Failed,
            ) if *from == moving.to => {
                // 确定的失败:目标没响。源已经停了,**不**自动恢复 —— 用户按一下
                // 播放就能在源上接着听,那一下是他自己按的。
                let effects = vec![moving.abort()];
                self.fail(format!(
                    "目标没能开始播放: {};原来那台已经停在原处",
                    reason()
                ));
                effects
            }
            _ => Vec::new(),
        }
    }

    /// 时间到了没有:准备超时放弃,停止与开始超时进「待确认」。
    pub fn tick(&mut self, now_ms: u64) -> Vec<Effect> {
        let Some(moving) = self.moving.as_mut() else {
            return Vec::new();
        };
        let Phase::Running(step) = moving.phase else {
            return Vec::new();
        };
        if now_ms <= moving.deadline_ms {
            return Vec::new();
        }
        match step {
            // 什么都还没动,放弃是安全的。
            Step::Preparing => {
                let effects = withdraw(moving);
                self.fail(
                    "目标没能及时准备好,原来那台照常在放"
                        .to_owned(),
                );
                effects
            }
            Step::Stopping => {
                moving.phase =
                    Phase::Unconfirmed(Doubt::SourceStop);
                Vec::new()
            }
            Step::Starting => {
                moving.phase =
                    Phase::Unconfirmed(Doubt::TargetStart);
                Vec::new()
            }
        }
    }

    /// 用户在「待确认」上按了重试:同一个操作号把那一步再发一遍。
    ///
    /// 同一个操作号是要点:两端都按它去重,重发的停止不会再报一个新位置,
    /// 重发的开始不会让已经在响的目标从锚点再起一遍。
    pub fn retry(&mut self, now_ms: u64) -> Vec<Effect> {
        let Some(moving) = self.moving.as_mut() else {
            return Vec::new();
        };
        match moving.phase {
            Phase::Unconfirmed(Doubt::SourceStop) => {
                moving.enter(Step::Stopping, now_ms);
                vec![moving.stop()]
            }
            Phase::Unconfirmed(Doubt::TargetStart) => {
                moving.enter(Step::Starting, now_ms);
                vec![moving.start()]
            }
            Phase::Running(_) => Vec::new(),
        }
    }

    /// 用户在「待确认」上按了放弃。
    ///
    /// 两种放弃都**不让任何一台自动开始**。源停止没确认时撤掉目标的准备,
    /// 输出仍是源;目标开始没确认时叫目标停 —— 它若断着网就收不到,所以
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
            Doubt::TargetStart => (
                vec![
                    Effect::Stop {
                        operation_id: moving
                            .operation_id
                            .clone(),
                        from: moving.to.clone(),
                    },
                    moving.abort(),
                ],
                "已放弃切换;已叫目标停止,但它是否还在出声不能确认",
            ),
        };
        self.fail(why.to_owned());
        effects
    }

    /// 服务端确认输出集合换好了,带回新任期。
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

    /// 服务端没登记下这一次(目标不在线、或者就是本机):作废,记下原因。
    ///
    /// 这时什么都还没动 —— 目标没被锁上,源照旧在放 —— 所以不必撤任何东西。
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

    /// 这台设备在进行中的迁移里是哪一端(源或目标),按设备 id 认。
    ///
    /// 远端的回话只带发信人的 id;拿它现造一个 `Output` 的话名字对不上,
    /// 回话就被当成「来自不该回这一步的那台」丢掉。
    pub fn party(&self, device_id: &str) -> Option<Output> {
        let moving = self.moving.as_ref()?;
        [&moving.to, &moving.from]
            .into_iter()
            .find(|end| end.target() == Some(device_id))
            .cloned()
    }

    /// 输出被收回本机(失权、失联、接管失败):进行中的那一次一并作废。
    pub fn come_home(&mut self) {
        self.output = Output::Local;
        self.moving = None;
    }

    /// 目标确认了:输出换过去,告诉服务端。
    fn commit(&mut self) -> Vec<Effect> {
        let Some(moving) = self.moving.take() else {
            return Vec::new();
        };
        self.output = moving.to;
        self.committing = Some(moving.operation_id.clone());
        vec![Effect::Commit {
            operation_id: moving.operation_id,
        }]
    }

    /// 这一次没成:记一句为什么,进行中的那一次清掉,输出不动。
    fn fail(&mut self, why: String) {
        self.moving = None;
        self.failure = Some(why);
    }
}

impl Move {
    /// 这一刻在等哪一步的回话 —— 待确认也照样等,迟到的确认是它的正路。
    fn waiting_for(&self) -> Option<Step> {
        Some(match self.phase {
            Phase::Running(step) => step,
            Phase::Unconfirmed(Doubt::SourceStop) => {
                Step::Stopping
            }
            Phase::Unconfirmed(Doubt::TargetStart) => {
                Step::Starting
            }
        })
    }

    fn enter(&mut self, step: Step, now_ms: u64) {
        self.phase = Phase::Running(step);
        self.deadline_ms = now_ms
            + match step {
                Step::Preparing => PREPARE_TIMEOUT_MS,
                Step::Stopping => STOP_TIMEOUT_MS,
                Step::Starting => START_TIMEOUT_MS,
            };
    }

    fn stop(&self) -> Effect {
        Effect::Stop {
            operation_id: self.operation_id.clone(),
            from: self.from.clone(),
        }
    }

    fn start(&self) -> Effect {
        Effect::Start {
            operation_id: self.operation_id.clone(),
            to: self.to.clone(),
            position_ms: self.anchor_ms().unwrap_or(0),
            playing: self
                .plan
                .as_ref()
                .is_some_and(|plan| plan.playing),
        }
    }

    fn abort(&self) -> Effect {
        Effect::Abort {
            operation_id: self.operation_id.clone(),
        }
    }
}

/// 撤掉一次还没开始出声的迁移:目标丢掉备好的那一份,服务端作罢。
///
/// 没东西可迁的那种从没叫目标准备过,只作罢。
fn withdraw(moving: &Move) -> Vec<Effect> {
    let mut effects = Vec::new();
    if moving.plan.is_some() {
        effects.push(Effect::Cancel {
            operation_id: moving.operation_id.clone(),
            to: moving.to.clone(),
        });
    }
    effects.push(moving.abort());
    effects
}

#[cfg(test)]
mod tests {
    use contract::DeviceDto;
    use similar_asserts::assert_eq;

    use super::*;

    const NOW: u64 = 1_000_000;

    fn device(id: &str) -> Output {
        Output::Remote(DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        })
    }

    fn track() -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: "a".to_owned(),
            title: "A".to_owned(),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 240_000,
        }
    }

    fn plan() -> Plan {
        Plan {
            queue_id: 7,
            revision: 3,
            entry_id: 12,
            position_ms: 61_500,
            playing: true,
            track: track(),
        }
    }

    fn ack(
        op: &str,
        phase: OperationPhase,
        position_ms: Option<u64>,
    ) -> OperationAckDto {
        OperationAckDto {
            operation_id: op.to_owned(),
            phase,
            position_ms,
            reason: None,
        }
    }

    fn failed(op: &str, reason: &str) -> OperationAckDto {
        OperationAckDto {
            operation_id: op.to_owned(),
            phase: OperationPhase::Failed,
            position_ms: None,
            reason: Some(reason.to_owned()),
        }
    }

    /// 一个输出在 `output` 上的会话。
    fn session_at(output: Output) -> Session {
        Session {
            output,
            ..Session::default()
        }
    }

    /// 本机 → pc1 的迁移已经走到「源在停」那一步。
    fn stopping_local_to_pc1() -> Session {
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");
        session.on_ack(
            &device("pc1"),
            &ack("op", OperationPhase::Prepared, None),
            NOW + 100,
        );
        session
    }

    /// 同上,走到「目标在起」。源停在 63 秒。
    fn starting_local_to_pc1() -> Session {
        let mut session = stopping_local_to_pc1();
        session.on_ack(
            &Output::Local,
            &ack(
                "op",
                OperationPhase::Stopped,
                Some(63_000),
            ),
            NOW + 200,
        );
        session
    }

    // ── 正常路径 ──

    /// 开始一次迁移:先向服务端登记新的输出集合,再叫目标准备 —— 源一个字节都不动。
    #[test]
    fn beginning_a_move_registers_the_outputs_and_prepares_the_target()
     {
        let mut session = Session::default();

        let effects = session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");

        assert_eq!(
            effects,
            vec![
                Effect::Begin {
                    operation_id: "op".to_owned(),
                    outputs: vec!["pc1".to_owned()],
                },
                Effect::Prepare {
                    operation_id: "op".to_owned(),
                    to: device("pc1"),
                    plan: plan(),
                },
            ]
        );
        assert_eq!(
            session.output(),
            &Output::Local,
            "确认之前输出不换"
        );
        assert_eq!(
            session.moving().map(|m| m.phase),
            Some(Phase::Running(Step::Preparing))
        );
    }

    /// 四步走完:准备好 → 停源 → 从源停下的那一毫秒开始 → 确认,输出这才换过去。
    ///
    /// 锚点是源**停下时**报的 63000,不是发起时的 61500:准备那几秒源一直在放,
    /// 按发起时的位置开始就等于让用户把那几秒再听一遍。
    #[test]
    fn a_move_walks_prepare_stop_start_commit_from_the_stop_anchor()
     {
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");

        let after_prepared = session.on_ack(
            &device("pc1"),
            &ack("op", OperationPhase::Prepared, None),
            NOW + 100,
        );
        assert_eq!(
            after_prepared,
            vec![Effect::Stop {
                operation_id: "op".to_owned(),
                from: Output::Local,
            }]
        );

        let after_stopped = session.on_ack(
            &Output::Local,
            &ack(
                "op",
                OperationPhase::Stopped,
                Some(63_000),
            ),
            NOW + 200,
        );
        assert_eq!(
            after_stopped,
            vec![Effect::Start {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                position_ms: 63_000,
                playing: true,
            }]
        );
        assert_eq!(
            session.output(),
            &Output::Local,
            "目标还没确认开始,输出不换"
        );

        let after_started = session.on_ack(
            &device("pc1"),
            &ack(
                "op",
                OperationPhase::Started,
                Some(63_000),
            ),
            NOW + 300,
        );
        assert_eq!(
            after_started,
            vec![Effect::Commit {
                operation_id: "op".to_owned()
            }]
        );
        assert_eq!(session.output(), &device("pc1"));
        assert_eq!(session.moving(), None);
    }

    /// 源停下时没报位置,就按发起那一刻的位置开始 —— 总比从 0:00 强。
    #[test]
    fn a_stop_without_a_position_starts_from_the_planned_one()
     {
        let mut session = stopping_local_to_pc1();

        let effects = session.on_ack(
            &Output::Local,
            &ack("op", OperationPhase::Stopped, None),
            NOW + 200,
        );

        assert_eq!(
            effects,
            vec![Effect::Start {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                position_ms: plan().position_ms,
                playing: true,
            }]
        );
    }

    /// 源是暂停着的:目标开始时也停在起点,不自己响起来。
    #[test]
    fn a_paused_source_starts_the_target_paused() {
        let mut session = Session::default();
        let paused = Plan {
            playing: false,
            ..plan()
        };
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(paused),
                NOW,
            )
            .expect("第一次迁移该开得起来");
        session.on_ack(
            &device("pc1"),
            &ack("op", OperationPhase::Prepared, None),
            NOW + 100,
        );

        let effects = session.on_ack(
            &Output::Local,
            &ack(
                "op",
                OperationPhase::Stopped,
                Some(61_500),
            ),
            NOW + 200,
        );

        assert_eq!(
            effects,
            vec![Effect::Start {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                position_ms: 61_500,
                playing: false,
            }]
        );
    }

    /// 远端 A → 远端 B:停的是 A,准备与开始的是 B,登记的集合只有 B。
    #[test]
    fn a_remote_to_remote_move_stops_the_old_device_and_starts_the_new_one()
     {
        let mut session = session_at(device("a"));

        let begun = session
            .begin(
                "op".to_owned(),
                device("b"),
                Some(plan()),
                NOW,
            )
            .expect("迁移该开得起来");
        assert_eq!(
            begun,
            vec![
                Effect::Begin {
                    operation_id: "op".to_owned(),
                    outputs: vec!["b".to_owned()],
                },
                Effect::Prepare {
                    operation_id: "op".to_owned(),
                    to: device("b"),
                    plan: plan(),
                },
            ]
        );

        let stop = session.on_ack(
            &device("b"),
            &ack("op", OperationPhase::Prepared, None),
            NOW + 100,
        );
        assert_eq!(
            stop,
            vec![Effect::Stop {
                operation_id: "op".to_owned(),
                from: device("a"),
            }]
        );

        let start = session.on_ack(
            &device("a"),
            &ack(
                "op",
                OperationPhase::Stopped,
                Some(63_000),
            ),
            NOW + 200,
        );
        assert_eq!(
            start,
            vec![Effect::Start {
                operation_id: "op".to_owned(),
                to: device("b"),
                position_ms: 63_000,
                playing: true,
            }]
        );
    }

    /// 远端 → 本机:登记的集合是空的(本机不经服务端),准备的是本机。
    #[test]
    fn moving_back_home_registers_no_outputs_and_prepares_locally()
     {
        let mut session = session_at(device("a"));

        let begun = session
            .begin(
                "op".to_owned(),
                Output::Local,
                Some(plan()),
                NOW,
            )
            .expect("迁移该开得起来");

        assert_eq!(
            begun,
            vec![
                Effect::Begin {
                    operation_id: "op".to_owned(),
                    outputs: Vec::new(),
                },
                Effect::Prepare {
                    operation_id: "op".to_owned(),
                    to: Output::Local,
                    plan: plan(),
                },
            ]
        );
    }

    /// 什么都没在放:没有东西可迁,不准备、不开始,只停源、换输出。
    ///
    /// 「被移除的成员停止实际音频输出」照样成立 —— 远端 A 改回本机时 A 仍要停,
    /// 否则它就成了一台没人管、还在响的设备(#137 ① F5)。
    #[test]
    fn a_move_with_nothing_playing_only_stops_the_source() {
        let mut session = session_at(device("a"));

        let begun = session
            .begin(
                "op".to_owned(),
                Output::Local,
                None,
                NOW,
            )
            .expect("迁移该开得起来");
        assert_eq!(
            begun,
            vec![
                Effect::Begin {
                    operation_id: "op".to_owned(),
                    outputs: Vec::new(),
                },
                Effect::Stop {
                    operation_id: "op".to_owned(),
                    from: device("a"),
                },
            ]
        );

        let committed = session.on_ack(
            &device("a"),
            &ack("op", OperationPhase::Stopped, Some(0)),
            NOW + 100,
        );
        assert_eq!(
            committed,
            vec![Effect::Commit {
                operation_id: "op".to_owned()
            }]
        );
        assert_eq!(session.output(), &Output::Local);
    }

    /// 服务端回的任期只认最后一次提交的那个操作。
    #[test]
    fn the_term_comes_only_from_the_committed_operation() {
        let mut session = starting_local_to_pc1();
        session.on_ack(
            &device("pc1"),
            &ack("op", OperationPhase::Started, None),
            NOW + 300,
        );

        session.committed("older", 9);
        assert_eq!(
            session.term(),
            0,
            "别的操作回的任期不算数"
        );

        session.committed("op", 4);
        assert_eq!(session.term(), 4);
    }

    // ── 不开始的情形 ──

    /// 选的就是现在的输出:什么都不做。
    #[test]
    fn selecting_the_current_output_is_refused() {
        let mut session = session_at(device("pc1"));

        assert_eq!(
            session.begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW
            ),
            Err(Refused::AlreadyThere)
        );
        assert_eq!(session.moving(), None);
    }

    /// 源已经在停了:不许半路换目标。
    #[test]
    fn a_new_target_is_refused_once_the_source_is_stopping()
    {
        let mut session = stopping_local_to_pc1();

        assert_eq!(
            session.begin(
                "op2".to_owned(),
                device("tv"),
                Some(plan()),
                NOW + 150
            ),
            Err(Refused::Busy)
        );
        assert_eq!(
            session
                .moving()
                .map(|m| m.operation_id.clone()),
            Some("op".to_owned()),
            "进行中的那一次原样留着"
        );
    }

    // ── AC-3.3:有效期 ──

    /// 准备途中换目标:旧目标撤掉(取消 + 作罢),新目标重新登记、准备。
    #[test]
    fn a_new_target_during_prepare_supersedes_the_old_one()
    {
        let mut session = Session::default();
        session
            .begin(
                "op1".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");

        let effects = session
            .begin(
                "op2".to_owned(),
                device("tv"),
                Some(plan()),
                NOW + 50,
            )
            .expect("准备途中可以换目标");

        assert_eq!(
            effects,
            vec![
                Effect::Cancel {
                    operation_id: "op1".to_owned(),
                    to: device("pc1"),
                },
                Effect::Abort {
                    operation_id: "op1".to_owned()
                },
                Effect::Begin {
                    operation_id: "op2".to_owned(),
                    outputs: vec!["tv".to_owned()],
                },
                Effect::Prepare {
                    operation_id: "op2".to_owned(),
                    to: device("tv"),
                    plan: plan(),
                },
            ]
        );
    }

    /// 被顶掉的那一次迟到的「准备好了」不推进任何东西。
    #[test]
    fn a_late_ready_from_a_superseded_move_is_ignored() {
        let mut session = Session::default();
        session
            .begin(
                "op1".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");
        session
            .begin(
                "op2".to_owned(),
                device("tv"),
                Some(plan()),
                NOW + 50,
            )
            .expect("准备途中可以换目标");

        let effects = session.on_ack(
            &device("pc1"),
            &ack("op1", OperationPhase::Prepared, None),
            NOW + 100,
        );

        assert_eq!(effects, Vec::new());
        assert_eq!(
            session.moving().map(|m| m.phase),
            Some(Phase::Running(Step::Preparing))
        );
    }

    /// 回话来自不该回这一步的那台,不算数:源说「准备好了」推进不了准备。
    #[test]
    fn an_ack_from_the_wrong_device_is_ignored() {
        let mut session = session_at(device("a"));
        session
            .begin(
                "op".to_owned(),
                device("b"),
                Some(plan()),
                NOW,
            )
            .expect("迁移该开得起来");

        let effects = session.on_ack(
            &device("a"),
            &ack("op", OperationPhase::Prepared, None),
            NOW + 100,
        );

        assert_eq!(effects, Vec::new());
    }

    /// 迁移已经结束,之后迟到的 ready / start 一概不理。
    #[test]
    fn acks_after_a_finished_move_change_nothing() {
        let mut session = starting_local_to_pc1();
        session.on_ack(
            &device("pc1"),
            &ack("op", OperationPhase::Started, None),
            NOW + 300,
        );

        for late in [
            ack("op", OperationPhase::Prepared, None),
            ack("op", OperationPhase::Started, None),
            ack("op", OperationPhase::Stopped, Some(1)),
        ] {
            assert_eq!(
                session.on_ack(
                    &device("pc1"),
                    &late,
                    NOW + 400
                ),
                Vec::new()
            );
        }
        assert_eq!(session.output(), &device("pc1"));
    }

    /// 同一步的重复回话只推进一次:两条「停了」只换来一次开始。
    #[test]
    fn a_duplicate_stop_starts_the_target_only_once() {
        let mut session = stopping_local_to_pc1();
        let stopped = ack(
            "op",
            OperationPhase::Stopped,
            Some(63_000),
        );

        let first = session.on_ack(
            &Output::Local,
            &stopped,
            NOW + 200,
        );
        let second = session.on_ack(
            &Output::Local,
            &stopped,
            NOW + 210,
        );

        assert_eq!(first.len(), 1);
        assert_eq!(
            second,
            Vec::new(),
            "重复的停止确认不该再开始一次"
        );
    }

    // ── AC-3.2:结果不确定 ──

    /// 源停止的确认没到:进「待确认」,**不启动目标**。
    #[test]
    fn an_unconfirmed_stop_never_starts_the_target() {
        let mut session = stopping_local_to_pc1();

        let at_deadline =
            session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);
        let later = session.tick(NOW + 60_000);

        assert_eq!(at_deadline, Vec::new());
        assert_eq!(later, Vec::new());
        assert_eq!(
            session.moving().map(|m| m.phase),
            Some(Phase::Unconfirmed(Doubt::SourceStop))
        );
        assert_eq!(session.output(), &Output::Local);
    }

    /// 停止的确认迟到了:照收,目标这才开始 —— 而且只开始一次。
    #[test]
    fn a_late_stop_after_the_doubt_starts_the_target_once()
    {
        let mut session = stopping_local_to_pc1();
        session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);
        let stopped = ack(
            "op",
            OperationPhase::Stopped,
            Some(63_000),
        );

        let first = session.on_ack(
            &Output::Local,
            &stopped,
            NOW + 9_000,
        );
        let again = session.on_ack(
            &Output::Local,
            &stopped,
            NOW + 9_100,
        );

        assert_eq!(
            first,
            vec![Effect::Start {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                position_ms: 63_000,
                playing: true,
            }]
        );
        assert_eq!(again, Vec::new());
    }

    /// 目标开始的确认没到:进「待确认」,**不恢复源**,什么都不发。
    #[test]
    fn an_unconfirmed_start_never_resumes_the_source() {
        let mut session = starting_local_to_pc1();

        let at_deadline =
            session.tick(NOW + 200 + START_TIMEOUT_MS + 1);
        let later = session.tick(NOW + 60_000);

        assert_eq!(at_deadline, Vec::new());
        assert_eq!(later, Vec::new());
        assert_eq!(
            session.moving().map(|m| m.phase),
            Some(Phase::Unconfirmed(Doubt::TargetStart))
        );
    }

    /// 开始的确认迟到了:照收并提交 —— 只提交一次,也不重发开始。
    #[test]
    fn a_late_start_after_the_doubt_commits_once() {
        let mut session = starting_local_to_pc1();
        session.tick(NOW + 200 + START_TIMEOUT_MS + 1);
        let started = ack(
            "op",
            OperationPhase::Started,
            Some(63_000),
        );

        let first = session.on_ack(
            &device("pc1"),
            &started,
            NOW + 20_000,
        );
        let again = session.on_ack(
            &device("pc1"),
            &started,
            NOW + 20_100,
        );

        assert_eq!(
            first,
            vec![Effect::Commit {
                operation_id: "op".to_owned()
            }]
        );
        assert_eq!(again, Vec::new());
        assert_eq!(session.output(), &device("pc1"));
    }

    /// 在「源停止待确认」上重试:同一个操作号再叫源停一次。
    #[test]
    fn retrying_an_unconfirmed_stop_resends_the_same_stop()
    {
        let mut session = stopping_local_to_pc1();
        session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);

        let effects = session.retry(NOW + 10_000);

        assert_eq!(
            effects,
            vec![Effect::Stop {
                operation_id: "op".to_owned(),
                from: Output::Local,
            }]
        );
        assert_eq!(
            session.moving().map(|m| m.phase),
            Some(Phase::Running(Step::Stopping))
        );
    }

    /// 在「目标开始待确认」上重试:同一个操作号、同一个锚点再叫一次开始。
    ///
    /// 目标那头按操作号去重,已经开始过的不会再从锚点起一遍。
    #[test]
    fn retrying_an_unconfirmed_start_resends_the_same_start()
     {
        let mut session = starting_local_to_pc1();
        session.tick(NOW + 200 + START_TIMEOUT_MS + 1);

        let effects = session.retry(NOW + 20_000);

        assert_eq!(
            effects,
            vec![Effect::Start {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                position_ms: 63_000,
                playing: true,
            }]
        );
    }

    /// 在「源停止待确认」上放弃:撤掉目标的准备,输出仍是源。不启动目标。
    #[test]
    fn abandoning_an_unconfirmed_stop_cancels_the_target() {
        let mut session = stopping_local_to_pc1();
        session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);

        let effects = session.abandon();

        assert_eq!(
            effects,
            vec![
                Effect::Cancel {
                    operation_id: "op".to_owned(),
                    to: device("pc1"),
                },
                Effect::Abort {
                    operation_id: "op".to_owned()
                },
            ]
        );
        assert_eq!(session.output(), &Output::Local);
        assert_eq!(session.moving(), None);
        assert!(
            session.failure().is_some(),
            "放弃了要说一句"
        );
    }

    /// 在「目标开始待确认」上放弃:叫目标停,**不恢复源**。
    ///
    /// 目标断着网的话这一条停止它收不到 —— 所以界面不能把放弃说成「已经静音」,
    /// 那句话在 `failure` 里,由界面照着说。
    #[test]
    fn abandoning_an_unconfirmed_start_stops_the_target_and_never_resumes_the_source()
     {
        let mut session = starting_local_to_pc1();
        session.tick(NOW + 200 + START_TIMEOUT_MS + 1);

        let effects = session.abandon();

        assert_eq!(
            effects,
            vec![
                Effect::Stop {
                    operation_id: "op".to_owned(),
                    from: device("pc1"),
                },
                Effect::Abort {
                    operation_id: "op".to_owned()
                },
            ]
        );
        assert!(
            !effects.iter().any(|effect| matches!(
                effect,
                Effect::Start { .. }
            )),
            "放弃之后不许有任何一台自动开始"
        );
        assert_eq!(session.moving(), None);
        assert!(
            session
                .failure()
                .is_some_and(|why| why.contains("不能确认")),
            "不能把放弃说成已经静音: {:?}",
            session.failure()
        );
    }

    /// 还在正常推进时按不了重试也按不了放弃 —— 那两颗键只在「待确认」上出现。
    #[test]
    fn retry_and_abandon_do_nothing_while_the_move_is_running()
     {
        let mut session = stopping_local_to_pc1();

        assert_eq!(session.retry(NOW + 150), Vec::new());
        assert_eq!(session.abandon(), Vec::new());
        assert_eq!(
            session.moving().map(|m| m.phase),
            Some(Phase::Running(Step::Stopping))
        );
    }

    // ── 确定的失败 ──

    /// 准备超时:这时候什么都还没动,放弃是安全的 —— 撤目标、作罢、源照旧在放。
    #[test]
    fn a_prepare_that_times_out_gives_up_and_leaves_the_source_alone()
     {
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");

        let effects =
            session.tick(NOW + PREPARE_TIMEOUT_MS + 1);

        assert_eq!(
            effects,
            vec![
                Effect::Cancel {
                    operation_id: "op".to_owned(),
                    to: device("pc1"),
                },
                Effect::Abort {
                    operation_id: "op".to_owned()
                },
            ]
        );
        assert_eq!(session.output(), &Output::Local);
        assert_eq!(session.moving(), None);
        assert!(session.failure().is_some());
    }

    /// 目标说准备不了:作罢,把它说的原因记下来。
    #[test]
    fn a_failed_prepare_keeps_the_reason() {
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");

        let effects = session.on_ack(
            &device("pc1"),
            &failed("op", "第 3 版里没有条目 12"),
            NOW + 100,
        );

        assert_eq!(
            effects,
            vec![Effect::Abort {
                operation_id: "op".to_owned()
            }]
        );
        assert!(
            session
                .failure()
                .is_some_and(|why| why.contains("条目 12")),
            "{:?}",
            session.failure()
        );
    }

    /// 目标明确说开始失败了:作罢,输出仍是(已经停了的)源,**不自动恢复源**。
    #[test]
    fn a_failed_start_leaves_the_stopped_source_as_the_output()
     {
        let mut session = starting_local_to_pc1();

        let effects = session.on_ack(
            &device("pc1"),
            &failed("op", "没有准备这一次"),
            NOW + 300,
        );

        assert_eq!(
            effects,
            vec![Effect::Abort {
                operation_id: "op".to_owned()
            }]
        );
        assert_eq!(session.output(), &Output::Local);
        assert_eq!(session.moving(), None);
    }

    /// 源明确说停不了:撤掉目标的准备,作罢。
    #[test]
    fn a_source_that_cannot_stop_cancels_the_target() {
        let mut session = stopping_local_to_pc1();

        let effects = session.on_ack(
            &Output::Local,
            &failed("op", "播放器不可用"),
            NOW + 200,
        );

        assert_eq!(
            effects,
            vec![
                Effect::Cancel {
                    operation_id: "op".to_owned(),
                    to: device("pc1"),
                },
                Effect::Abort {
                    operation_id: "op".to_owned()
                },
            ]
        );
    }

    /// 收回本机时进行中的那一次一并作废,输出回本机。
    #[test]
    fn coming_home_drops_the_move() {
        let mut session = stopping_local_to_pc1();

        session.come_home();

        assert_eq!(session.output(), &Output::Local);
        assert_eq!(session.moving(), None);
    }

    /// 服务端没登记下:这一次作废,原因记下来;别的操作的拒绝不理。
    #[test]
    fn a_move_the_server_rejects_is_dropped() {
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");

        session.rejected("older", "device_offline");
        assert!(
            session.moving().is_some(),
            "别的操作的拒绝不算"
        );

        session.rejected(
            "op",
            "device_offline: 设备 pc1 不在线",
        );
        assert_eq!(session.moving(), None);
        assert!(
            session
                .failure()
                .is_some_and(|why| why.contains("不在线"))
        );
        assert_eq!(session.output(), &Output::Local);
    }

    /// 按设备 id 认出迁移的两端 —— 带着会话里记的那个名字,回话才对得上。
    #[test]
    fn the_parties_of_a_move_are_found_by_device_id() {
        let mut session = session_at(device("a"));
        session
            .begin(
                "op".to_owned(),
                device("b"),
                Some(plan()),
                NOW,
            )
            .expect("迁移该开得起来");

        assert_eq!(session.party("a"), Some(device("a")));
        assert_eq!(session.party("b"), Some(device("b")));
        assert_eq!(session.party("c"), None);
        assert_eq!(
            Session::default().party("a"),
            None,
            "没在迁移就没有两端"
        );
    }

    /// 新的一次开始时,上一次的失败说明清掉。
    #[test]
    fn a_new_move_clears_the_last_failure() {
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW,
            )
            .expect("第一次迁移该开得起来");
        session.tick(NOW + PREPARE_TIMEOUT_MS + 1);
        assert!(session.failure().is_some());

        session
            .begin(
                "op2".to_owned(),
                device("pc1"),
                Some(plan()),
                NOW + 30_000,
            )
            .expect("失败之后可以再来");

        assert_eq!(session.failure(), None);
    }
}
