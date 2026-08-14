//! 两处要说给用户听的话:断流的横幅,以及启动时连不上服务端的提示。

use super::*;
use crate::Player;
use crate::Shell;
use slint::ComponentHandle as _;

/// 声音放到一半没了:停下,弹横幅,再去问清是哪一种没了。
///
/// 先弹粗文案,不等探测 —— 等的话最坏要让用户对着没声音的界面干等二十多秒,
/// 那个区间里他已经在想"是不是卡死了"(见 `docs/adr/0013`)。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn report_stream_loss(
    ui: &MainWindow,
    deck: &Deck,
) {
    // 证据取走即清空。这个条件会一直成立到下次换歌,不清的话横幅每秒重弹一次。
    deck.stream.borrow_mut().take();

    let opening = describe_stream_loss(None);
    deck.playback.borrow_mut().fail(opening.to_owned());
    ui.global::<Player>().set_is_playing(false);
    ui.global::<Player>().set_playback_text(
        describe_playback(deck.playback.borrow().state())
            .into(),
    );
    ui.global::<Shell>().set_banner_text(opening.into());

    // 探测结果回来了再把话说准。探不通=本机没网,探得通=这条播放地址不行了。
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let reachable = api::health().await.is_ok();
        if let Some(ui) = weak.upgrade() {
            // 期间用户可能已经把横幅关了,或者又放起了别的歌 —— 那就不打扰他。
            if !ui
                .global::<Shell>()
                .get_banner_text()
                .is_empty()
            {
                ui.global::<Shell>().set_banner_text(
                    describe_stream_loss(Some(reachable))
                        .into(),
                );
            }
        }
    })
    .expect("event loop must be running");
}

/// 开机静默自检:`GET /health` 一次,健康就一声不吭。
///
/// Server 页删掉之后,这是协议版本协商唯一的运行时入口(`api::health` 内部
/// 比对 `PROTOCOL_VERSION`)。
///
/// 坏消息走横幅:这是**开机那一刻**的一次探测,不是一个会自己更新的状态。
/// 写进播放状态行的话,上游恢复之后没有任何东西会重算它,那句「失败: 上游超时」
/// 就一直挂在歌单顶上(见 `crate::notice`)。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn startup_check(ui: &MainWindow) {
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let result = api::health().await.map(|_dto| ());
        if let Some(message) = describe_startup(&result)
            && let Some(ui) = weak.upgrade()
        {
            crate::notice::show(&ui, message);
        }
    })
    .expect("event loop must be running");
}
