//! 一次性提示的唯一出口。
//!
//! 界面上有两类文字,写法完全不同:
//!
//! - **状态的投影**:`playback-text` 是 `PlaybackState` 渲染出来的,`sync-text` 是
//!   同播角色渲染出来的。投影没有「清除」这回事 —— 状态一变就重算,于是它永远
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

/// 一句提示在屏幕上待多久。
///
/// 够长到能读完一句中文错误,短到用户回过神来时它已经不在了 —— 提示留得比它
/// 描述的那一刻还久,就是在说一件不再为真的事。
const NOTICE_LIFETIME: core::time::Duration =
    core::time::Duration::from_secs(8);

/// 说一句一次性的提示。
///
/// 到点自己收,但**只收自己那句**:这期间可能已经换成了别的提示或断流横幅,
/// 那些各有各的寿命,不该被上一句的计时器带走。比对文本就够认出来,不必为此
/// 再养一个代号 —— 两句一模一样的提示谁先收都是同一个结果。
pub fn show(ui: &MainWindow, text: String) {
    ui.set_banner_text(text.clone().into());

    let weak = ui.as_weak();
    slint::Timer::single_shot(NOTICE_LIFETIME, move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_banner_text() == text.as_str() {
            ui.set_banner_text(slint::SharedString::new());
        }
    });
}
