//! 播放条翻面的落点计算(#82)。
//!
//! 几何真相放在 Rust 而不是 .slint:落点是一段能写死断言的算术,
//! 而无头测试驱动不了真实指针 —— 留在界面里就等于没有测试盖着
//! (progress.slint 那条拖动线的教训)。

use super::*;
use crate::Shell;

/// 一共几面。改这个数要同时改 playerbar.slint 的 `faces` 与各面宽度。
pub(crate) const FACES: i32 = 3;

/// 拖到多远才算一次翻面,逻辑像素。取条身高度的量级 ——
/// 比这短的位移在一条 56~62px 高的胶囊上更像是手指按住时的轻微游移。
const DRAG_THRESHOLD: f32 = 24.0;

/// 拖动(或滚轮)之后该停在哪一面。
///
/// `drag` 是本次手势的竖直位移,向上为负(与指针坐标同向)。往上拖等于把下一面
/// 推上来,所以负位移前进一面。
///
/// 一次手势最多翻一面:拖得再远也只走一格。跨两三面的话人看不清自己经过了什么,
/// 落在哪面全靠猜。两端不环绕 —— 三面是一条有头有尾的带子,首尾相接会让
/// 「我在第几面」失去参照。
pub(crate) fn snap_target(current: i32, drag: f32) -> i32 {
    let current = current.clamp(0, FACES - 1);
    let step = if drag <= -DRAG_THRESHOLD {
        1
    } else if drag >= DRAG_THRESHOLD {
        -1
    } else {
        0
    };
    (current + step).clamp(0, FACES - 1)
}

/// 把翻面手势接到界面上。界面报一次手势的位移,落点由上面那个函数算。
pub(crate) fn bind_flip(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.global::<Shell>().on_bar_flip_drag(move |drag| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let shell = ui.global::<Shell>();
        shell.set_bar_face(snap_target(
            shell.get_bar_face(),
            drag,
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 向上拖过阈值翻到下一面,向下拖过阈值翻回上一面。
    /// 方向跟着内容走:面往上走,等于看下一面。
    #[test]
    fn a_drag_past_the_threshold_turns_one_face() {
        assert_eq!(snap_target(0, -DRAG_THRESHOLD), 1);
        assert_eq!(snap_target(1, DRAG_THRESHOLD), 0);
    }

    /// 没拖到阈值就回原位。手指轻轻一碰不该把面翻走 ——
    /// 条上还压着一排按钮,误触的代价是操作错对象。
    #[test]
    fn a_short_drag_stays_put() {
        assert_eq!(snap_target(1, 0.0), 1);
        assert_eq!(snap_target(1, -DRAG_THRESHOLD + 0.1), 1);
        assert_eq!(snap_target(1, DRAG_THRESHOLD - 0.1), 1);
    }

    /// 拖得再远也只翻一面。一次手势跨两三面的话,
    /// 人根本看不清自己经过了什么,落在哪面全靠猜。
    #[test]
    fn one_gesture_turns_at_most_one_face() {
        assert_eq!(snap_target(0, -1000.0), 1);
        assert_eq!(snap_target(2, 1000.0), 1);
    }

    /// 两端不越界:第一面再往回、最后一面再往前,都停在原地。
    /// 三面是一条有头有尾的带子,不是首尾相接的环 ——
    /// 环会让「我在第几面」这件事失去参照。
    #[test]
    fn the_two_ends_do_not_wrap_around() {
        assert_eq!(snap_target(0, 1000.0), 0, "第一面再往回该停住");
        assert_eq!(
            snap_target(FACES - 1, -1000.0),
            FACES - 1,
            "最后一面再往前该停住"
        );
    }

    /// 面号越界的输入被夹回合法区间。面号从界面来,
    /// 界面的值可能因为别处的改动越界,落点计算不该跟着算飞。
    #[test]
    fn an_out_of_range_face_is_clamped() {
        assert_eq!(snap_target(99, 0.0), FACES - 1);
        assert_eq!(snap_target(-7, 0.0), 0);
        assert_eq!(snap_target(99, -1000.0), FACES - 1);
    }

    /// 手势真的接上了:界面报一把位移,面号跟着落到算出来的那一面。
    /// 算得对与接得上是两件事,上面几条只证得了前一件。
    #[test]
    fn the_gesture_is_wired_to_the_face() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        bind_flip(&ui);

        ui.global::<Shell>().set_bar_face(0);
        ui.global::<Shell>().invoke_bar_flip_drag(-100.0);
        assert_eq!(
            ui.global::<Shell>().get_bar_face(),
            1,
            "往上拖一把该翻到下一面"
        );

        ui.global::<Shell>().invoke_bar_flip_drag(100.0);
        assert_eq!(
            ui.global::<Shell>().get_bar_face(),
            0,
            "往下拖一把该翻回去"
        );
    }
}
