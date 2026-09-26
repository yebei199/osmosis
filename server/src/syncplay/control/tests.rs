//! `super` 的测试:控制权槽位的规则,与遥控消息的转发。
//!
//! 单独成文件是因为实现加测试放一个文件就破了七百行那条线(全局协议
//! 「代码质量」)。测试本身没动过,连顺序都一样。

use std::time::{Duration, Instant};

use contract::{
    ClientSignal, DeviceDto, RemoteCommand,
    RemotePlayState, RemoteStateDto, ServerSignal,
};
use similar_asserts::assert_eq;
use tokio::sync::mpsc;

use super::*;
use crate::syncplay::roster::Roster;
use crate::syncplay::signaling::{AccountId, Sink};

const ALICE: AccountId = 1;
const BOB: AccountId = 2;
const CAPACITY: usize = 32;

fn device(id: &str) -> DeviceDto {
    DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    }
}

fn report() -> RemoteStateDto {
    RemoteStateDto {
        track: None,
        position_ms: 1_000,
        state: RemotePlayState::Playing,
        queue_id: None,
        revision: None,
        applied_revision: None,
        entry_id: None,
        queue_len: 0,
        volume: 1.0,
        epoch: 1_700_000_000_000,
        state_seq: 42,
        operation: None,
        fault: None,
        route: None,
    }
}

/// 一台手机、一台 pc、外加一台备用手机,都在 Alice 名下。
fn three_devices() -> (
    Roster<Sink>,
    mpsc::Receiver<ServerSignal>,
    mpsc::Receiver<ServerSignal>,
    mpsc::Receiver<ServerSignal>,
) {
    let (phone, rx_phone) = mpsc::channel(CAPACITY);
    let (pc, rx_pc) = mpsc::channel(CAPACITY);
    let (spare, rx_spare) = mpsc::channel(CAPACITY);
    let mut roster = Roster::default();
    roster.join(ALICE, device("phone"), phone);
    roster.join(ALICE, device("pc"), pc);
    roster.join(ALICE, device("spare"), spare);
    (roster, rx_phone, rx_pc, rx_spare)
}

/// 接管成功:遥控器拿到代次,被控端收到「你被谁遥控了」。
///
/// 被控端非知道不可 —— 它要据此锁住本地播放动作并挂出横幅。
#[test]
fn claiming_grants_control_and_tells_the_target() {
    let (roster, mut rx_phone, mut rx_pc, _) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );

    assert!(
        matches!(
            reply,
            Some(ServerSignal::ControlGranted { .. })
        ),
        "实得 {reply:?}"
    );
    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::ControlledBy {
            device: device("phone")
        })
    );
    assert!(
        rx_phone.try_recv().is_err(),
        "应答走返回值,不该再往自己的收件箱里塞一份"
    );
}

/// 第二台**主动**接管顶掉第一台,第一台收到撤权。
#[test]
fn a_second_claim_revokes_the_first_controller() {
    let (roster, mut rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    let _ = rx_phone.try_recv();

    route(
        &roster,
        &mut control,
        ALICE,
        "spare",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );

    assert_eq!(
        rx_phone.try_recv(),
        Ok(ServerSignal::ControlRevoked {
            by: "spare".to_owned()
        }),
        "被顶掉的那台必须知道自己失权了"
    );
    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("spare")
    );
}

/// 同一台遥控器再接管一次,不该给自己发撤权。
///
/// 发了的话遥控器会把自己降级回本机输出,而它其实还持着权。
#[test]
fn reclaiming_as_the_same_controller_revokes_nobody() {
    let (roster, mut rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    let claim = || ClientSignal::ClaimControl {
        target: "pc".to_owned(),
        resume: None,
    };
    route(&roster, &mut control, ALICE, "phone", claim());
    let _ = rx_phone.try_recv();

    route(&roster, &mut control, ALICE, "phone", claim());

    assert!(
        rx_phone.try_recv().is_err(),
        "不该给自己发一条撤权"
    );
}

/// **断线的旧遥控器重连不夺回**(产品规则)。
///
/// 重连时自动重发的那条带着它手上的旧代次;槽位已经换人,就只能得到
/// 一条撤权。不区分的话,手机一恢复网络就会把接管者顶掉,而接管者那边
/// 什么也没做过。
#[test]
fn a_stale_resume_does_not_steal_control_back() {
    let (roster, mut rx_phone, _rx_pc, mut rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    let granted = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    let Some(ServerSignal::ControlGranted { generation }) =
        granted
    else {
        panic!("实得 {granted:?}")
    };
    route(
        &roster,
        &mut control,
        ALICE,
        "spare",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    while rx_phone.try_recv().is_ok() {}
    while rx_spare.try_recv().is_ok() {}

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: Some(generation),
        },
    );

    assert_eq!(
        reply,
        Some(ServerSignal::ControlRevoked {
            by: "spare".to_owned()
        })
    );
    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("spare"),
        "重连的那台把控制权抢回去了"
    );
    assert!(
        rx_spare.try_recv().is_err(),
        "接管者不该因为别人重连而收到任何东西"
    );
}

/// 代次还对得上的重连:续上,不换人也不换代次。
#[test]
fn a_matching_resume_keeps_the_same_generation() {
    let (roster, mut rx_phone, mut rx_pc, _) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    let granted = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    let Some(ServerSignal::ControlGranted { generation }) =
        granted
    else {
        panic!("实得 {granted:?}")
    };
    while rx_phone.try_recv().is_ok() {}
    while rx_pc.try_recv().is_ok() {}

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: Some(generation),
        },
    );

    assert_eq!(
        reply,
        Some(ServerSignal::ControlGranted { generation })
    );
}

/// 被控端按了「退出被遥控」:槽位清空,遥控器收到撤权。
#[test]
fn the_target_exiting_revokes_its_controller() {
    let (roster, mut rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    while rx_phone.try_recv().is_ok() {}

    route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::ExitControlled,
    );

    assert_eq!(
        rx_phone.try_recv(),
        Ok(ServerSignal::ControlRevoked {
            by: "pc".to_owned()
        })
    );
    assert_eq!(control.controller_of(ALICE, "pc"), None);
}

/// **遥控器下线不解锁被控端**(产品规则:手机没电不能让 pc1 停)。
///
/// 与被控端下线那条正好相反,最容易写反 —— 写反的症状是手机一锁屏
/// pc1 就自己解锁了,而用户根本没碰过它。
#[test]
fn a_controller_going_offline_leaves_the_target_locked() {
    let (roster, _rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );

    let freed = control.release(ALICE, "phone");

    assert_eq!(freed, None, "遥控器下线不该清掉槽位");
    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone"),
        "被控端不该因为遥控器下线而解锁"
    );
}

/// 被控端下线:槽位清掉,并把失权的遥控器交还给调用方去通知。
#[test]
fn the_target_going_offline_frees_the_slot() {
    let (roster, _rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );

    let freed = control.release(ALICE, "pc");

    assert_eq!(freed, Some("phone".to_owned()));
    assert_eq!(control.controller_of(ALICE, "pc"), None);
}

/// 命令只从持权的那台转过去。
///
/// 不看这一条的话,同账号下任意一台设备都能让 pc1 切歌 ——
/// 而 pc1 前面的人只会看到歌自己跳了。
#[test]
fn a_command_from_a_non_controller_is_refused() {
    let (roster, _rx_phone, mut rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "spare",
        ClientSignal::Command {
            to: "pc".to_owned(),
            cmd: RemoteCommand::Next,
        },
    );

    assert!(
        matches!(
            reply,
            Some(ServerSignal::Error { ref code, .. })
                if code == "not_controller"
        ),
        "实得 {reply:?}"
    );
    assert!(
        rx_pc.try_recv().is_err(),
        "没持权的命令不该真的送过去"
    );
}

/// 持权时命令原样转给被控端。
#[test]
fn a_command_from_the_controller_reaches_the_target() {
    let (roster, _rx_phone, mut rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    while rx_pc.try_recv().is_ok() {}

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::Command {
            to: "pc".to_owned(),
            cmd: RemoteCommand::Seek { ms: 30_000 },
        },
    );

    assert_eq!(reply, None, "转发成功时不该有应答");
    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::Command {
            cmd: RemoteCommand::Seek { ms: 30_000 }
        })
    );
}

/// 上报只送给持权的遥控器,不广播。
///
/// 广播的话,同账号的每台设备每秒都会收到一份别人的播放位置。
#[test]
fn a_report_goes_only_to_the_controller() {
    let (roster, mut rx_phone, _rx_pc, mut rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    while rx_phone.try_recv().is_ok() {}

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, None);
    assert_eq!(
        rx_phone.try_recv(),
        Ok(ServerSignal::State {
            from: "pc".to_owned(),
            state: Box::new(report()),
        })
    );
    assert!(rx_spare.try_recv().is_err());
}

/// 没人持权时的上报谁也不转,但要回一条 `NotControlled`。
///
/// 这里曾经是就地丢掉、什么也不回,理由是「一台刚失权的被控端会每秒收到一条
/// 它做不了任何事的报错」。那条理由对报错成立,对这一条不成立:`NotControlled`
/// 正是它做得了事的那一条 —— 收到就解锁、撤横幅,而 `Remote::report` 只在
/// 锁定态下才发上报,于是下一秒它自己就不再报了,这条应答发一次就停(#102 F-004)。
/// 静默丢掉的代价反而更大:被控端挂着假横幅锁死本机,谁也不来告诉它一声。
#[test]
fn a_report_without_a_controller_frees_the_target() {
    let (roster, mut rx_phone, _rx_pc, mut rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, Some(ServerSignal::NotControlled));
    assert!(
        rx_phone.try_recv().is_err(),
        "没人持权,这条上报谁也不该收到"
    );
    assert!(rx_spare.try_recv().is_err());
}

/// 要快照同样要持权,转过去的是一条不带参数的请求。
#[test]
fn a_snapshot_request_needs_control() {
    let (roster, _rx_phone, mut rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let refused = route(
        &roster,
        &mut control,
        ALICE,
        "spare",
        ClientSignal::SnapshotRequest {
            to: "pc".to_owned(),
        },
    );
    assert!(
        matches!(
            refused,
            Some(ServerSignal::Error { ref code, .. })
                if code == "not_controller"
        ),
        "实得 {refused:?}"
    );

    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    while rx_pc.try_recv().is_ok() {}
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::SnapshotRequest {
            to: "pc".to_owned(),
        },
    );

    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::SnapshotRequest)
    );
}

/// 接管一台不在线的设备要回错误,与信令那侧同一个说法。
#[test]
fn claiming_an_offline_device_reports_an_error() {
    let (roster, _rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "不在线".to_owned(),
            resume: None,
        },
    );

    assert!(
        matches!(
            reply,
            Some(ServerSignal::Error { ref code, .. })
                if code == "device_offline"
        ),
        "实得 {reply:?}"
    );
}

/// 接管自己没有意义:输出设备选本机走的是本地那条路,一条信令都不发。
#[test]
fn claiming_yourself_is_refused() {
    let (roster, _rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "phone".to_owned(),
            resume: None,
        },
    );

    assert!(
        matches!(
            reply,
            Some(ServerSignal::Error { ref code, .. })
                if code == "cannot_control_self"
        ),
        "实得 {reply:?}"
    );
    assert_eq!(control.controller_of(ALICE, "phone"), None);
}

/// 跨账号够不着:别人账号下的设备一律当作不在线。
#[test]
fn claiming_across_accounts_is_refused() {
    let (mut roster, _rx_phone, _rx_pc, _) =
        three_devices();
    let (sink, mut rx_bob) = mpsc::channel(CAPACITY);
    roster.join(BOB, device("bobpc"), sink);
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "bobpc".to_owned(),
            resume: None,
        },
    );

    assert!(
        matches!(
            reply,
            Some(ServerSignal::Error { ref code, .. })
                if code == "device_offline"
        ),
        "实得 {reply:?}"
    );
    assert!(
        rx_bob.try_recv().is_err(),
        "不该把别人的设备锁进本账号的遥控里"
    );
}

/// 两个账号各有各的槽位,互不影响。
#[test]
fn each_account_has_its_own_slot() {
    let (mut roster, _rx_phone, _rx_pc, _) =
        three_devices();
    let (bob_phone, _rx1) = mpsc::channel(CAPACITY);
    let (bob_pc, _rx2) = mpsc::channel(CAPACITY);
    roster.join(BOB, device("phone"), bob_phone);
    roster.join(BOB, device("pc"), bob_pc);
    let mut control = Control::booted_at(LEASE, 0);

    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    control.release(BOB, "pc");

    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone"),
        "别人账号上的退出把这一桶的槽位清掉了"
    );
}

/// 槽位上查不到时,那一下「退出被遥控」也要有回音。
///
/// 吞掉的话被控端只是本地撤了横幅,锁定态与服务端的看法从此各说各话。
#[test]
fn exiting_without_a_grant_still_gets_an_answer() {
    let (roster, _rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::ExitControlled,
    );

    assert_eq!(reply, Some(ServerSignal::NotControlled));
}

/// 真的持权时不许回 `NotControlled` —— 回了就是每秒把正常的遥控关系拆一次。
#[test]
fn a_report_with_a_grant_is_forwarded_as_before() {
    let (roster, mut rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    );
    while rx_phone.try_recv().is_ok() {}

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, None, "持权时应答走转发,不回给自己");
    assert!(
        matches!(
            rx_phone.try_recv(),
            Ok(ServerSignal::State { .. })
        ),
        "上报该转给遥控器"
    );
}

/// 手机接管 pc,交出代次。租约那几条都从这一步开始。
fn phone_controls_pc(control: &mut Control) -> Generation {
    let Claim::Granted { generation, .. } =
        control.claim(ALICE, "phone", "pc", None)
    else {
        panic!("主动接管不该被拒");
    };
    generation
}

/// 遥控器下线满一个租约,槽位就清掉 —— 被控端下一次上报拿到 `NotControlled`
/// 自己解锁(#111)。不清的话 force-stop 过的手机会让 pc1 永远挂着横幅。
#[test]
fn a_vanished_controller_loses_the_slot_after_the_lease() {
    let lease = Duration::from_secs(90);
    let mut control = Control::booted_at(lease, 0);
    phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.controller_left(ALICE, "phone", gone);
    control.expire(ALICE, gone + lease);

    assert_eq!(control.controller_of(ALICE, "pc"), None);
}

/// 租约没满之前槽位还在:遥控器只是在重连的路上。
#[test]
fn the_slot_survives_until_the_lease_runs_out() {
    let lease = Duration::from_secs(90);
    let mut control = Control::booted_at(lease, 0);
    phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.controller_left(ALICE, "phone", gone);
    control.expire(
        ALICE,
        gone + lease - Duration::from_millis(1),
    );

    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone")
    );
}

/// 短暂断网:租约内带着代次续上了,之后再久也不清 —— 续上即回到在线。
#[test]
fn a_controller_that_resumes_within_the_lease_keeps_the_slot()
 {
    let lease = Duration::from_secs(90);
    let mut control = Control::booted_at(lease, 0);
    let generation = phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.controller_left(ALICE, "phone", gone);
    assert_eq!(
        control.claim(
            ALICE,
            "phone",
            "pc",
            Some(generation)
        ),
        Claim::Granted {
            generation,
            revoked: None
        }
    );
    control.expire(ALICE, gone + lease * 10);

    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone")
    );
}

/// 下线的不是遥控器(被控端、旁边一台、别的账号同名设备),租约都不起算。
///
/// 被控端下线走的是 `release`,不归租约;把租约挂到它身上,
/// 就成了另一种写法的「被控端下线不清槽」。
#[test]
fn only_the_controller_leaving_starts_the_lease() {
    let lease = Duration::from_secs(90);
    let mut control = Control::booted_at(lease, 0);
    phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.controller_left(ALICE, "spare", gone);
    control.controller_left(ALICE, "pc", gone);
    control.controller_left(BOB, "phone", gone);
    control.expire(ALICE, gone + lease * 10);

    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone")
    );
}

/// 过期由路由自己判,不靠另起定时器:被控端过期后的第一条上报
/// 就拿到 `NotControlled`,而那条上报谁也不转。
#[test]
fn a_report_after_the_lease_gets_not_controlled() {
    let (roster, mut rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(Duration::ZERO, 0);
    phone_controls_pc(&mut control);
    control.controller_left(ALICE, "phone", Instant::now());

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, Some(ServerSignal::NotControlled));
    assert!(
        rx_phone.try_recv().is_err(),
        "过期的遥控器不该再收到上报"
    );
}

// ── 播放组:改在这些设备播放(#137 ③)──

/// 手机开始把输出换成 `outputs`,返回服务端给的应答。
fn begin(
    roster: &Roster<Sink>,
    control: &mut Control,
    op: &str,
    outputs: &[&str],
) -> Option<ServerSignal> {
    route(
        roster,
        control,
        ALICE,
        "phone",
        ClientSignal::BeginOutputs {
            operation_id: op.to_owned(),
            outputs: outputs
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            master: None,
        },
    )
}

fn commit(
    roster: &Roster<Sink>,
    control: &mut Control,
    op: &str,
) -> Option<ServerSignal> {
    route(
        roster,
        control,
        ALICE,
        "phone",
        ClientSignal::CommitOutputs {
            operation_id: op.to_owned(),
            outputs: None,
        },
    )
}

fn command_to(
    roster: &Roster<Sink>,
    control: &mut Control,
    to: &str,
) -> Option<ServerSignal> {
    route(
        roster,
        control,
        ALICE,
        "phone",
        ClientSignal::Command {
            to: to.to_owned(),
            cmd: RemoteCommand::Stop {
                operation_id: "op".to_owned(),
            },
        },
    )
}

/// 把收件箱里已有的消息全倒掉,只看之后来的。
fn drain(rx: &mut mpsc::Receiver<ServerSignal>) {
    while rx.try_recv().is_ok() {}
}

/// 手机已经把输出换到 pc 上,组里就 pc 一台。
fn phone_moved_to_pc(
    roster: &Roster<Sink>,
    control: &mut Control,
) {
    begin(roster, control, "op-pc", &["pc"]);
    commit(roster, control, "op-pc");
}

/// 开始一次换输出:新来的那台被锁上、开始听命令;遥控器拿到代次。
#[test]
fn beginning_outputs_locks_the_incoming_device_and_grants_a_generation()
 {
    let (roster, _rx_phone, mut rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = begin(&roster, &mut control, "op", &["pc"]);

    assert!(
        matches!(
            reply,
            Some(ServerSignal::OutputsBegun { ref operation_id, .. })
                if operation_id == "op"
        ),
        "{reply:?}"
    );
    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::ControlledBy {
            device: device("phone")
        })
    );
}

/// 操作进行中,旧成员与新来的那台**都**收得到命令 —— 源要能被叫停,
/// 目标要能被叫准备、叫开始。只认其中一台的话,迁移做不完。
#[test]
fn commands_reach_both_the_old_member_and_the_incoming_one()
{
    let (roster, _rx_phone, mut rx_pc, mut rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    begin(&roster, &mut control, "op-spare", &["spare"]);
    drain(&mut rx_pc);
    drain(&mut rx_spare);

    assert_eq!(
        command_to(&roster, &mut control, "pc"),
        None
    );
    assert_eq!(
        command_to(&roster, &mut control, "spare"),
        None
    );
    assert!(matches!(
        rx_pc.try_recv(),
        Ok(ServerSignal::Command { .. })
    ));
    assert!(matches!(
        rx_spare.try_recv(),
        Ok(ServerSignal::Command { .. })
    ));
}

/// 新来的那台的上报转给遥控器 —— 它的「准备好了」就搭在上报里。
#[test]
fn reports_from_the_incoming_device_reach_the_controller() {
    let (roster, mut rx_phone, _rx_pc, _) = three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    begin(&roster, &mut control, "op", &["pc"]);
    drain(&mut rx_phone);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, None);
    assert!(matches!(
        rx_phone.try_recv(),
        Ok(ServerSignal::State { ref from, .. }) if from == "pc"
    ));
}

/// 提交:成员换过去、任期加一,换下来的那台撤锁,之后命令也到不了它了。
#[test]
fn committing_swaps_members_bumps_the_term_and_unlocks_the_removed()
 {
    let (roster, _rx_phone, mut rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    let before = control.term(ALICE);
    begin(&roster, &mut control, "op-spare", &["spare"]);
    drain(&mut rx_pc);

    let reply = commit(&roster, &mut control, "op-spare");

    assert_eq!(
        reply,
        Some(ServerSignal::OutputsCommitted {
            operation_id: "op-spare".to_owned(),
            term: before + 1,
        })
    );
    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::NotControlled),
        "换下来的那台该撤锁"
    );
    assert_eq!(
        control.members(ALICE),
        vec!["spare".to_owned()]
    );
    assert!(matches!(
        command_to(&roster, &mut control, "pc"),
        Some(ServerSignal::Error { .. })
    ));
}

/// 换下来的那台迟到的上报不再转给遥控器:它已经不是成员了。
#[test]
fn a_report_from_a_removed_member_is_not_forwarded() {
    let (roster, mut rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    begin(&roster, &mut control, "op-spare", &["spare"]);
    commit(&roster, &mut control, "op-spare");
    drain(&mut rx_phone);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, Some(ServerSignal::NotControlled));
    assert!(rx_phone.try_recv().is_err());
}

/// 提交的操作号对不上就拒绝,成员集合不动 —— 旧操作迟到的提交不能把新的一次换掉。
#[test]
fn committing_the_wrong_operation_is_refused() {
    let (roster, _rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    begin(&roster, &mut control, "op-new", &["spare"]);

    let reply = commit(&roster, &mut control, "op-old");

    assert!(
        matches!(
            reply,
            Some(ServerSignal::Error { ref code, .. })
                if code == "operation_mismatch"
        ),
        "{reply:?}"
    );
    assert_eq!(
        control.members(ALICE),
        vec!["pc".to_owned()]
    );
}

/// 作罢:新来的那台撤锁,成员集合原样。
#[test]
fn aborting_unlocks_the_incoming_device_and_keeps_the_members()
 {
    let (roster, _rx_phone, mut rx_pc, mut rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    begin(&roster, &mut control, "op-spare", &["spare"]);
    drain(&mut rx_pc);
    drain(&mut rx_spare);

    route(
        &roster,
        &mut control,
        ALICE,
        "phone",
        ClientSignal::AbortOutputs {
            operation_id: "op-spare".to_owned(),
        },
    );

    assert_eq!(
        rx_spare.try_recv(),
        Ok(ServerSignal::NotControlled)
    );
    assert!(rx_pc.try_recv().is_err(), "成员不受影响");
    assert_eq!(
        control.members(ALICE),
        vec!["pc".to_owned()]
    );
}

/// 准备途中换目标:新的一次顶掉旧的,旧目标撤锁,之后命令到不了它。
#[test]
fn a_new_begin_replaces_the_pending_one_and_unlocks_the_dropped_device()
 {
    let (roster, _rx_phone, mut rx_pc, mut rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    begin(&roster, &mut control, "op1", &["pc"]);
    drain(&mut rx_pc);

    begin(&roster, &mut control, "op2", &["spare"]);

    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::NotControlled)
    );
    assert!(matches!(
        rx_spare.try_recv(),
        Ok(ServerSignal::ControlledBy { .. })
    ));
    assert!(matches!(
        command_to(&roster, &mut control, "pc"),
        Some(ServerSignal::Error { .. })
    ));
}

/// 改回本机 = 提交一个空集合:组散掉,原来那台撤锁。
#[test]
fn committing_an_empty_set_dissolves_the_group() {
    let (roster, _rx_phone, mut rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    begin(&roster, &mut control, "op-home", &[]);
    drain(&mut rx_pc);

    let reply = commit(&roster, &mut control, "op-home");

    assert!(matches!(
        reply,
        Some(ServerSignal::OutputsCommitted { .. })
    ));
    assert_eq!(
        rx_pc.try_recv(),
        Ok(ServerSignal::NotControlled)
    );
    assert_eq!(
        control.members(ALICE),
        Vec::<String>::new()
    );
    assert_eq!(control.controller_of(ALICE, "pc"), None);
}

/// **控制端离线满租约不解散组**:遥控器清掉,成员还在;下一位遥控器接上时
/// 成员重新被锁上。
#[test]
fn the_group_survives_the_controller_lease() {
    let (roster, _rx_phone, mut rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(Duration::ZERO, 0);
    phone_moved_to_pc(&roster, &mut control);
    control.controller_left(ALICE, "phone", Instant::now());
    control.expire(ALICE, Instant::now());

    assert_eq!(control.controller_of(ALICE, "pc"), None);
    assert_eq!(
        control.members(ALICE),
        vec!["pc".to_owned()],
        "遥控器走了,组不散"
    );

    drain(&mut rx_pc);
    begin(&roster, &mut control, "op-again", &["pc"]);
    assert!(matches!(
        rx_pc.try_recv(),
        Ok(ServerSignal::ControlledBy { .. })
    ));
}

/// 另一台遥控器开始换输出,等于接管:旧遥控器收到失权。
#[test]
fn a_begin_from_another_device_takes_over_and_revokes_the_old_controller()
 {
    let (roster, mut rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    phone_moved_to_pc(&roster, &mut control);
    drain(&mut rx_phone);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "spare",
        ClientSignal::BeginOutputs {
            operation_id: "op".to_owned(),
            outputs: vec!["pc".to_owned()],
            master: None,
        },
    );

    assert!(matches!(
        reply,
        Some(ServerSignal::OutputsBegun { .. })
    ));
    assert_eq!(
        rx_phone.try_recv(),
        Ok(ServerSignal::ControlRevoked {
            by: "spare".to_owned()
        })
    );
}

/// 输出里有不在线的设备、或者就是自己:整次拒绝,什么都不锁。
#[test]
fn a_begin_with_an_unusable_output_is_refused() {
    let (roster, _rx_phone, mut rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let offline =
        begin(&roster, &mut control, "op", &["tv"]);
    let myself =
        begin(&roster, &mut control, "op", &["phone"]);

    assert!(matches!(
        offline,
        Some(ServerSignal::Error { ref code, .. }) if code == "device_offline"
    ));
    assert!(matches!(
        myself,
        Some(ServerSignal::Error { ref code, .. }) if code == "cannot_control_self"
    ));
    assert!(rx_pc.try_recv().is_err());
}

/// 不是遥控器的那台提交不了。
#[test]
fn only_the_controller_can_commit() {
    let (roster, _rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);
    begin(&roster, &mut control, "op", &["pc"]);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "spare",
        ClientSignal::CommitOutputs {
            operation_id: "op".to_owned(),
            outputs: None,
        },
    );

    assert!(matches!(
        reply,
        Some(ServerSignal::Error { ref code, .. }) if code == "not_controller"
    ));
    assert_eq!(
        control.members(ALICE),
        Vec::<String>::new()
    );
}

// ── 两端对称的断线租约、跨重启的代次与任期(#142)──

/// 被控端闪断:租约内回来,组与遥控器都还在。
#[test]
fn a_member_back_within_the_lease_keeps_the_group() {
    let lease = Duration::from_secs(60);
    let mut control = Control::booted_at(lease, 0);
    phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.member_left(ALICE, "pc", gone);
    control.member_back(ALICE, "pc");

    assert_eq!(
        control.expire(ALICE, gone + lease * 10),
        None
    );
    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone")
    );
}

/// 租约没满之前,下线的被控端还在组里:遥控器只是在等它重连。
#[test]
fn a_member_stays_in_the_group_until_its_lease_runs_out() {
    let lease = Duration::from_secs(60);
    let mut control = Control::booted_at(lease, 0);
    phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.member_left(ALICE, "pc", gone);

    assert_eq!(
        control.expire(
            ALICE,
            gone + lease - Duration::from_millis(1)
        ),
        None
    );
    assert_eq!(
        control.controller_of(ALICE, "pc"),
        Some("phone")
    );
}

/// 满了租约才移出组;组空了就把失权的遥控器与走掉的那台交给调用方去通知。
#[test]
fn a_member_gone_past_the_lease_frees_the_controller() {
    let lease = Duration::from_secs(60);
    let mut control = Control::booted_at(lease, 0);
    phone_controls_pc(&mut control);
    let gone = Instant::now();

    control.member_left(ALICE, "pc", gone);

    assert_eq!(
        control.expire(ALICE, gone + lease),
        Some(("phone".to_owned(), "pc".to_owned()))
    );
    assert_eq!(control.controller_of(ALICE, "pc"), None);
}

/// 路由里过期的那一下把撤权送到遥控器,说是被控端走的。
#[test]
fn the_route_tells_the_controller_when_a_member_expires() {
    let (roster, mut rx_phone, _rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(Duration::ZERO, 0);
    phone_controls_pc(&mut control);
    control.member_left(ALICE, "pc", Instant::now());

    sweep(&roster, &mut control, ALICE);

    assert_eq!(
        rx_phone.try_recv(),
        Ok(ServerSignal::ControlRevoked {
            by: "pc".to_owned()
        })
    );
}

/// 上一个进程发的代次来续权:组随进程没了,当主动接管重建,不判「早被顶替」。
#[test]
fn a_resume_from_before_a_restart_rebuilds_the_group() {
    let lease = Duration::from_secs(60);
    let mut before = Control::booted_at(lease, 1_000);
    let Claim::Granted { generation, .. } =
        before.claim(ALICE, "phone", "pc", None)
    else {
        panic!("主动接管不该被拒");
    };
    let old_term = before.term(ALICE);

    let mut after = Control::booted_at(lease, 2_000);
    let claim =
        after.claim(ALICE, "phone", "pc", Some(generation));

    assert!(
        matches!(
            claim,
            Claim::Granted { revoked: None, .. }
        ),
        "重启前的代次该重建,得到 {claim:?}"
    );
    assert_eq!(
        after.controller_of(ALICE, "pc"),
        Some("phone")
    );
    assert!(
        after.term(ALICE) > old_term,
        "重启后的任期必须比重启前的大:{} <= {old_term}",
        after.term(ALICE)
    );
}

/// 本进程发的代次、组却没了(被控端退出过):仍然续不上 —— 重建只给重启那一种。
#[test]
fn a_resume_after_the_target_exited_is_still_stale() {
    let mut control = Control::booted_at(LEASE, 0);
    let generation = phone_controls_pc(&mut control);
    control.release(ALICE, "pc");

    assert!(matches!(
        control.claim(
            ALICE,
            "phone",
            "pc",
            Some(generation)
        ),
        Claim::Stale { .. }
    ));
}

/// 组散了再建,新组的任期也比旧组的大:客户端不理任期更低的通告。
#[test]
fn a_new_group_never_reuses_a_lower_term() {
    let mut control = Control::booted_at(LEASE, 0);
    phone_controls_pc(&mut control);
    let old_term = control.term(ALICE);
    control.release(ALICE, "pc");

    phone_controls_pc(&mut control);

    assert!(control.term(ALICE) > old_term);
}

/// 已经不在任何组里的设备还在上报:除了撤锁,还通告一份没有它的组,让它离开旧组。
#[test]
fn a_report_from_outside_any_group_is_told_to_leave() {
    let (roster, _rx_phone, mut rx_pc, _rx_spare) =
        three_devices();
    let mut control = Control::booted_at(LEASE, 0);

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State {
            state: Box::new(report()),
        },
    );

    assert_eq!(reply, Some(ServerSignal::NotControlled));
    assert!(matches!(
        rx_pc.try_recv(),
        Ok(ServerSignal::Group { members, .. }) if members.is_empty()
    ));
}
