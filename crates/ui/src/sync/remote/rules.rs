//! 遥控器模式里那些纯判断与纯文案。
//!
//! 与 `music::rules` 同一个用意:界面接线里混着的判断挪出来,能不起窗口
//! 就测得到。它们是最容易写反、也最难从截图上看出写反了的那部分。

use std::collections::HashMap;

use app_core::{
    Doubt, Move, Output, OutputRouteDto, Phase, Progress,
    RemotePlayState, RemoteView, Role, Session, Step,
};

/// 没做过声学校准的路由(蓝牙、有线/USB)上的成员，组那一行这样标(#137 ⑤ 冻结的合同)。
pub const UNCALIBRATED: &str = "该路由未校准,不保证同步";

/// 界面上那几句写死在 `.slint` 里的遥控文案,在这里各留一份。
///
/// 抄一份不是为了让 Rust 用它们 —— 是为了让下面那条字体守卫覆盖得到。
/// `.slint` 里的中文没人守着,少一个字形在桌面上看不出来,到了安卓真机上
/// 就是一个方框。改了那边的措辞,这里跟着改。
///
/// 出处:`slint/drawer.slint` 的输出设备一行,`slint/app.slint` 的被遥控横幅。
#[cfg(test)]
pub const SLINT_COPY: &[&str] =
    &["输出设备", "本机", "退出被遥控", "加入", "移出"];

/// 输出设备那一行怎么写。
///
/// 「输出设备 = 谁」就是遥控器模式的全部状态(产品规则),所以这一行
/// 永远在,选本机时写「本机」而不是空着 —— 空着会让人以为功能没开。
pub fn describe_output(output: &Output) -> String {
    output.name().map_or_else(
        || "输出: 本机".to_owned(),
        |name| format!("输出: {name}"),
    )
}

/// 被控端横幅怎么写。没被遥控时是空串 —— 空串就是不显示。
pub fn describe_controlled(by: Option<&str>) -> String {
    by.map_or_else(String::new, |name| {
        format!("正被 {name} 遥控")
    })
}

/// 遥控时播放状态那一行怎么写。
///
/// 过期排在最前面:那时后面那些字段全是几秒前的旧闻,照着它们写
/// 「正在播放 X」是在撒谎,而用户看不出区别。
pub fn describe_remote(
    view: &RemoteView,
    now_ms: u64,
) -> String {
    // 还没收到过上报:接管刚发出去,快照还在路上。与过期分开说 ——
    // 「过期」是曾经知道又失去了,而这里是还没开始。
    if !view.is_known() {
        return "遥控: 正在连接".to_owned();
    }
    // 不说「已过期」、也不自己回本机(#142):被控端多半在重连,服务端替它留着租约。
    // 真的不回来了,服务端的撤权会把输出收回本机。
    if view.is_stale(now_ms) {
        return "遥控: 重连中…".to_owned();
    }
    match view.state() {
        RemotePlayState::Idle => "遥控: 没在放".to_owned(),
        RemotePlayState::Buffering => {
            "遥控: 缓冲中…".to_owned()
        }
        RemotePlayState::Paused => {
            "遥控: 已暂停".to_owned()
        }
        RemotePlayState::Playing => {
            view.track().map_or_else(
                || "遥控: 正在播放".to_owned(),
                |track| {
                    format!(
                        "遥控: 正在播放 {}",
                        track.title
                    )
                },
            )
        }
    }
}

/// 失去控制权时那句提示。
///
/// 两种失权长得完全不一样,而服务端发来的是同一条信令(`ControlRevoked`):
/// 别的设备抢了控制权,或者被控端自己按了「退出被遥控」。分不开的话,
/// pc1 上的人点一下退出,遥控器上写的是「遥控已被 pc1-353238 接管」——
/// 语义正好反了,而那串 id 用户从没见过。
///
/// 判据是 `by` 就是当前输出的那台设备:抢权的一定是**另一台**,自己退出的
/// 才会是自己。名字也从输出那份取 —— 信令里带的是 id。
pub fn describe_revoked(
    output: &Output,
    by: &str,
) -> String {
    if output.target() == Some(by) {
        let name = output.name().unwrap_or(by);
        return format!("{name} 已退出被遥控");
    }
    format!("遥控已被 {by} 接管")
}

/// 被控端久不上报,该向服务端核一次还持不持权吗(#142)。
///
/// 撤权是服务端尽力而为的一发,丢了之后没有第二次(`server::syncplay::control` 的
/// `send`)。遥控器这头的 socket 还好好的,不会重连,也就走不到重连那条
/// 自愈(#102 F-003)。从前这里直接收回本机,可闪断的被控端有租约、还会回来,
/// 于是遥控器自己掉回本机而被控端还在放。现在只去问服务端,由它裁决。
pub fn lost_remote(
    output: &Output,
    view: &RemoteView,
    now_ms: u64,
) -> bool {
    output.target().is_some() && view.is_lost(now_ms)
}

/// 接管没成、回到本机时那句提示。
///
/// 要说出是哪台设备、为什么、以及声音现在在哪儿(#142:重新接管失败要给出看得懂的原因)。
/// `reason` 是客户端给的那一行(服务端的错误码,或者信令断开),翻成人话;认不出的
/// 不照抄 —— 那是给日志看的。
pub fn describe_claim_failed(
    output: &Output,
    reason: &str,
) -> String {
    let name = output.name().unwrap_or("那台设备");
    let why = if reason.contains("device_offline") {
        "它不在线"
    } else if reason.contains("信令断开") {
        "本机连不上服务端"
    } else if reason.contains("cannot_control_self") {
        "那就是本机"
    } else {
        "服务端没有同意"
    };
    format!("接管 {name} 失败({why}),已回到本机")
}

/// 目标在别的设备、但这一下没提交成功时说的那句话。
///
/// **不回落本机**是这句话存在的理由(`docs/adr/0030`)。过期时把声音抢回
/// 遥控器自己这台,用户一低头就发现歌从手机里放出来了 —— 而他要的是让
/// pc1 放。既然不回落,就必须说一句,否则按下去与坏掉毫无区别。
pub fn describe_unavailable(output: &Output) -> String {
    output.name().map_or_else(
        || "控制暂不可用".to_owned(),
        |name| format!("{name} 控制暂不可用"),
    )
}

/// 一条命令大到发不出去时说的那句话。
///
/// 必须说得出「为什么」与「现在能怎么办」:它与「控制暂不可用」是两回事 ——
/// 那个等一等就好了,这个等多久都不会好,得换一个短一点的列表
/// (根治见 #109)。不说的话,用户会一直点同一首歌。
pub fn describe_too_large(output: &Output) -> String {
    output.name().map_or_else(
        || "队列太长,暂时发不过去".to_owned(),
        |name| format!("队列太长,暂时发不到 {name}"),
    )
}

/// 这一下控制动作该不该发出去。
///
/// 过期时不发:那份状态已经不知道被控端在干什么了,照着它发命令等于蒙 ——
/// 而用户会看到「按了没反应」,再按几下,然后一次全到。
///
/// 但**刚接管、一条上报都还没回来时要发**:从选中设备到第一条上报回来是
/// 三个来回,而「选完设备马上点一首歌」是最自然的操作顺序。把那一下也挡掉
/// 的话,得到的同样是「按了没反应」—— 只是原因反过来了。
pub fn accepts_control(
    output: &Output,
    view: &RemoteView,
    now_ms: u64,
) -> bool {
    output.target().is_some() && !view.is_stale(now_ms)
}

/// 迁移那几秒,状态行那一句怎么写(#137 ③)。
///
/// 每一步都点出在等谁:用户盯着的是控制条,「正在切换」四个字说不出卡在哪 ——
/// 而「待确认」要他去处理,就必须说清楚不确定的是哪一台的哪件事。
pub fn describe_move(moving: &Move) -> String {
    let name = |end: &Output| {
        end.name().unwrap_or("本机").to_owned()
    };
    // 加人减人、或者一次换好几台(#137 ⑤):逐台说走到哪了。
    if moving.keeps_playing() || moving.parties.len() > 2 {
        return describe_parties(moving);
    }
    let (to, from) = (name(&moving.to), name(&moving.from));
    match moving.phase {
        Phase::Running(Step::Preparing) => {
            format!("正在切到 {to}:等它准备好")
        }
        Phase::Running(Step::Stopping) => {
            format!("正在切到 {to}:停下 {from}")
        }
        Phase::Running(Step::Starting) => {
            format!("正在切到 {to}:等它开始播放")
        }
        Phase::Unconfirmed(Doubt::SourceStop) => format!(
            "切到 {to} 待确认:{from} 停没停不知道,{to} 先不放"
        ),
        Phase::Unconfirmed(Doubt::TargetStart) => format!(
            "切到 {to} 待确认:{to} 起没起不知道,{from} 不自动恢复"
        ),
    }
}

/// 跟随端照已确认的计划放完、主端还是没动静：停下时说的那一句(#137 ⑤)。
pub fn describe_master_lost() -> String {
    "主端失联:已按确认的计划放完,停在这里。重新选择设备继续"
        .to_owned()
}

/// 跟随端取不到组计划要的那一首时报的故障。它不自己从头放、不换下一首。
pub fn describe_media_fault(why: &str) -> String {
    format!("取不到媒体: {why}")
}

/// 跟随端手上那一版队列里没有计划要的那一条。
pub fn describe_missing_entry(
    revision: i64,
    entry_id: i64,
) -> String {
    format!("第 {revision} 版里没有条目 {entry_id}")
}

/// 跟随端取不下计划那一版的队列。
pub fn describe_copy_fault(why: &str) -> String {
    format!("队列没取下来: {why}")
}

/// 多台一起换时，状态行逐台说：谁在加入、谁在移出、各自走到哪。「待确认」要说清楚是哪一台。
fn describe_parties(moving: &Move) -> String {
    let parts: Vec<String> = moving
        .parties
        .iter()
        .map(|party| {
            let name =
                party.output.name().unwrap_or("本机");
            let verb = match party.role {
                Role::Join => "加入",
                Role::Leave => "移出",
            };
            let progress = match &party.progress {
                Progress::Waiting => "排队",
                Progress::Preparing => "准备中",
                Progress::Prepared => "已备好",
                Progress::Stopping => "停止中",
                Progress::Stopped => "已停",
                Progress::Starting => "开始中",
                Progress::Started => "已跟上",
                Progress::Failed(_) => "失败",
                Progress::Unconfirmed => "待确认",
            };
            format!("{verb} {name}({progress})")
        })
        .collect();
    let head = match moving.phase {
        Phase::Unconfirmed(Doubt::SourceStop) => {
            "待确认:有设备停没停不知道,新加入的先不放"
        }
        Phase::Unconfirmed(Doubt::TargetStart) => {
            "待确认:新主端起没起不知道,原来的不自动恢复"
        }
        Phase::Running(_) => "正在调整一起播放的设备",
    };
    format!("{head}:{}", parts.join("、"))
}

/// 播放组那一行(#137 ⑤):组里有哪几台、谁是主端，哪几台待确认、哪几台报了故障。
///
/// 只有本机(或一台不剩)且没有要说的故障时是空串，那一行不出现。
///
/// 蓝牙、有线/USB 的成员标「该路由未校准,不保证同步」:同步合同只对电脑扬声器 + 手机扬声器做过
/// 声学校准(#137 ⑤)。`routes` 按设备 id,本机是空串。
pub fn describe_group(
    session: &Session,
    faults: &HashMap<String, String>,
    routes: &HashMap<String, OutputRouteDto>,
) -> String {
    let name = |output: &Output| {
        output.name().unwrap_or("本机").to_owned()
    };
    let members = session.members();
    let mut parts = Vec::new();
    if members.len() > 1 {
        let listed: Vec<String> = members
            .iter()
            .map(|member| {
                if member.target()
                    == session.output().target()
                {
                    format!("{}(主端)", name(member))
                } else {
                    name(member)
                }
            })
            .collect();
        parts.push(format!(
            "一起播放: {}",
            listed.join("、")
        ));
    }
    for member in session.unconfirmed() {
        parts.push(format!(
            "{} 待确认:开始了没有不知道",
            name(member)
        ));
    }
    for member in members {
        if let Some(why) =
            member.target().and_then(|id| faults.get(id))
        {
            parts.push(format!("{}: {why}", name(member)));
        }
    }
    if members.len() > 1 {
        for member in members {
            let route = routes
                .get(member.target().unwrap_or_default());
            if matches!(
                route,
                Some(
                    OutputRouteDto::Bluetooth
                        | OutputRouteDto::Wired
                )
            ) {
                parts.push(format!(
                    "{}: {UNCALIBRATED}",
                    name(member)
                ));
            }
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use app_core::{DeviceDto, RemoteStateDto, TrackDto};
    use similar_asserts::assert_eq;

    use super::*;

    fn remote() -> Output {
        Output::Remote(DeviceDto {
            id: "pc1".to_owned(),
            name: "pc1".to_owned(),
        })
    }

    fn track() -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: "1".to_owned(),
            title: "Gurenge".to_owned(),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 200_000,
        }
    }

    fn view(state: RemotePlayState) -> RemoteView {
        let mut view = RemoteView::default();
        view.accept(
            RemoteStateDto {
                track: Some(track()),
                position_ms: 0,
                state,
                volume: 1.0,
                queue_id: Some(7),
                revision: Some(1),
                applied_revision: Some(1),
                entry_id: Some(12),
                queue_len: 1,
                epoch: 1_700_000_000_000,
                state_seq: 1,
                operation: None,
                fault: None,
                route: None,
            },
            0,
        );
        view
    }

    /// 选本机时也写一行 —— 不留空。
    #[test]
    fn the_output_row_names_the_local_machine_too() {
        assert_eq!(
            describe_output(&Output::Local),
            "输出: 本机"
        );
        assert_eq!(describe_output(&remote()), "输出: pc1");
    }

    /// 没被遥控时横幅是空的 —— 空串就是不显示。
    #[test]
    fn the_banner_is_empty_when_nobody_is_in_control() {
        assert_eq!(describe_controlled(None), "");
        assert_eq!(
            describe_controlled(Some("小米13")),
            "正被 小米13 遥控"
        );
    }

    /// 被控端自己退出,和被别的设备抢走,是两句相反的话。
    ///
    /// 服务端两种情况发的是同一条信令,只有 `by` 不同 —— 不比这一下的话,
    /// pc1 上的人点「退出被遥控」,遥控器上却写「遥控已被 pc1 接管」。
    #[test]
    fn a_controlled_device_leaving_is_not_a_takeover() {
        assert_eq!(
            describe_revoked(&remote(), "pc1"),
            "pc1 已退出被遥控"
        );
        assert_eq!(
            describe_revoked(&remote(), "别的设备"),
            "遥控已被 别的设备 接管"
        );
        assert_eq!(
            describe_revoked(&Output::Local, "pc1"),
            "遥控已被 pc1 接管"
        );
    }

    /// 失联才去核持权,过期不必 —— 过期只是这几秒没来。
    #[test]
    fn only_a_lost_device_is_checked_with_the_server() {
        let view = view(RemotePlayState::Playing);

        assert!(
            !lost_remote(&remote(), &view, 10_000),
            "过期了但没失联,声音该留在那台设备上"
        );
        assert!(lost_remote(&remote(), &view, 20_000));
        assert!(
            !lost_remote(&Output::Local, &view, 20_000),
            "输出本来就在本机,没什么可收回的"
        );
        assert!(
            !lost_remote(
                &remote(),
                &RemoteView::default(),
                u64::MAX
            ),
            "一条上报都没来过,那是还没开始,不是失联"
        );
    }

    /// 过期压过一切:后面那些字段全是旧闻,不许拿它们写「正在播放」。
    #[test]
    fn a_stale_view_says_so_instead_of_reading_out_old_news()
     {
        let view = view(RemotePlayState::Playing);

        assert_eq!(
            describe_remote(&view, 10_000),
            "遥控: 重连中…"
        );
    }

    /// 四种状态各有各的说法,曲名跟着走。
    #[test]
    fn each_remote_state_gets_its_own_line() {
        assert_eq!(
            describe_remote(
                &view(RemotePlayState::Playing),
                0
            ),
            "遥控: 正在播放 Gurenge"
        );
        assert_eq!(
            describe_remote(
                &view(RemotePlayState::Buffering),
                0
            ),
            "遥控: 缓冲中…"
        );
        assert_eq!(
            describe_remote(
                &view(RemotePlayState::Paused),
                0
            ),
            "遥控: 已暂停"
        );
        assert_eq!(
            describe_remote(
                &view(RemotePlayState::Idle),
                0
            ),
            "遥控: 没在放"
        );
    }

    /// 本机输出时控制动作不走遥控这条路 —— 它该落到本地播放器上。
    #[test]
    fn local_output_never_sends_a_command() {
        let view = view(RemotePlayState::Playing);

        assert!(!accepts_control(&Output::Local, &view, 0));
    }

    /// 遥控且状态新鲜时才发得出去。
    #[test]
    fn a_fresh_remote_view_accepts_control() {
        let view = view(RemotePlayState::Playing);

        assert!(accepts_control(&remote(), &view, 0));
    }

    /// 刚接管、快照还在路上时,控制要能发出去。
    ///
    /// 挡掉的话,选完设备马上点的那一首会被静默丢掉 —— 那正是这条
    /// 判断本来要防住的「按了没反应」,只是原因反过来了。
    #[test]
    fn a_just_claimed_view_still_accepts_control() {
        let view = RemoteView::default();

        assert!(accepts_control(&remote(), &view, 0));
    }

    /// 那一刻界面说的是「正在连接」,不是「状态已过期」——
    /// 后者会让人以为出了故障,而实际上一切正常,只是还没回来。
    #[test]
    fn a_view_awaiting_its_first_report_says_so() {
        assert_eq!(
            describe_remote(&RemoteView::default(), 0),
            "遥控: 正在连接"
        );
    }

    /// 过期时按键不发命令。
    ///
    /// 发了的话用户会看到「按了没反应」,再按几下,然后一次全到 ——
    /// 歌一口气跳过去五首。
    #[test]
    fn a_stale_remote_view_refuses_control() {
        let view = view(RemotePlayState::Playing);

        assert!(!accepts_control(&remote(), &view, 10_000));
    }

    /// 本模块吐出的中文必须在子集字体里 —— 与 music、syncplay 那两条同一个守卫。
    ///
    /// 新增文案而忘了重跑 `just font-subset`,这里就红并报出缺的是哪个字。
    /// 设备名与歌名不在此列:它们是对端自报的任意文本,不可能预裁。
    #[test]
    fn remote_copy_only_uses_subset_glyphs() {
        const CJK_SUBSET: &[u8] =
            include_bytes!("../../../fonts/cjk-subset.ttf");

        let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
            .expect("子集字体应能被解析");

        let mut copy: Vec<String> = SLINT_COPY
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
        copy.push(describe_output(&Output::Local));
        copy.push(describe_controlled(None));
        copy.push(describe_revoked(&Output::Local, "pc1"));
        copy.push(describe_unavailable(&Output::Local));
        copy.push(describe_unavailable(&remote()));
        copy.push(describe_too_large(&Output::Local));
        copy.push(describe_too_large(&remote()));
        copy.push(describe_revoked(&remote(), "pc1"));
        // #142 新加的几句。
        for reason in [
            "device_offline",
            "信令断开",
            "cannot_control_self",
            "x",
        ] {
            copy.push(describe_claim_failed(&remote(), reason));
        }
        for line in [
            "这首已经在放或正在切过去",
            "遥控已结束,刚才那次点歌没有执行",
            "本机正被遥控",
            "已经在这些设备上播放",
            "正在重新接管 pc1",
        ] {
            copy.push(line.to_owned());
        }
        for state in [
            RemotePlayState::Idle,
            RemotePlayState::Buffering,
            RemotePlayState::Paused,
            RemotePlayState::Playing,
        ] {
            copy.push(describe_remote(&view(state), 0));
        }
        copy.push(describe_remote(
            &RemoteView::default(),
            0,
        ));
        // 变量部分喂 ASCII:检查的是文案里的固定字。
        copy.push(describe_controlled(Some("pc1")));
        // 播放组(#137 ⑤)。
        copy.push(describe_master_lost());
        copy.push(describe_media_fault("x"));
        copy.push(describe_missing_entry(1, 2));
        copy.push(describe_copy_fault("x"));
        let mut grouped = Session::with_me("me");
        let _ = grouped.change(
            "op".to_owned(),
            vec![Output::Local, remote()],
            None,
            0,
        );
        copy.push(describe_group(
            &grouped,
            &HashMap::from([(
                "pc1".to_owned(),
                "x".to_owned(),
            )]),
            &HashMap::from([(
                "pc1".to_owned(),
                OutputRouteDto::Bluetooth,
            )]),
        ));
        copy.push("待确认:开始了没有不知道".to_owned());
        for head in [
            "待确认:有设备停没停不知道,新加入的先不放",
            "待确认:新主端起没起不知道,原来的不自动恢复",
            "正在调整一起播放的设备",
            "加入移出排队准备中已备好停止中已停开始中已跟上失败",
        ] {
            copy.push(head.to_owned());
        }

        let missing: Vec<char> = copy
            .iter()
            .flat_map(|line| line.chars())
            .filter(|c| {
                !c.is_ascii()
                    && face.glyph_index(*c).is_none()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "子集字体缺字形:{missing:?} —— 重跑 just font-subset"
        );
    }

    /// 接管失败说得出原因,认不出的原因不照抄(#142)。
    #[test]
    fn a_failed_claim_says_why_in_plain_words() {
        assert_eq!(
            describe_claim_failed(
                &remote(),
                "device_offline: 设备 pc 不在线"
            ),
            "接管 pc1 失败(它不在线),已回到本机"
        );
        assert_eq!(
            describe_claim_failed(&remote(), "weird: 42"),
            "接管 pc1 失败(服务端没有同意),已回到本机"
        );
    }

    /// 迁移的每一步都说出在等谁;「待确认」说清楚不确定的是哪一台的哪件事,
    /// 以及此刻**不做**什么。
    #[test]
    fn a_move_says_who_it_is_waiting_for() {
        use app_core::{Plan, Session};

        let pc = Output::Remote(app_core::DeviceDto {
            id: "pc".to_owned(),
            name: "pc1".to_owned(),
        });
        let mut session = Session::default();
        session
            .begin(
                "op".to_owned(),
                pc.clone(),
                Some(Plan {
                    queue_id: 1,
                    revision: 1,
                    entry_id: 1,
                    position_ms: 0,
                    playing: true,
                    track: app_core::TrackDto {
                        platform: "netease".to_owned(),
                        id: "a".to_owned(),
                        title: "A".to_owned(),
                        alias: None,
                        artists: Vec::new(),
                        cover: None,
                        duration_ms: 1,
                    },
                }),
                0,
            )
            .expect("迁移该开得起来");
        let preparing = describe_move(
            session.moving().expect("该在迁移"),
        );
        assert!(preparing.contains("pc1"), "{preparing}");

        session.on_ack(
            &pc,
            &app_core::OperationAckDto {
                operation_id: "op".to_owned(),
                phase: app_core::OperationPhase::Prepared,
                position_ms: None,
                reason: None,
            },
            1,
        );
        session.tick(1 + app_core::STOP_TIMEOUT_MS + 1);
        let doubt = describe_move(
            session.moving().expect("该在待确认"),
        );
        assert!(doubt.contains("待确认"), "{doubt}");
        assert!(
            doubt.contains("本机"),
            "要点出是哪台停没停不知道: {doubt}"
        );
        assert!(
            doubt.contains("先不放"),
            "要说清楚此刻不做什么: {doubt}"
        );
    }

    fn device(id: &str) -> Output {
        Output::Remote(DeviceDto {
            id: id.to_owned(),
            name: id.to_owned(),
        })
    }

    fn plan() -> app_core::Plan {
        app_core::Plan {
            queue_id: 1,
            revision: 1,
            entry_id: 1,
            position_ms: 0,
            playing: true,
            track: track(),
        }
    }

    /// 只有本机时那一行不出现;几台一起放时点名主端。
    #[test]
    fn the_group_row_names_the_members_and_the_master() {
        let session = Session::with_me("me");
        assert_eq!(
            describe_group(
                &session,
                &HashMap::new(),
                &HashMap::new()
            ),
            ""
        );

        let mut grouped = Session::with_me("me");
        let _ = grouped.change(
            "op".to_owned(),
            vec![Output::Local, device("pc1")],
            None,
            0,
        );
        assert_eq!(
            describe_group(
                &grouped,
                &HashMap::new(),
                &HashMap::new()
            ),
            "一起播放: 本机(主端)、pc1"
        );
    }

    /// 某台报了故障：逐台点名，说出原因。
    #[test]
    fn the_group_row_lists_member_faults_one_by_one() {
        let mut grouped = Session::with_me("me");
        let _ = grouped.change(
            "op".to_owned(),
            vec![
                Output::Local,
                device("pc1"),
                device("tv"),
            ],
            None,
            0,
        );
        let faults = HashMap::from([(
            "tv".to_owned(),
            "取不到媒体".to_owned(),
        )]);

        let row = describe_group(
            &grouped,
            &faults,
            &HashMap::new(),
        );

        assert!(row.contains("tv: 取不到媒体"), "{row}");
        assert!(
            !row.contains("pc1:"),
            "没报故障的不点名: {row}"
        );
    }

    /// 加入一台时状态行说的是「加入 pc1」,不是「切到本机」。
    #[test]
    fn adding_a_member_is_described_as_joining() {
        let mut session = Session::with_me("me");
        let _ = session.change(
            "op".to_owned(),
            vec![Output::Local, device("pc1")],
            Some(plan()),
            0,
        );

        let text = describe_move(
            session.moving().expect("该在进行"),
        );

        assert!(
            text.contains("加入 pc1(准备中)"),
            "{text}"
        );
    }

    /// 蓝牙、有线的成员标「该路由未校准」;扬声器与查不出来的不标(查不出来不替它下结论)。
    #[test]
    fn members_on_uncalibrated_routes_are_marked() {
        let mut grouped = Session::with_me("me");
        let _ = grouped.change(
            "op".to_owned(),
            vec![
                Output::Local,
                device("pc1"),
                device("tv"),
            ],
            None,
            0,
        );
        let routes = HashMap::from([
            (String::new(), OutputRouteDto::Speaker),
            ("pc1".to_owned(), OutputRouteDto::Bluetooth),
        ]);

        let row = describe_group(
            &grouped,
            &HashMap::new(),
            &routes,
        );

        assert!(
            row.contains(&format!("pc1: {UNCALIBRATED}")),
            "{row}"
        );
        assert!(
            !row.contains(&format!("本机: {UNCALIBRATED}")),
            "{row}"
        );
        assert!(
            !row.contains(&format!("tv: {UNCALIBRATED}")),
            "查不出来不下结论: {row}"
        );
    }
}
