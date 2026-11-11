//! 「动态效果」开关与设置页的日夜开关。
//!
//! 「动态效果」关着就是减弱动效:`.slint` 里的弹簧换成 200ms 的 ease-out(`Motion.calm`
//! 跟着 `Shell.motion-on` 走),
//! 日夜开关的弹簧也收成临界阻尼。值的真相在 `api::settings`(`motion`),跟着设备走。
//!
//! 日夜开关是 unicat-switch(用户自己的仓库)的 `UnicatSwitch`,像素在 CPU 上画。
//! [`unicat_switch_slint::Driver`] 持有弹簧、拖动与天色,只在弹簧还在动时开一只 16ms
//! 的定时器,每一拍把 `DayNight.tick` 加一 —— 属性变了 Slint 才重画那一块,弹簧停了
//! 定时器就停,空闲时不出帧(`docs/adr/0035`)。驱动器由下面几个回调闭包共同持有,
//! 活得和窗口一样久。

use slint::ComponentHandle;
use unicat_switch_slint::{
    Commit, Driver, Frame, Input, View,
};

use crate::{
    DayNight, MainWindow, Shell, SwitchCommit, SwitchFrame,
    SwitchInput,
};

/// 恢复「动态效果」,接上设置页的拨动与日夜开关的驱动器。
pub(crate) fn bind(ui: &MainWindow) {
    let weak = ui.as_weak();
    let driver = Driver::new(move |tick| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<DayNight>().set_tick(tick);
        }
    });

    apply(ui, &driver, api::settings::load().motion);

    let weak = ui.as_weak();
    let toggled = driver.clone();
    ui.global::<Shell>().on_motion_toggled(move || {
        let Some(ui) = weak.upgrade() else { return };
        let on = !ui.global::<Shell>().get_motion_on();
        apply(&ui, &toggled, on);
        api::settings::save(&api::settings::Settings {
            motion: on,
            ..api::settings::load()
        });
    });

    let render = driver.clone();
    ui.global::<DayNight>().on_render(
        move |width, height, pad, scale, night, _tick| {
            let px = |v: i32| u32::try_from(v).unwrap_or(0);
            let Frame { image, phase, glow } = render
                .render(View {
                    width: px(width),
                    height: px(height),
                    pad: px(pad),
                    scale,
                    night,
                });
            SwitchFrame { image, phase, glow }
        },
    );
    ui.global::<DayNight>().on_input(move |kind, x| {
        let input = match kind {
            SwitchInput::Down => Input::Down,
            SwitchInput::Move => Input::Move,
            SwitchInput::Up => Input::Up,
            SwitchInput::Cancel => Input::Cancel,
            SwitchInput::Leave => Input::Leave,
        };
        match driver.input(input, x) {
            Commit::None => SwitchCommit::None,
            Commit::Toggle => SwitchCommit::Toggle,
            Commit::Set(false) => SwitchCommit::Day,
            Commit::Set(true) => SwitchCommit::Night,
        }
    });
}

/// 把开关落到界面与驱动器上:`.slint` 的弹簧经 `Motion.calm` 读同一个值。
fn apply(ui: &MainWindow, driver: &Driver, on: bool) {
    ui.global::<Shell>().set_motion_on(on);
    driver.set_reduced_motion(!on);
}
