//! 迁移的这一端:本机被叫去「准备 / 停止 / 开始 / 取消」时怎么做,以及选设备时
//! 从本机的播放里凑出要迁过去的那一份(#137 ③)。
//!
//! 哪一步该发给谁、等不到回话怎么办,归 `app_core::Session`(遥控器那一侧的规则)。
//! 这里只管一台设备自己的那一步,远端的与本机的走同一段:
//!
//! - **被遥控的设备**收到 `RemoteCommand::Prepare` 这一类命令(经 `dispatch::execute`),
//!   做完把回话记在自己的 observed 里、立刻报一次(见 [`Reply::Remote`]);
//! - **遥控器自己**是源或目标时,会话交回来的本机那一步也落到这里,回话直接交还
//!   会话(见 [`Reply::Local`])。
//!
//! 每一步都按操作号去重:重发的停止不会再报一个新位置,重发的开始不会让已经在响的
//! 那一首从锚点再起一遍。没备过的那一次一律报失败,不临时现取 —— 重启过的设备
//! 收到一条旧的 `Start` 时,这正是它该有的反应(AC-3.3)。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{
    OperationAckDto, OperationPhase, Output, Plan,
    PlaybackState, TrackDto,
};

use super::*;
use crate::Shell;
use crate::music::*;

/// 这一步的回话交给谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::music) enum Reply {
    /// 本机被遥控:记进 observed,立刻报给遥控器。
    Remote,
    /// 本机就是遥控器:交还本机的会话。
    Local,
}

/// 备好的那一份:取下来的执行副本,加上已经开好的流。
struct Staged {
    operation_id: String,
    queue_id: i64,
    revision: i64,
    entry_ids: Vec<i64>,
    tracks: Vec<TrackDto>,
    index: usize,
    /// 取直链、开流、预读都好了的那一份。`None` 是还在准备。
    ready: Option<(audio::Loaded, audio::StreamHealth)>,
}

#[derive(Default)]
struct State {
    staged: Option<Staged>,
    /// 最近一次做完的那一步 —— 每条上报都带着它(见 `RemoteStateDto::operation`)。
    last: Option<OperationAckDto>,
}

/// 本机作为迁移一端的账本。
#[derive(Clone, Default)]
pub(in crate::music) struct Member {
    inner: Rc<RefCell<State>>,
}

/// 收到「开始」时该怎么办。
#[derive(Debug, PartialEq, Eq)]
enum Begin {
    /// 这一次已经开始过了:把上一次的回话再报一遍,别再动播放器。
    Again,
    /// 没备过这一次(或者还没备好):报失败。
    Unprepared,
    Go,
}

impl Member {
    /// 最近一次做完的那一步,每条上报都带着。
    pub(in crate::music) fn ack(
        &self,
    ) -> Option<OperationAckDto> {
        self.inner.borrow().last.clone()
    }

    /// 开始准备这一次。返回 `false`:这一次已经备好或者已经开始了,不必再来。
    fn begin_prepare(&self, operation_id: &str) -> bool {
        let mut state = self.inner.borrow_mut();
        let done =
            state.last.as_ref().is_some_and(|last| {
                last.operation_id == operation_id
                    && matches!(
                        last.phase,
                        OperationPhase::Prepared
                            | OperationPhase::Started
                    )
            });
        if done {
            return false;
        }
        state.staged = Some(Staged {
            operation_id: operation_id.to_owned(),
            queue_id: 0,
            revision: 0,
            entry_ids: Vec::new(),
            tracks: Vec::new(),
            index: 0,
            ready: None,
        });
        true
    }

    /// 这一次还在不在备 —— 被取消、被下一次顶掉、或者本机不再被遥控时就不在了。
    fn staging(&self, operation_id: &str) -> bool {
        self.inner.borrow().staged.as_ref().is_some_and(
            |staged| staged.operation_id == operation_id,
        )
    }

    /// 备好了:把取下来的与开好的都放上。只放在还在备的那一次上。
    fn stage(&self, staged: Staged) {
        let mut state = self.inner.borrow_mut();
        if state.staged.as_ref().is_some_and(|held| {
            held.operation_id == staged.operation_id
        }) {
            state.staged = Some(staged);
        }
    }

    fn decide_start(&self, operation_id: &str) -> Begin {
        let state = self.inner.borrow();
        if state.last.as_ref().is_some_and(|last| {
            last.operation_id == operation_id
                && last.phase == OperationPhase::Started
        }) {
            return Begin::Again;
        }
        match &state.staged {
            Some(staged)
                if staged.operation_id == operation_id
                    && staged.ready.is_some() =>
            {
                Begin::Go
            }
            _ => Begin::Unprepared,
        }
    }

    fn take_staged(&self) -> Option<Staged> {
        self.inner.borrow_mut().staged.take()
    }

    /// 这一次已经停过的话,停在哪一毫秒 —— 重发的停止照报那个数,不再停一次。
    fn stopped_at(
        &self,
        operation_id: &str,
    ) -> Option<u64> {
        let state = self.inner.borrow();
        let last = state.last.as_ref()?;
        (last.operation_id == operation_id
            && last.phase == OperationPhase::Stopped)
            .then_some(last.position_ms.unwrap_or(0))
    }

    /// 丢掉这一次备好的那一份。不是这一次的不动。
    fn cancel(&self, operation_id: &str) {
        let mut state = self.inner.borrow_mut();
        if state.staged.as_ref().is_some_and(|staged| {
            staged.operation_id == operation_id
        }) {
            state.staged = None;
        }
    }

    /// 本机不再被遥控:备好的那一份丢掉。开着的流与临时文件留着就是白占。
    pub(in crate::music) fn forget_staged(&self) {
        self.inner.borrow_mut().staged = None;
    }

    fn note(&self, ack: OperationAckDto) {
        self.inner.borrow_mut().last = Some(ack);
    }
}

fn ack(
    operation_id: &str,
    phase: OperationPhase,
    position_ms: Option<u64>,
    reason: Option<String>,
) -> OperationAckDto {
    OperationAckDto {
        operation_id: operation_id.to_owned(),
        phase,
        position_ms,
        reason,
    }
}

/// 把这一步的回话交出去,并记进本机的 observed。
fn respond(
    deck: &Deck,
    reply: Reply,
    answer: OperationAckDto,
) {
    log::info!(
        "迁移回话: 操作 {} {:?} 位置 {:?}{}",
        answer.operation_id,
        answer.phase,
        answer.position_ms,
        answer
            .reason
            .as_deref()
            .map(|why| format!("({why})"))
            .unwrap_or_default()
    );
    deck.member.note(answer.clone());
    match reply {
        // 不等下一趟每秒轮询:遥控器那头正按秒表等这一句。
        Reply::Remote => {
            deck.remote.report(snapshot(deck));
        }
        Reply::Local => deck.remote.local_ack(answer),
    }
}

/// 准备:按标识取下整份执行副本,把那一条的流开好,**不出声**。
///
/// 手上原来在放的不动 —— 开始之前它照旧响着;准备没成的话它就一直是它。
pub(in crate::music) fn stage_move(
    deck: &Deck,
    operation_id: String,
    queue_id: i64,
    revision: i64,
    entry_id: i64,
    reply: Reply,
) {
    if !deck.member.begin_prepare(&operation_id) {
        if let Some(last) = deck.member.ack() {
            respond(deck, reply, last);
        }
        return;
    }

    let deck = deck.clone();
    let _ = slint::spawn_local(async move {
        let fetched =
            api::fetch_queue(queue_id, revision).await;
        if !deck.member.staging(&operation_id) {
            log::info!(
                "操作 {operation_id} 取数期间被取消或顶掉了"
            );
            return;
        }
        let entries = match fetched {
            Ok(entries) => entries,
            Err(error) => {
                deck.member.cancel(&operation_id);
                respond(
                    &deck,
                    reply,
                    ack(
                        &operation_id,
                        OperationPhase::Failed,
                        None,
                        Some(format!(
                            "队列没取下来: {error}"
                        )),
                    ),
                );
                return;
            }
        };
        // 别猜第一首:放一首没点过的歌比不出声更糟。
        let Some(index) = entries
            .iter()
            .position(|entry| entry.entry_id == entry_id)
        else {
            deck.member.cancel(&operation_id);
            respond(
                &deck,
                reply,
                ack(
                    &operation_id,
                    OperationPhase::Failed,
                    None,
                    Some(format!(
                        "第 {revision} 版里没有条目 {entry_id}"
                    )),
                ),
            );
            return;
        };

        let entry_ids: Vec<i64> = entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect();
        let tracks: Vec<TrackDto> = entries
            .into_iter()
            .map(|entry| entry.track)
            .collect();
        let ready = super::super::report::prepare(
            deck.player.clone(),
            None,
            tracks[index].clone(),
        )
        .await;
        if !deck.member.staging(&operation_id) {
            log::info!(
                "操作 {operation_id} 开流期间被取消或顶掉了"
            );
            return;
        }
        match ready {
            Ok(ready) => {
                deck.member.stage(Staged {
                    operation_id: operation_id.clone(),
                    queue_id,
                    revision,
                    entry_ids,
                    tracks,
                    index,
                    ready: Some(ready),
                });
                respond(
                    &deck,
                    reply,
                    ack(
                        &operation_id,
                        OperationPhase::Prepared,
                        None,
                        None,
                    ),
                );
            }
            Err(error) => {
                deck.member.cancel(&operation_id);
                respond(
                    &deck,
                    reply,
                    ack(
                        &operation_id,
                        OperationPhase::Failed,
                        None,
                        Some(error),
                    ),
                );
            }
        }
    });
}

/// 停止实际音频输出,报停在哪一毫秒。执行副本留着 —— 停的是声音,不是队列。
pub(in crate::music) fn stop_for_move(
    ui: &MainWindow,
    deck: &Deck,
    operation_id: String,
    reply: Reply,
) {
    let position_ms =
        match deck.member.stopped_at(&operation_id) {
            // 重发的停止:照报上次那个数,不再按一次(也就不会报一个更晚的位置)。
            Some(at) => at,
            None => {
                let at = deck
                    .player
                    .as_ref()
                    .as_ref()
                    .map(|player| {
                        player.position().as_millis() as u64
                    })
                    .unwrap_or(0);
                rest_local(ui, deck);
                // 备好的下一首也作废:它是按这一台接着往下放准备的。
                deck.prefetched.borrow_mut().take();
                at
            }
        };
    respond(
        deck,
        reply,
        ack(
            &operation_id,
            OperationPhase::Stopped,
            Some(position_ms),
            None,
        ),
    );
}

/// 从锚点开始出声(或停在锚点)。只认备好的那一次。
pub(in crate::music) fn start_move(
    ui: &MainWindow,
    deck: &Deck,
    operation_id: String,
    position_ms: u64,
    playing: bool,
    reply: Reply,
) {
    match deck.member.decide_start(&operation_id) {
        Begin::Again => {
            if let Some(last) = deck.member.ack() {
                respond(deck, reply, last);
            }
            return;
        }
        Begin::Unprepared => {
            respond(
                deck,
                reply,
                ack(
                    &operation_id,
                    OperationPhase::Failed,
                    None,
                    Some("没有准备这一次".to_owned()),
                ),
            );
            return;
        }
        Begin::Go => {}
    }
    let Some(staged) = deck.member.take_staged() else {
        return;
    };
    let Some(ready) = staged.ready else { return };
    let track = staged.tracks[staged.index].clone();

    // 换批与记账一起换(`docs/adr/0031` 七:换上是原子的)。
    let shuffled = deck.queue.borrow().is_shuffled();
    deck.queue
        .borrow_mut()
        .replace(staged.tracks, staged.index);
    if shuffled {
        deck.queue.borrow_mut().shuffle(shuffle_seed());
    }
    deck.execution.adopt(
        staged.queue_id,
        staged.revision,
        staged.entry_ids,
    );
    // 备好的那一份经预取那条路交给起播:它认 id,起播时当场取走。
    *deck.prefetched.borrow_mut() = Some((track.id, ready));
    deck.start_at.set(Some(super::super::report::Start {
        at: core::time::Duration::from_millis(position_ms),
        playing,
    }));
    // 多成员组(#137 ⑤):新主端先写下一起开始的那一刻、自己也照它等;普通成员从这一刻起
    // 跟着组的计划放 —— 这一下之前,它手上原来在放的一直没动。
    begin_as_master(deck, position_ms, playing);
    deck.remote.group_join();
    play_current(ui, deck);
    crate::media::push(ui, &deck.playback, &deck.media);
    checkpoint(deck, staged.index);
    mark_sync(ui, deck);

    respond(
        deck,
        reply,
        ack(
            &operation_id,
            OperationPhase::Started,
            Some(position_ms),
            None,
        ),
    );
}

/// 放弃这一次准备。
pub(in crate::music) fn cancel_move(
    deck: &Deck,
    operation_id: &str,
) {
    deck.member.cancel(operation_id);
}

/// 选设备:从当前输出上凑出要迁过去的那一份,交给会话。
///
/// 凑不出来(手上这一批还没同步到服务端)时**不迁**,说一句为什么 —— 目标只能按
/// 服务端的标识取执行副本,半份或猜一份都比不迁更糟。
pub(in crate::music) fn select_output(
    ui: &MainWindow,
    deck: &Deck,
    id: &str,
) {
    let to = if id.is_empty() {
        Output::Local
    } else {
        Output::Remote(app_core::DeviceDto {
            id: id.to_owned(),
            name: crate::sync::remote::device_name(ui, id),
        })
    };
    let plan = if deck.remote.is_remote() {
        deck.remote.with_view(remote_plan)
    } else {
        local_plan(ui, deck)
    };
    match plan {
        Ok(plan) => deck.remote.begin_move(to, plan),
        Err(why) => crate::notice::show(ui, why),
    }
}

/// 加入一起播放 / 移出(#137 ⑤):在当前成员集合上加上或去掉这一台。空串是本机。
///
/// 加进来的那台从当前那一份准备、跟上组的时间线;移出的那台停止实际出声。
/// 移出主端时交给留下的第一台(显式交接);移出最后一台之后没有任何输出。
pub(in crate::music) fn toggle_member(
    ui: &MainWindow,
    deck: &Deck,
    id: &str,
) {
    let output = if id.is_empty() {
        Output::Local
    } else {
        Output::Remote(app_core::DeviceDto {
            id: id.to_owned(),
            name: crate::sync::remote::device_name(ui, id),
        })
    };
    let mut set = deck.remote.members();
    let before = set.len();
    set.retain(|member| member.target() != output.target());
    if set.len() == before {
        set.push(output);
    }
    let plan = if deck.remote.is_remote() {
        deck.remote.with_view(remote_plan)
    } else {
        local_plan(ui, deck)
    };
    match plan {
        Ok(plan) => deck.remote.change_outputs(set, plan),
        Err(why) => crate::notice::show(ui, why),
    }
}

/// 本机正在放的那一份。什么都没放是 `Ok(None)`:没有东西可迁,不是错。
fn local_plan(
    ui: &MainWindow,
    deck: &Deck,
) -> Result<Option<Plan>, String> {
    let state = deck.playback.borrow().state().clone();
    if !matches!(
        state,
        PlaybackState::Playing(_)
            | PlaybackState::Loading(_)
    ) {
        return Ok(None);
    }
    let queue = deck.queue.borrow();
    let Some(track) = queue.current().cloned() else {
        return Ok(None);
    };
    let (queue_id, _, applied) = deck.execution.identity();
    let entry = deck.execution.entry_at(queue.index());
    let (Some(queue_id), Some(revision), Some(entry_id)) =
        (queue_id, applied, entry)
    else {
        drop(queue);
        // 顺手补一次同步:服务端回来了的话,下一次按就迁得动了。
        resync_local_queue(ui, deck);
        return Err("本机这一批还没同步到服务端,稍后再切"
            .to_owned());
    };
    let player = deck.player.as_ref().as_ref().ok();
    Ok(Some(Plan {
        queue_id,
        revision,
        entry_id,
        position_ms: player.map_or(0, |player| {
            player.position().as_millis() as u64
        }),
        playing: player.is_some_and(is_sounding),
        track,
    }))
}

/// 遥控着的那台正在放的那一份,按它最近的上报凑。
fn remote_plan(
    view: &app_core::RemoteView,
    now_ms: u64,
) -> Result<Option<Plan>, String> {
    use app_core::RemotePlayState;

    // 一条上报都还没收到:那台在放什么不知道。当成「什么都没在放」的话,
    // 迁移只会把它停掉,而它正在放的那一首就这么丢了。
    if !view.is_known() {
        return Err("还不知道那台在放什么,等它报上来再切"
            .to_owned());
    }
    let Some(track) = view.track().cloned() else {
        return Ok(None);
    };
    if view.state() == RemotePlayState::Idle {
        return Ok(None);
    }
    let (Some(queue_id), Some(revision), Some(entry_id)) = (
        view.queue_id(),
        view.applied_revision(),
        view.entry_id(),
    ) else {
        return Err(
            "那台手上这一批还没同步到服务端,没法迁过去"
                .to_owned(),
        );
    };
    Ok(Some(Plan {
        queue_id,
        revision,
        entry_id,
        position_ms: view.position_ms(now_ms),
        playing: view.state() != RemotePlayState::Paused,
        track,
    }))
}

/// 本机播放器此刻在不在出声:没按暂停、手上也有源。
///
/// 播放逻辑问这里,不问界面上的 ⏸/▶ —— 界面是投影(#137 ③)。
pub(in crate::music) fn is_sounding(
    player: &audio::Player,
) -> bool {
    !player.is_paused() && !player.empty()
}

/// 会话交回来的本机那一步。
pub(in crate::music) fn run_local(
    ui: &MainWindow,
    deck: &Deck,
    effect: app_core::Effect,
) {
    use app_core::Effect;
    match effect {
        Effect::Prepare {
            operation_id, plan, ..
        } => stage_move(
            deck,
            operation_id,
            plan.queue_id,
            plan.revision,
            plan.entry_id,
            Reply::Local,
        ),
        Effect::Stop { operation_id, .. } => {
            stop_for_move(
                ui,
                deck,
                operation_id,
                Reply::Local,
            );
        }
        Effect::Start {
            operation_id,
            position_ms,
            playing,
            ..
        } => start_move(
            ui,
            deck,
            operation_id,
            position_ms,
            playing,
            Reply::Local,
        ),
        Effect::Cancel { operation_id, .. } => {
            cancel_move(deck, &operation_id);
        }
        // 这三样发给服务端,由会话那一层自己发,到不了这里。
        Effect::Begin { .. }
        | Effect::Commit { .. }
        | Effect::Abort { .. } => {}
    }
}

/// 选设备、会话交回来的本机那一步、「待确认」上的两颗键。
pub(in crate::music) fn bind_session(
    ui: &MainWindow,
    deck: &Deck,
) {
    let selecting = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_set_output(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        select_output(&ui, &selecting, &id);
    });

    let toggling = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_toggle_member(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        toggle_member(&ui, &toggling, &id);
    });

    let running = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_session_effects(move || {
        let Some(ui) = weak.upgrade() else { return };
        while let Some(effect) =
            running.remote.take_local_effect()
        {
            run_local(&ui, &running, effect);
        }
    });

    let retrying = deck.remote.clone();
    ui.global::<Shell>()
        .on_move_retry(move || retrying.retry());
    let abandoning = deck.remote.clone();
    ui.global::<Shell>()
        .on_move_abandon(move || abandoning.abandon());
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn stopped(op: &str, at: u64) -> OperationAckDto {
        ack(op, OperationPhase::Stopped, Some(at), None)
    }

    /// 重发的停止照报上次停下的位置 —— 不会再停一次、报一个更晚的数。
    #[test]
    fn a_repeated_stop_reports_the_first_position() {
        let member = Member::default();
        member.note(stopped("op", 63_000));

        assert_eq!(member.stopped_at("op"), Some(63_000));
        assert_eq!(
            member.stopped_at("other"),
            None,
            "别的操作的停止不算"
        );
    }

    /// 没备过的那一次开始不了 —— 重启过的设备收到旧的开始,就该是这个反应。
    #[test]
    fn a_start_without_a_prepare_is_unprepared() {
        let member = Member::default();

        assert_eq!(
            member.decide_start("op"),
            Begin::Unprepared
        );
    }

    /// 还在备、流还没开好时来了开始,同样开始不了。
    #[test]
    fn a_start_before_the_stream_is_ready_is_unprepared() {
        let member = Member::default();
        assert!(member.begin_prepare("op"));

        assert_eq!(
            member.decide_start("op"),
            Begin::Unprepared
        );
    }

    /// 开始过的那一次再来一遍:只把回话再报一次,不再动播放器。
    #[test]
    fn a_repeated_start_is_answered_again_without_restarting()
     {
        let member = Member::default();
        member.note(ack(
            "op",
            OperationPhase::Started,
            Some(1),
            None,
        ));

        assert_eq!(member.decide_start("op"), Begin::Again);
    }

    /// 已经备好或开始过的那一次,重发的准备不再取一遍。
    #[test]
    fn a_repeated_prepare_is_not_redone() {
        let member = Member::default();
        member.note(ack(
            "op",
            OperationPhase::Prepared,
            None,
            None,
        ));

        assert!(!member.begin_prepare("op"));
        assert!(
            member.begin_prepare("next"),
            "新的一次照常准备"
        );
    }

    /// 取消只丢这一次备的那一份;新的一次顶掉旧的,旧的迟到的结果放不上来。
    #[test]
    fn a_cancelled_or_superseded_prepare_cannot_land() {
        let member = Member::default();
        assert!(member.begin_prepare("op1"));
        assert!(member.begin_prepare("op2"));

        assert!(
            !member.staging("op1"),
            "被顶掉的那一次不再算在备"
        );
        assert!(member.staging("op2"));

        member.cancel("op1");
        assert!(member.staging("op2"), "取消旧的不动新的");
        member.cancel("op2");
        assert!(!member.staging("op2"));
    }

    /// 不再被遥控时备好的那一份丢掉。
    #[test]
    fn forgetting_drops_the_staged_copy() {
        let member = Member::default();
        assert!(member.begin_prepare("op"));

        member.forget_staged();

        assert!(!member.staging("op"));
    }
}
