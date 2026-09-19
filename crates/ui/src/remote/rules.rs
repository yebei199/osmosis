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

/// 控制权被别人接管时那句提示。
pub fn describe_revoked(by: &str) -> String {
    format!("遥控已被 {by} 接管")
}

/// 这一下控制动作该不该发出去。
///
/// 过期时不发:那份状态已经不知道被控端在干什么了,照着它发命令等于蒙 ——
/// 而用户会看到「按了没反应」,再按几下,然后一次全到。
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
                queue: vec![track()],
                queue_index: 0,
                volume: 1.0,
                sent_at: 1,
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
            include_bytes!("../../fonts/cjk-subset.ttf");

        let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
            .expect("子集字体应能被解析");

        let mut copy: Vec<String> = SLINT_COPY
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
        copy.push(describe_output(&Output::Local));
        copy.push(describe_controlled(None));
        copy.push(describe_revoked("pc1"));
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
