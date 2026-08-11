//! 明暗主题:开局恢复上次的选择,改了写回去。
//!
//! 颜色本身全在 `slint/theme.slint` 的 `Theme` 全局里,这个模块只管**那三档
//! 选择住在哪**:真相在 `api::settings`(`ThemeMode`,深/浅/跟随系统),
//! 跟着设备走 —— 与音量同一条理由,同一个账号在办公室的亮屏和床上的暗屏
//! 想要的不是同一套。
//!
//! 「跟随系统」读 std-widgets 的 `Palette.color-scheme`(winit 报系统偏好,
//! 变了会触发 `MainWindow` 上的观察器转发回来);拿不到(Unknown)按深色
//! 处理。`Theme.dark` 与 `Theme.mode` 的唯一写入方是这儿。

use api::settings::{self, Settings, ThemeMode};
use slint::ComponentHandle;

use crate::{MainWindow, Theme};

/// 恢复上次的选择并接上设置页。
pub(crate) fn bind(ui: &MainWindow) {
    apply(ui, settings::load().theme);

    // 设置页的唯一入口:日月开关拨显式明/暗,「跟随系统」开关报 2,
    // 档位换算在 .slint 侧做完,这里只收档位序号(#68 起,控制簇不再有主题键)。
    let weak = ui.as_weak();
    ui.on_theme_mode_selected(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        let mode = mode_from_index(index);
        settings::save(&Settings {
            theme: mode,
            ..settings::load()
        });
        apply(&ui, mode);
    });

    // 系统主题变了(`MainWindow` 上的 changed 观察器转发)。只在跟随系统档生效。
    let weak = ui.as_weak();
    ui.on_system_scheme_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.global::<Theme>().get_mode() == 2 {
            let dark = system_prefers_dark(&ui);
            ui.global::<Theme>().set_dark(dark);
        }
    });
}

/// 把一档选择写进 `Theme`:mode 给设置页画选中态,dark 是解出来的结论。
fn apply(ui: &MainWindow, mode: ThemeMode) {
    ui.global::<Theme>().set_mode(mode_index(mode));
    ui.global::<Theme>().set_dark(match mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => system_prefers_dark(ui),
    });
}

/// 系统现在偏好深色吗。真相由 `MainWindow.system-prefers-dark` 镜像
/// std-widgets 的 `Palette.color-scheme`(unknown 已在那边折成深色)。
fn system_prefers_dark(ui: &MainWindow) -> bool {
    ui.get_system_prefers_dark()
}

/// 设置页分段控件的序号 ↔ 档位。序号只在 UI 与这儿之间走,不落盘。
fn mode_index(mode: ThemeMode) -> i32 {
    match mode {
        ThemeMode::Dark => 0,
        ThemeMode::Light => 1,
        ThemeMode::System => 2,
    }
}

fn mode_from_index(index: i32) -> ThemeMode {
    match index {
        1 => ThemeMode::Light,
        2 => ThemeMode::System,
        _ => ThemeMode::Dark,
    }
}
