//! ⑤ 的多成员(#137 ⑤):改在这些设备播放、加入、移出，显式主端交接，以及结果不确定时
//! 逐台列出(AC-5.2)、成员失败不拖别人(AC-5.3)。

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

fn plan() -> Plan {
    Plan {
        queue_id: 7,
        revision: 3,
        entry_id: 12,
        position_ms: 61_500,
        playing: true,
        track: TrackDto {
            platform: "netease".to_owned(),
            id: "a".to_owned(),
            title: "A".to_owned(),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 240_000,
        },
    }
}

fn ack(op: &str, phase: OperationPhase, position_ms: Option<u64>) -> OperationAckDto {
    OperationAckDto {
        operation_id: op.to_owned(),
        phase,
        position_ms,
        reason: None,
    }
}

fn failed(op: &str, reason: &str) -> OperationAckDto {
    OperationAckDto {
        reason: Some(reason.to_owned()),
        ..ack(op, OperationPhase::Failed, None)
    }
}

/// 本机 id 叫 "me"、成员是 `members`、主端是 `master` 的会话。
fn grouped(members: Vec<Output>, master: Output) -> Session {
    Session {
        members,
        master,
        ..Session::with_me("me")
    }
}

fn start(op: &str, to: Output, position_ms: u64) -> Effect {
    Effect::Start {
        operation_id: op.to_owned(),
        to,
        position_ms,
        playing: true,
    }
}

fn stop(op: &str, from: Output) -> Effect {
    Effect::Stop {
        operation_id: op.to_owned(),
        from,
    }
}

fn commit(op: &str, outputs: Option<Vec<&str>>) -> Effect {
    Effect::Commit {
        operation_id: op.to_owned(),
        outputs: outputs.map(|ids| ids.into_iter().map(str::to_owned).collect()),
    }
}

// ── 加入 ──

/// 本机在放时加入 pc1:本机以自己的 id 登记、仍是主端，一个字节都不停;pc1 备好就跟上。
#[test]
fn adding_a_device_keeps_the_group_playing_and_joins_it() {
    let mut session = grouped(vec![Output::Local], Output::Local);

    let begun = session
        .change("op".to_owned(), vec![Output::Local, device("pc1")], Some(plan()), NOW)
        .expect("加入该开得起来");
    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: vec!["me".to_owned(), "pc1".to_owned()],
                master: Some("me".to_owned()),
            },
            Effect::Prepare {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                plan: plan(),
            },
        ],
        "本机不停、不重备"
    );
    assert!(!session.holds_transport(), "主端一直在响，控制不压");

    let started = session.on_ack(&device("pc1"), &ack("op", OperationPhase::Prepared, None), NOW + 100);
    assert_eq!(started, vec![start("op", device("pc1"), 61_500)]);

    let committed = session.on_ack(&device("pc1"), &ack("op", OperationPhase::Started, None), NOW + 200);
    assert_eq!(committed, vec![commit("op", None)]);
    assert_eq!(session.members(), &[Output::Local, device("pc1")]);
    assert_eq!(session.output(), &Output::Local, "主端还是本机");
}

// ── 移出 ──

/// 移出一台普通成员：只停它，别的照常;只剩本机时不经服务端。
#[test]
fn removing_a_follower_stops_only_it() {
    let mut session = grouped(vec![Output::Local, device("pc1")], Output::Local);

    let begun = session
        .change("op".to_owned(), vec![Output::Local], Some(plan()), NOW)
        .expect("移出该开得起来");
    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: Vec::new(),
                master: None,
            },
            stop("op", device("pc1")),
        ]
    );

    let committed = session.on_ack(&device("pc1"), &ack("op", OperationPhase::Stopped, Some(70_000)), NOW + 100);
    assert_eq!(committed, vec![commit("op", None)]);
    assert_eq!(session.members(), &[Output::Local]);
}

/// 移出主端：显式交给留下的第一台，它不重备、不重起 —— 它本来就在同一条时间线上。
#[test]
fn removing_the_master_hands_off_to_a_kept_member() {
    let mut session = grouped(vec![device("a"), device("b")], device("a"));

    let begun = session
        .change("op".to_owned(), vec![device("b")], Some(plan()), NOW)
        .expect("移出主端该开得起来");
    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: vec!["b".to_owned()],
                master: Some("b".to_owned()),
            },
            stop("op", device("a")),
        ]
    );
    assert!(session.holds_transport(), "交接那几秒命令先压着，不发给正在停的旧主端");

    let committed = session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(70_000)), NOW + 100);
    assert_eq!(committed, vec![commit("op", None)]);
    assert_eq!(session.output(), &device("b"), "命令从此发给新主端");
}

/// 移出最后一台：它停下，之后没有任何输出 —— 不自动回到本机，也不恢复被换掉的旧歌。
#[test]
fn removing_the_last_device_leaves_no_output_and_starts_nothing() {
    let mut session = grouped(vec![device("a")], device("a"));

    let begun = session
        .change("op".to_owned(), Vec::new(), Some(plan()), NOW)
        .expect("移出最后一台该开得起来");
    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: Vec::new(),
                master: None,
            },
            stop("op", device("a")),
        ],
        "不准备本机"
    );

    let committed = session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(70_000)), NOW + 100);
    assert_eq!(committed, vec![commit("op", None)]);
    assert!(session.members().is_empty());
}

// ── 改在这些设备播放(整组换人)──

/// 整组换成 b、c:都备好才停旧的，从旧主端停下的那一毫秒一起开始;都确认了才提交。
#[test]
fn replacing_the_group_starts_the_new_members_together_from_the_stop_anchor() {
    let mut session = grouped(vec![device("a")], device("a"));

    let begun = session
        .change("op".to_owned(), vec![device("b"), device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    assert_eq!(begun.len(), 3, "登记 + 两台准备: {begun:?}");
    assert_eq!(
        begun[0],
        Effect::Begin {
            operation_id: "op".to_owned(),
            outputs: vec!["b".to_owned(), "c".to_owned()],
            master: Some("b".to_owned()),
        }
    );

    assert_eq!(
        session.on_ack(&device("b"), &ack("op", OperationPhase::Prepared, None), NOW + 100),
        Vec::new(),
        "c 还在备，等它一会儿"
    );
    assert_eq!(
        session.on_ack(&device("c"), &ack("op", OperationPhase::Prepared, None), NOW + 200),
        vec![stop("op", device("a"))]
    );
    assert_eq!(
        session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 300),
        vec![start("op", device("b"), 63_000), start("op", device("c"), 63_000)]
    );
    assert!(session.holds_transport(), "新主端没确认开始之前命令压着");
    assert_eq!(
        session.on_ack(&device("b"), &ack("op", OperationPhase::Started, None), NOW + 400),
        Vec::new(),
        "c 还没确认"
    );
    assert_eq!(
        session.on_ack(&device("c"), &ack("op", OperationPhase::Started, None), NOW + 500),
        vec![commit("op", None)]
    );
    assert_eq!(session.output(), &device("b"));
}

/// 慢的那台不拖住全组：新主端备好后最多再等 READY_GRACE_MS,之后先开始，慢的备好了再跟上。
#[test]
fn a_slow_member_joins_later_instead_of_holding_the_group() {
    let mut session = grouped(vec![device("a")], device("a"));
    session
        .change("op".to_owned(), vec![device("b"), device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    session.on_ack(&device("b"), &ack("op", OperationPhase::Prepared, None), NOW + 100);

    assert_eq!(session.tick(NOW + 100 + READY_GRACE_MS - 1), Vec::new());
    assert_eq!(session.tick(NOW + 100 + READY_GRACE_MS), vec![stop("op", device("a"))]);
    assert_eq!(
        session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 3_200),
        vec![start("op", device("b"), 63_000)],
        "只叫备好的那台"
    );
    session.on_ack(&device("b"), &ack("op", OperationPhase::Started, None), NOW + 3_300);
    assert!(!session.holds_transport(), "新主端响了，控制放开");

    assert_eq!(
        session.on_ack(&device("c"), &ack("op", OperationPhase::Prepared, None), NOW + 5_000),
        vec![start("op", device("c"), 63_000)],
        "慢的备好了再跟上"
    );
    assert_eq!(
        session.on_ack(&device("c"), &ack("op", OperationPhase::Started, None), NOW + 5_100),
        vec![commit("op", None)]
    );
}

/// 过了准备期限还没好的那台：取消它，提交时不带它，说一句是谁、为什么。
#[test]
fn a_member_that_never_prepares_is_left_out_of_the_commit() {
    let mut session = grouped(vec![device("a")], device("a"));
    session
        .change("op".to_owned(), vec![device("b"), device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    session.on_ack(&device("b"), &ack("op", OperationPhase::Prepared, None), NOW + 100);
    session.tick(NOW + 100 + READY_GRACE_MS);
    session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 3_200);
    session.on_ack(&device("b"), &ack("op", OperationPhase::Started, None), NOW + 3_300);

    let effects = session.tick(NOW + PREPARE_TIMEOUT_MS + 1);

    assert_eq!(
        effects,
        vec![
            Effect::Cancel {
                operation_id: "op".to_owned(),
                to: device("c"),
            },
            commit("op", Some(vec!["b"])),
        ]
    );
    assert_eq!(session.members(), &[device("b")]);
    assert!(
        session.failure().is_some_and(|why| why.contains("设备 c")),
        "{:?}",
        session.failure()
    );
}

// ── AC-5.3:取不到媒体的成员报失败，别人照常 ──

/// 取不到媒体(准备失败)的成员报失败、不进集合;别的成员照常加入，本机一直在响。
#[test]
fn a_member_that_cannot_get_the_media_is_reported_and_left_out() {
    let mut session = grouped(vec![Output::Local], Output::Local);
    session
        .change(
            "op".to_owned(),
            vec![Output::Local, device("pc1"), device("tv")],
            Some(plan()),
            NOW,
        )
        .expect("加入该开得起来");

    assert_eq!(
        session.on_ack(&device("pc1"), &failed("op", "取不到媒体"), NOW + 100),
        Vec::new(),
        "失败的那台不叫停任何人"
    );
    assert_eq!(
        session.on_ack(&device("tv"), &ack("op", OperationPhase::Prepared, None), NOW + 200),
        vec![start("op", device("tv"), 61_500)]
    );
    assert_eq!(
        session.on_ack(&device("tv"), &ack("op", OperationPhase::Started, None), NOW + 300),
        vec![commit("op", Some(vec!["me", "tv"]))]
    );
    assert!(
        session.failure().is_some_and(|why| why.contains("设备 pc1") && why.contains("取不到媒体")),
        "{:?}",
        session.failure()
    );
}

/// 开始失败(比如跳不到要的位置)的普通成员同样不进集合，新主端照常提交。
#[test]
fn a_member_that_fails_to_start_is_left_out() {
    let mut session = grouped(vec![device("a")], device("a"));
    session
        .change("op".to_owned(), vec![device("b"), device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    session.on_ack(&device("b"), &ack("op", OperationPhase::Prepared, None), NOW + 100);
    session.on_ack(&device("c"), &ack("op", OperationPhase::Prepared, None), NOW + 200);
    session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 300);
    session.on_ack(&device("b"), &ack("op", OperationPhase::Started, None), NOW + 400);

    let effects = session.on_ack(&device("c"), &failed("op", "跳不到 63 秒"), NOW + 500);

    assert_eq!(effects, vec![commit("op", Some(vec!["b"]))]);
}

// ── AC-5.2:结果不确定 ──

/// 走的那台里有一台停没停不知道：不启动任何新来的(不引入集合外的新输出),逐台标出是哪台。
#[test]
fn an_unconfirmed_leaver_never_starts_the_new_members() {
    let mut session = grouped(vec![device("a"), device("b")], device("a"));
    session
        .change("op".to_owned(), vec![device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    session.on_ack(&device("c"), &ack("op", OperationPhase::Prepared, None), NOW + 100);
    session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 200);

    let effects = session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);

    assert_eq!(effects, Vec::new());
    let moving = session.moving().expect("该在待确认");
    assert_eq!(moving.phase, Phase::Unconfirmed(Doubt::SourceStop));
    let doubted: Vec<&Output> = moving
        .parties
        .iter()
        .filter(|party| party.progress == Progress::Unconfirmed)
        .map(|party| &party.output)
        .collect();
    assert_eq!(doubted, vec![&device("b")], "只列停没停不知道的那台");
    assert_eq!(session.members(), &[device("a"), device("b")], "集合不动");
}

/// 普通新成员开始没确认：它照样算在集合里(被授权的那台),逐台列出;迟到的确认把它划掉。
#[test]
fn unconfirmed_members_are_listed_one_by_one_and_cleared_by_a_late_ack() {
    let mut session = grouped(vec![device("a")], device("a"));
    session
        .change("op".to_owned(), vec![device("b"), device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    session.on_ack(&device("b"), &ack("op", OperationPhase::Prepared, None), NOW + 100);
    session.on_ack(&device("c"), &ack("op", OperationPhase::Prepared, None), NOW + 200);
    session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 300);
    session.on_ack(&device("b"), &ack("op", OperationPhase::Started, None), NOW + 400);

    let effects = session.tick(NOW + 300 + START_TIMEOUT_MS + 1);

    assert_eq!(effects, vec![commit("op", None)], "b 已经在响，不因为 c 拖住全组");
    assert_eq!(session.unconfirmed(), &[device("c")]);

    session.on_ack(&device("c"), &ack("op", OperationPhase::Started, None), NOW + 20_000);
    assert!(session.unconfirmed().is_empty());
}

/// 提交之后迟到的回话也认得出是谁：遥控器按设备 id 找到那一台再交给会话，「待确认」才划得掉。
/// 从前只在有进行中的那一次时才找(`party`),提交之后的迟到确认被丢掉，那一行一直挂着。
#[test]
fn a_member_is_found_by_id_after_the_change_is_committed() {
    let mut session = grouped(vec![device("a")], device("a"));
    session
        .change("op".to_owned(), vec![device("b"), device("c")], Some(plan()), NOW)
        .expect("换组该开得起来");
    session.on_ack(&device("b"), &ack("op", OperationPhase::Prepared, None), NOW + 100);
    session.on_ack(&device("c"), &ack("op", OperationPhase::Prepared, None), NOW + 200);
    session.on_ack(&device("a"), &ack("op", OperationPhase::Stopped, Some(63_000)), NOW + 300);
    session.on_ack(&device("b"), &ack("op", OperationPhase::Started, None), NOW + 400);
    session.tick(NOW + 300 + START_TIMEOUT_MS + 1);
    assert_eq!(session.moving(), None, "已经提交");

    let found = session.output_of("c").expect("待确认的那台该认得出");
    session.on_ack(&found, &ack("op", OperationPhase::Started, None), NOW + 20_000);

    assert!(session.unconfirmed().is_empty());
    assert_eq!(session.output_of("b"), Some(device("b")), "已确认的成员也认得出");
    assert_eq!(session.output_of("zz"), None);
}

/// 组里有人在响时再换：进行中那一次过了准备就不许插队。
#[test]
fn a_second_change_is_refused_once_members_are_leaving() {
    let mut session = grouped(vec![device("a"), device("b")], device("a"));
    session
        .change("op".to_owned(), vec![device("a")], Some(plan()), NOW)
        .expect("移出该开得起来");

    assert_eq!(
        session.change("op2".to_owned(), vec![device("a"), device("c")], Some(plan()), NOW + 10),
        Err(Refused::Busy)
    );
}
