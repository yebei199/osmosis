//! 遥控器模式里那些纯判断与纯文案。
//!
//! 与 `music::rules` 同一个用意:界面接线里混着的判断挪出来,能不起窗口
//! 就测得到。它们是最容易写反、也最难从截图上看出写反了的那部分。

use app_core::{Output, RemotePlayState, RemoteView};

/// 界面上那几句写死在 `.slint` 里的遥控文案,在这里各留一份。
///
/// 抄一份不是为了让 Rust 用它们 —— 是为了让下面那条字体守卫覆盖得到。
/// `.slint` 里的中文没人守着,少一个字形在桌面上看不出来,到了安卓真机上
/// 就是一个方框。改了那边的措辞,这里跟着改。
///
/// 出处:`slint/drawer.slint` 的输出设备一行,`slint/app.slint` 的被遥控横幅。
#[cfg(test)]
pub const SLINT_COPY: &[&str] =
    &["输出设备", "本机", "退出被遥控"];

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
    if view.is_stale(now_ms) {
        return "遥控: 状态已过期".to_owned();
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

/// 被控端失联了、该把输出收回本机吗。
///
/// 撤权是服务端尽力而为的一发,丢了之后没有第二次(`server::syncplay::control` 的
/// `send`)。遥控器这头的 socket 还好好的,不会重连,也就走不到重连那条
/// 自愈;于是它永久停在「遥控: 状态已过期」,芯片还亮在那台设备上,
/// 而本机什么也放不了(#102 F-003)。这一条是那种情况下唯一的出口。
pub fn lost_remote(
    output: &Output,
    view: &RemoteView,
    now_ms: u64,
) -> bool {
    output.target().is_some() && view.is_lost(now_ms)
}

/// 自动回本机时那句提示。
///
/// 必须说出是哪台设备、以及现在声音在哪儿:不说的话,用户只看到歌换了个
/// 地方放,会以为是自己按错了。
pub fn describe_lost(output: &Output) -> String {
    let name = output.name().unwrap_or("那台设备");
    format!("{name} 失联,已回到本机")
}

/// 接管没成、回到本机时那句提示。
///
/// 与失联那句一样,要说出是哪台设备、以及声音现在在哪儿。原因(不在线、
/// 信令断了)记日志,不上界面:用户能做的只有一件事 —— 过会儿再选一次。
pub fn describe_claim_failed(output: &Output) -> String {
    let name = output.name().unwrap_or("那台设备");
    format!("没能接管 {name},已回到本机")
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

    /// 失联要收回输出,而过期不要 —— 两档的去向相反。
    #[test]
    fn only_a_lost_device_takes_the_output_back() {
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

    /// 自动回本机那句话要说清是哪台设备、声音现在在哪。
    #[test]
    fn the_lost_notice_names_the_device_and_where_sound_went()
     {
        assert_eq!(
            describe_lost(&remote()),
            "pc1 失联,已回到本机"
        );
    }

    /// 过期压过一切:后面那些字段全是旧闻,不许拿它们写「正在播放」。
    #[test]
    fn a_stale_view_says_so_instead_of_reading_out_old_news()
     {
        let view = view(RemotePlayState::Playing);

        assert_eq!(
            describe_remote(&view, 10_000),
            "遥控: 状态已过期"
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
        copy.push(describe_lost(&remote()));
        copy.push(describe_lost(&Output::Local));
        copy.push(describe_unavailable(&Output::Local));
        copy.push(describe_unavailable(&remote()));
        copy.push(describe_too_large(&Output::Local));
        copy.push(describe_too_large(&remote()));
        copy.push(describe_revoked(&remote(), "pc1"));
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
}
