//! 一次性提示的唯一出口。
//!
//! 界面上有两类文字,写法完全不同:
//!
//! - **状态的投影**:`playback-text` 是 `PlaybackState` 渲染出来的,`download-text`
//!   是下载进度渲染出来的。投影没有「清除」这回事 —— 状态一变就重算,于是它永远
//!   等于此刻的真相,过不了期。写它的只该有一处。
//! - **一次性事件**:某次请求失败了、名字没填、这首跳不了。它们描述的是**那一刻**,
//!   下一刻就未必还成立,所以必须自带寿命。
//!
//! 把第二类写进第一类的位置,是这个界面反复出问题的根源:投影的位置没人会去重算
//! 事件写下的那句话,只能等下一个写者恰好覆盖掉它。于是「取歌单失败」在歌单已经
//! 摆满之后还挂着,「失败: 上游超时」在上游恢复之后还挂着 —— 同一个错误的两次现形。
//! 每加一个写者,就多一个别人猜不到什么时候该清的句子,而写者之间互相不认识。
//!
//! 所以事件一律走这里,落到横幅上:横幅空串即整块不占位、可以手动关掉,并且
//! [`NOTICE_LIFETIME`] 到了自己会走。
//!
//! **持续性的状况不走这里**:断流横幅描述的是「现在没声音」这个仍在成立的条件,
//! 该由条件的结束(声音回来)来收,不该由计时器来收。那一处直接写 `banner-text`。

use slint::ComponentHandle;

use crate::MainWindow;
use crate::Shell;

/// 一句提示在屏幕上待多久。
///
/// 够长到能读完一句中文错误,短到用户回过神来时它已经不在了 —— 提示留得比它
/// 描述的那一刻还久,就是在说一件不再为真的事。
const NOTICE_LIFETIME: core::time::Duration =
    core::time::Duration::from_secs(8);

/// 个人页在导航里的位次(app.slint 里 `Shell.current-tab == 2` 那一支)。
const PROFILE_TAB: i32 = 2;

/// 点不了的那一句用这个位次。
const NOWHERE: i32 = -1;

/// 说一句一次性的提示。
///
/// 到点自己收,但**只收自己那句**:这期间可能已经换成了别的提示或断流横幅,
/// 那些各有各的寿命,不该被上一句的计时器带走。比对文本就够认出来,不必为此
/// 再养一个代号 —— 两句一模一样的提示谁先收都是同一个结果。
pub fn show(ui: &MainWindow, text: String) {
    show_at(ui, text, NOWHERE);
}

/// 同 [`show`],但这一句**点得动**:点它就去个人页。
///
/// 给出去处的那种提示才算说完整:「网易云未登录」告诉用户出了什么事,
/// 而他还得自己在四个页签里找到该去哪儿绑。
pub fn show_to_profile(ui: &MainWindow, text: String) {
    show_at(ui, text, PROFILE_TAB);
}

/// 摆一条**持续性状况**的横幅:断流那一类。
///
/// 它不自带寿命 —— 那句话描述的是一个仍在成立的条件,该由条件的结束(声音
/// 回来)来收,不该由计时器来收(见本模块开头)。但它仍然要经过这里:
/// `banner-tab` 归这个模块管。绕过去直接写 `banner-text` 的话,上一条提示
/// 留下的去处会挂到这一句上 —— 断流横幅于是成了「点一下跳个人页」,
/// 而点它还顺手把这句话清掉。
pub fn banner_without_link(ui: &MainWindow, text: String) {
    ui.global::<Shell>().set_banner_tab(NOWHERE);
    ui.global::<Shell>().set_banner_text(text.into());
}

/// 落一句提示,并说明它点下去去哪一页。
fn show_at(ui: &MainWindow, text: String, tab: i32) {
    ui.global::<Shell>().set_banner_tab(tab);
    ui.global::<Shell>()
        .set_banner_text(text.clone().into());

    let weak = ui.as_weak();
    slint::Timer::single_shot(NOTICE_LIFETIME, move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.global::<Shell>().get_banner_text()
            == text.as_str()
        {
            ui.global::<Shell>().set_banner_text(slint::SharedString::new());
        }
    });
}

#[cfg(test)]
mod tests {
    use slint::ComponentHandle as _;

    use crate::{MainWindow, Shell};

    /// 上一条提示的去处不能挂到下一条横幅上。
    ///
    /// 提示被关掉或过期时清的只有文案,`banner-tab` 还留着 —— 下一条横幅
    /// 若绕过本模块直接写文案,它就会继承那个去处。断流那句话于是变成
    /// 「点一下跳个人页」,点它还顺手把断流的提示清掉。
    #[test]
    fn a_persistent_banner_never_inherits_the_last_link() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");

        super::show_to_profile(
            &ui,
            "网易云未登录,去个人页扫码绑定".to_owned(),
        );
        // 提示被收掉(关闭键只清文案),随后声音断了
        ui.global::<Shell>()
            .set_banner_text(slint::SharedString::new());
        super::banner_without_link(
            &ui,
            "声音没了".to_owned(),
        );

        assert_eq!(
            ui.global::<Shell>().get_banner_tab(),
            -1,
            "断流横幅不该点得动 —— 那会把人送到一个与断流无关的页面"
        );
    }

    /// 每个投影只有一个模块写得动它。
    ///
    /// 这条规矩本身是拦不住编译器的:往 `playback-text` 里写一句报错照样过编译,
    /// 而它过期之后没有任何东西会去重算 —— 这个界面已经为此出过三次同样的错。
    /// 所以在这里当着源码点名:谁写的、写的是不是自己那份状态。
    ///
    /// 新加的写者如果确实在渲染状态本身,把文件名添进 `owners` 即可;如果写的是
    /// 一次性提示,那它该走 [`super::show`]。
    #[test]
    fn only_the_owner_writes_a_projection() {
        // 分成两截拼,免得这个测试在源码里留下自己要找的字样 —— 它会举报自己。
        let projections: [(String, &[&str]); 1] = [
            // 播放状态行:music 渲染 PlaybackState;remote.rs 在输出设备不是本机
            // 时渲染被控端报来的状态,而本机的 playback 此刻是空的。
            (
                format!("set_{}", "playback_text"),
                &["music.rs", "remote.rs"],
            ),
        ];

        let src = std::path::Path::new(env!(
            "CARGO_MANIFEST_DIR"
        ))
        .join("src");

        for (setter, owners) in &projections {
            for entry in std::fs::read_dir(&src)
                .expect("src 目录读不到")
            {
                let path =
                    entry.expect("目录项读不到").path();
                if path
                    .extension()
                    .is_none_or(|ext| ext != "rs")
                {
                    continue;
                }
                let name = path
                    .file_name()
                    .expect("文件名")
                    .to_string_lossy()
                    .into_owned();
                let body = std::fs::read_to_string(&path)
                    .expect("源码读不到");

                assert!(
                    !body.contains(setter.as_str())
                        || owners.contains(&name.as_str()),
                    "{name} 在写 {setter},而那是投影,不是它的状态 —— \
                     一次性的提示走 crate::notice::show",
                );
            }
        }
    }
}
