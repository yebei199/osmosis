//! 明暗主题:开局恢复上次的选择,拨一下写回去。
//!
//! 颜色本身全在 `slint/theme.slint` 的 `Theme` 全局里,这个模块只管**那一位
//! 布尔值住在哪**:真相在 `api::settings`,跟着设备走 —— 与音量同一条理由,
//! 同一个账号在办公室的亮屏和床上的暗屏想要的不是同一套。
//!
//! 日月开关自己不置位(见 `slint/widgets.slint` 的 `DayNightSwitch`),拨一下
//! 只喊一声 `theme-toggled`,值由这里写进 `Theme.dark`。唯一的写入方是这儿。

use slint::ComponentHandle;

use crate::{MainWindow, Theme};

/// 恢复上次的选择并接上开关。
pub(crate) fn bind(ui: &MainWindow) {
    ui.global::<Theme>()
        .set_dark(api::settings::load().dark);

    let weak = ui.as_weak();
    ui.on_theme_toggled(move || {
        let Some(ui) = weak.upgrade() else { return };
        let dark = !ui.global::<Theme>().get_dark();
        ui.global::<Theme>().set_dark(dark);

        // **先读再改**:整份重造的话,这个文件里的音量会被这次换主题
        // 顺手冲回默认值。
        api::settings::save(&api::settings::Settings {
            dark,
            ..api::settings::load()
        });
    });
}
