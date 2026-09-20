//! `super` 的测试:控制权槽位的规则,与遥控消息的转发。
//!
//! 单独成文件是因为实现加测试放一个文件就破了七百行那条线(全局协议
//! 「代码质量」)。测试本身没动过,连顺序都一样。

use contract::{
    ClientSignal, DeviceDto, RemoteCommand,
    RemotePlayState, RemoteStateDto, ServerSignal,
};
use similar_asserts::assert_eq;
use tokio::sync::mpsc;

use super::*;
use crate::roster::Roster;
use crate::signaling::{AccountId, Sink};

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
        queue: Vec::new(),
        queue_index: 0,
        volume: 1.0,
        sent_at: 42,
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
    let mut control = Control::default();

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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
    let mut control = Control::default();

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
    let mut control = Control::default();
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
    let mut control = Control::default();
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
        ClientSignal::State { state: report() },
    );

    assert_eq!(reply, None);
    assert_eq!(
        rx_phone.try_recv(),
        Ok(ServerSignal::State {
            from: "pc".to_owned(),
            state: report(),
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
    let mut control = Control::default();

    let reply = route(
        &roster,
        &mut control,
        ALICE,
        "pc",
        ClientSignal::State { state: report() },
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
    let mut control = Control::default();

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
    let mut control = Control::default();

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
    let mut control = Control::default();

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
    let mut control = Control::default();

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
    let mut control = Control::default();

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
    let mut control = Control::default();

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
    let mut control = Control::default();
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
        ClientSignal::State { state: report() },
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
