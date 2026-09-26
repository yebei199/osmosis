//! 多成员播放组(#137 ⑤):谁是主端、共同计划转给谁、主端怎么交接。
//!
//! 服务端仍然不解释计划 —— 它只认「是不是当前主端、当前任期发的」,然后原样转发。
//! 所以这里测的全是身份与去向，不看计划的内容。

use contract::{
    ClientSignal, DeviceDto, GroupPlanDto, LoopModeDto,
    ServerSignal, TrackDto,
};
use similar_asserts::assert_eq;
use tokio::sync::mpsc;

use super::*;
use crate::syncplay::roster::Roster;
use crate::syncplay::signaling::{AccountId, Sink};

const ALICE: AccountId = 1;
const CAPACITY: usize = 32;

fn device(id: &str) -> DeviceDto {
    DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    }
}

/// 遥控器 phone,两台出声的 pc 与 pad,都在 Alice 名下。
struct Room {
    roster: Roster<Sink>,
    control: Control,
    phone: mpsc::Receiver<ServerSignal>,
    pc: mpsc::Receiver<ServerSignal>,
    pad: mpsc::Receiver<ServerSignal>,
}

fn room() -> Room {
    let (phone, rx_phone) = mpsc::channel(CAPACITY);
    let (pc, rx_pc) = mpsc::channel(CAPACITY);
    let (pad, rx_pad) = mpsc::channel(CAPACITY);
    let mut roster = Roster::default();
    roster.join(ALICE, device("phone"), phone);
    roster.join(ALICE, device("pc"), pc);
    roster.join(ALICE, device("pad"), pad);
    Room {
        roster,
        control: Control::default(),
        phone: rx_phone,
        pc: rx_pc,
        pad: rx_pad,
    }
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

fn inbox(
    rx: &mut mpsc::Receiver<ServerSignal>,
) -> Vec<ServerSignal> {
    let mut got = Vec::new();
    while let Ok(message) = rx.try_recv() {
        got.push(message);
    }
    got
}

fn groups(
    messages: &[ServerSignal],
) -> Vec<(u64, Option<String>, Vec<String>)> {
    messages
        .iter()
        .filter_map(|message| match message {
            ServerSignal::Group {
                term,
                master,
                members,
            } => Some((
                *term,
                master.clone(),
                members.clone(),
            )),
            _ => None,
        })
        .collect()
}

fn plans(
    messages: &[ServerSignal],
) -> Vec<(String, u64, u64)> {
    messages
        .iter()
        .filter_map(|message| match message {
            ServerSignal::GroupPlan {
                from,
                term,
                plan,
            } => Some((from.clone(), *term, plan.seq)),
            _ => None,
        })
        .collect()
}

impl Room {
    fn send(
        &mut self,
        from: &str,
        message: ClientSignal,
    ) -> Option<ServerSignal> {
        route(
            &self.roster,
            &mut self.control,
            ALICE,
            from,
            message,
        )
    }

    fn begin(
        &mut self,
        op: &str,
        outputs: &[&str],
        master: Option<&str>,
    ) -> Option<ServerSignal> {
        self.send(
            "phone",
            ClientSignal::BeginOutputs {
                operation_id: op.to_owned(),
                outputs: ids(outputs),
                master: master.map(str::to_owned),
            },
        )
    }

    fn commit(&mut self, op: &str) -> Option<ServerSignal> {
        self.send(
            "phone",
            ClientSignal::CommitOutputs {
                operation_id: op.to_owned(),
                outputs: None,
            },
        )
    }

    /// 只提交真正跟上的那几台。
    fn commit_only(
        &mut self,
        op: &str,
        outputs: &[&str],
    ) -> Option<ServerSignal> {
        self.send(
            "phone",
            ClientSignal::CommitOutputs {
                operation_id: op.to_owned(),
                outputs: Some(ids(outputs)),
            },
        )
    }

    fn plan(
        &mut self,
        from: &str,
        term: u64,
        seq: u64,
    ) -> Option<ServerSignal> {
        self.send(
            from,
            ClientSignal::GroupPlan {
                term,
                plan: Box::new(plan(seq)),
            },
        )
    }

    fn drain(&mut self) {
        inbox(&mut self.phone);
        inbox(&mut self.pc);
        inbox(&mut self.pad);
    }

    /// 组已经是 pc(主端)+ pad,任期 1。
    fn playing_on_pc_and_pad(&mut self) {
        self.begin("op1", &["pc", "pad"], Some("pc"));
        self.commit("op1");
        self.drain();
    }
}

fn plan(seq: u64) -> GroupPlanDto {
    GroupPlanDto {
        seq,
        clock_epoch: 1,
        queue_id: 3,
        revision: 1,
        entry_id: 10,
        track: TrackDto {
            platform: "netease".to_owned(),
            id: "1".to_owned(),
            title: "歌".to_owned(),
            alias: None,
            artists: vec![],
            cover: None,
            duration_ms: 200_000,
        },
        anchor_us: 1_000_000,
        position_us: 0,
        playing: true,
        start_us: 1_000_000,
        next: None,
        valid_until_us: 201_000_000,
        play_order: None,
        round: 0,
        shuffled: false,
        loop_mode: LoopModeDto::Off,
    }
}

/// 提交之后，组里每一台与遥控器都收到组的新样子：任期、谁是主端、成员。
#[test]
fn committing_announces_the_group_to_every_member_and_the_controller()
 {
    let mut room = room();
    room.begin("op1", &["pc", "pad"], Some("pc"));
    room.drain();

    room.commit("op1");

    let want = vec![(
        1,
        Some("pc".to_owned()),
        ids(&["pc", "pad"]),
    )];
    assert_eq!(groups(&inbox(&mut room.pc)), want);
    assert_eq!(groups(&inbox(&mut room.pad)), want);
    assert_eq!(
        groups(&inbox(&mut room.phone)),
        want,
        "遥控器也要知道谁是主端"
    );
}

/// 指定的主端不在输出集合里：取集合的第一台，不留一个没有主端的组。
#[test]
fn a_master_outside_the_outputs_falls_back_to_the_first_output()
 {
    let mut room = room();
    room.begin("op1", &["pad", "pc"], Some("phone"));
    room.commit("op1");

    assert_eq!(
        room.control.master(ALICE),
        Some("pad".to_owned())
    );
}

/// 加入一台：进行中这一次就告诉现任主端与新来的，主端据此把计划再发一遍，新来的就跟得上。
#[test]
fn beginning_to_add_a_member_tells_the_current_master_and_the_newcomer()
 {
    let mut room = room();
    room.begin("op1", &["pc"], Some("pc"));
    room.commit("op1");
    room.drain();

    room.begin("op2", &["pc", "pad"], Some("pc"));

    let want = vec![(
        1,
        Some("pc".to_owned()),
        ids(&["pc", "pad"]),
    )];
    assert_eq!(
        groups(&inbox(&mut room.pc)),
        want,
        "主端要知道有人要进来"
    );
    assert_eq!(
        groups(&inbox(&mut room.pad)),
        want,
        "新来的要知道该听谁的"
    );
}

/// 当前主端、当前任期发的计划转给组里其余成员与遥控器，不回给主端自己。
#[test]
fn the_masters_plan_reaches_the_other_members_and_the_controller()
 {
    let mut room = room();
    room.playing_on_pc_and_pad();

    let reply = room.plan("pc", 1, 7);

    assert_eq!(reply, None);
    assert_eq!(
        plans(&inbox(&mut room.pad)),
        vec![("pc".to_owned(), 1, 7)]
    );
    assert_eq!(
        plans(&inbox(&mut room.phone)),
        vec![("pc".to_owned(), 1, 7)]
    );
    assert_eq!(
        plans(&inbox(&mut room.pc)),
        vec![],
        "不回给主端自己"
    );
}

/// 不是主端的成员发来的计划不转：共同计划只有当前主端有权推进。
#[test]
fn a_plan_from_a_non_master_is_refused() {
    let mut room = room();
    room.playing_on_pc_and_pad();

    let reply = room.plan("pad", 1, 7);

    assert!(
        matches!(reply, Some(ServerSignal::Error { .. })),
        "{reply:?}"
    );
    assert_eq!(plans(&inbox(&mut room.pc)), vec![]);
    assert_eq!(plans(&inbox(&mut room.phone)), vec![]);
}

/// 旧任期里迟到的计划不转：换过一次成员，它就不再作数。
#[test]
fn a_plan_from_an_old_term_is_refused() {
    let mut room = room();
    room.playing_on_pc_and_pad();
    room.begin("op2", &["pc", "pad"], Some("pc"));
    room.commit("op2"); // 任期 2
    room.drain();

    let reply = room.plan("pc", 1, 9);

    assert!(
        matches!(reply, Some(ServerSignal::Error { .. })),
        "{reply:?}"
    );
    assert_eq!(plans(&inbox(&mut room.pad)), vec![]);
}

/// 显式交接：把主端换下时，新主端从提交那一刻起有权发计划，旧主端发的不再转。
#[test]
fn handing_over_the_master_moves_the_right_to_publish() {
    let mut room = room();
    room.playing_on_pc_and_pad();

    room.begin("op2", &["pad"], Some("pad"));
    room.commit("op2");
    let pc_inbox = inbox(&mut room.pc);
    assert!(
        pc_inbox.iter().any(|m| matches!(
            m,
            ServerSignal::NotControlled
        )),
        "被换下的旧主端要撤锁: {pc_inbox:?}"
    );
    assert_eq!(
        groups(&inbox(&mut room.pad)),
        vec![(2, Some("pad".to_owned()), ids(&["pad"]))]
    );

    assert!(matches!(
        room.plan("pc", 2, 1),
        Some(ServerSignal::Error { .. })
    ));
    assert_eq!(room.plan("pad", 2, 1), None);
    assert_eq!(
        plans(&inbox(&mut room.phone)),
        vec![("pad".to_owned(), 2, 1)]
    );
}

/// 主端下线不自动另选主端:组还在，主端还是它(跟随端把已确认的计划放完再停)。
#[test]
fn a_master_dropping_offline_does_not_elect_a_new_one() {
    let mut room = room();
    room.playing_on_pc_and_pad();

    room.control.controller_left(
        ALICE,
        "pc",
        std::time::Instant::now(),
    );

    assert_eq!(
        room.control.master(ALICE),
        Some("pc".to_owned())
    );
    assert_eq!(
        room.control.members(ALICE),
        ids(&["pc", "pad"])
    );
}

/// 遥控器本机也可以是组员(本机在放时「加入一起播放」pc):登记得上，本机不给自己上锁、
/// 也收组的通告;只剩本机自己时仍不经服务端(那是单机输出)。
#[test]
fn the_controller_can_be_a_member_of_its_own_group() {
    let mut room = room();

    let reply =
        room.begin("op1", &["phone", "pc"], Some("phone"));
    assert!(
        matches!(
            reply,
            Some(ServerSignal::OutputsBegun { .. })
        ),
        "{reply:?}"
    );
    let phone = inbox(&mut room.phone);
    assert!(
        !phone.iter().any(|m| matches!(
            m,
            ServerSignal::ControlledBy { .. }
        )),
        "遥控器不锁自己: {phone:?}"
    );
    room.commit("op1");
    assert_eq!(
        room.control.master(ALICE),
        Some("phone".to_owned())
    );
    assert_eq!(
        room.control.members(ALICE),
        ids(&["phone", "pc"])
    );
    assert_eq!(
        room.plan("phone", 1, 1),
        None,
        "本机主端发的计划照转"
    );
    assert_eq!(
        plans(&inbox(&mut room.pc)),
        vec![("phone".to_owned(), 1, 1)]
    );

    let alone =
        room.begin("op2", &["phone"], Some("phone"));
    assert!(
        matches!(alone, Some(ServerSignal::Error { .. })),
        "只剩本机不经服务端: {alone:?}"
    );
}

/// 把遥控器本机移出组时，不给自己发撤锁。
#[test]
fn removing_the_controller_from_the_group_does_not_unlock_itself()
 {
    let mut room = room();
    room.begin("op1", &["phone", "pc"], Some("phone"));
    room.commit("op1");
    room.drain();

    room.begin("op2", &["pc"], Some("pc"));
    room.commit("op2");

    let phone = inbox(&mut room.phone);
    assert!(
        !phone.iter().any(|m| matches!(
            m,
            ServerSignal::NotControlled
        )),
        "{phone:?}"
    );
    assert_eq!(
        room.control.master(ALICE),
        Some("pc".to_owned())
    );
}

/// 提交时只登记真正跟上的那几台：准备不了、开始失败的撤锁，不进组;主端跟着留下的走。
#[test]
fn committing_a_subset_leaves_out_the_members_that_failed()
{
    let mut room = room();
    room.begin("op1", &["pc", "pad"], Some("pc"));
    room.drain();

    let reply = room.commit_only("op1", &["pc"]);

    assert!(
        matches!(
            reply,
            Some(ServerSignal::OutputsCommitted { .. })
        ),
        "{reply:?}"
    );
    assert_eq!(room.control.members(ALICE), ids(&["pc"]));
    assert!(
        inbox(&mut room.pad).iter().any(|m| matches!(
            m,
            ServerSignal::NotControlled
        )),
        "没跟上的那台撤锁"
    );
}

/// 提交的集合里有登记之外的设备：不认，进行中那一次原样留着。
#[test]
fn committing_outputs_that_were_never_begun_is_refused() {
    let mut room = room();
    room.begin("op1", &["pc"], Some("pc"));

    let reply = room.commit_only("op1", &["pc", "pad"]);

    assert!(
        matches!(reply, Some(ServerSignal::Error { .. })),
        "{reply:?}"
    );
    assert_eq!(
        room.commit("op1").map(|m| matches!(
            m,
            ServerSignal::OutputsCommitted { .. }
        )),
        Some(true)
    );
}

/// 被移出的成员也收到组的新样子:它据此看出自己不在里面、离组 —— 撤锁(`NotControlled`)只是
/// 撤锁,遥控器满租约时也发,不能拿它当离组(控制端离线不解散播放组)。
#[test]
fn a_removed_member_hears_the_new_group_without_itself() {
    let mut room = room();
    room.playing_on_pc_and_pad();

    room.begin("op2", &["pc"], Some("pc"));
    room.commit("op2");

    assert_eq!(
        groups(&inbox(&mut room.pad)),
        vec![(2, Some("pc".to_owned()), ids(&["pc"]))]
    );
}
