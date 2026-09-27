//! 按需渲染的节拍(#153,`docs/adr/0035`):渲染循环什么时候要下一帧,以及
//! 不经 Slint 属性就改了画面的那些到达怎么叫醒它。
//!
//! 两半:
//! - **每帧的需求** [`Demand`]:各渲染器交回「我还在过渡 / 我有东西没画完」(`busy`,
//!   无条件要下一帧)与「我有常驻的环境动效」(`ambient`,只在活跃期内要)。
//! - **活跃期** [`Pace`]:用户最近一次输入起 [`AMBIENT_IDLE`] 之内算活跃;换歌换封面
//!   这类状态变化只放行 [`SETTLE`] 那么一小段。过了就定格,Slint 进入空闲。
//!
//! 唤醒只有三个入口,全是 `Shell` 上的回调:`user-input`(`.slint` 的输入探针喊)、
//! `settle` 与 `wake`(Rust 侧经 [`settle`] / [`wake`] 喊)。上一代按需渲染败在
//! 「每个动效各发明一套冻结与唤醒」,这里把唤醒收成一处,谁改了画面谁就走这一处。

use core::time::Duration;
use std::cell::Cell;
use std::rc::Rc;

use slint::ComponentHandle;
use web_time::Instant;

use crate::{MainWindow, Shell};

/// 连续多久没有用户输入,常驻的环境动效(流体胶囊、播放键光环、点云、星云)就定格。
pub const AMBIENT_IDLE: Duration = Duration::from_secs(10);

/// 换歌、换封面之后环境动效再走多久。点云的换歌渐变是 0.9 秒
/// (render3d 的 `COLOR_FADE_SECS`),余下的留给封面纹理上传那一两帧。
pub const SETTLE: Duration = Duration::from_millis(1500);

/// 一帧里某个渲染器交回的需求。多个渲染器用 `|=` 并起来。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Demand {
    /// 无论活不活跃都要下一帧:这一帧刚换了图(差一帧才上屏)、过渡还没走完、
    /// 纹理还没就绪、还有封面没传完。都是有尽头的。
    pub busy: bool,
    /// 画面里有常驻的环境动效,活跃期内要一直动下去。
    pub ambient: bool,
}

impl Demand {
    /// 要不要下一帧。
    pub fn next_frame(self, live: bool) -> bool {
        self.busy || (live && self.ambient)
    }
}

impl core::ops::BitOrAssign for Demand {
    fn bitor_assign(&mut self, other: Self) {
        self.busy |= other.busy;
        self.ambient |= other.ambient;
    }
}

/// 活跃期的截止时刻。渲染循环与 `Shell` 的三个回调共享同一份。
#[derive(Debug)]
pub struct Pace {
    live_until: Cell<Instant>,
}

impl Pace {
    /// 开局算一次输入:首屏的动效先走一段,首帧不冻在还没就绪的纹理上。
    pub fn new(now: Instant) -> Self {
        Self {
            live_until: Cell::new(now + AMBIENT_IDLE),
        }
    }

    /// 用户动了:环境动效从 `now` 起再活 [`AMBIENT_IDLE`]。
    pub fn touch(&self, now: Instant) {
        self.extend(now + AMBIENT_IDLE);
    }

    /// 状态变了、要一小段过渡:放行到 `now + SETTLE`,不缩短已有的活跃期。
    pub fn settle(&self, now: Instant) {
        self.extend(now + SETTLE);
    }

    /// `now` 还在活跃期内吗。
    pub fn live(&self, now: Instant) -> bool {
        now < self.live_until.get()
    }

    fn extend(&self, until: Instant) {
        if until > self.live_until.get() {
            self.live_until.set(until);
        }
    }
}

/// 把 `Shell` 的三个回调接到一份 [`Pace`] 上,交回它给渲染循环读。
///
/// 三个回调都顺手请求一次重绘:活跃期只有在有帧的时候才被读到。
pub fn bind(ui: &MainWindow) -> Rc<Pace> {
    let pace = Rc::new(Pace::new(Instant::now()));
    let redraw = {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        }
    };
    let shell = ui.global::<Shell>();
    {
        let (pace, redraw) = (pace.clone(), redraw.clone());
        shell.on_user_input(move || {
            pace.touch(Instant::now());
            redraw();
        });
    }
    {
        let (pace, redraw) = (pace.clone(), redraw.clone());
        shell.on_settle(move || {
            pace.settle(Instant::now());
            redraw();
        });
    }
    shell.on_wake(redraw);
    pace
}

/// 用户动了,但指针没挪(光标不动的滚轮、读屏动作)。`.slint` 的输入探针只认
/// 指针挪动,这些得由接住它们的回调自己报。
pub fn input(ui: &MainWindow) {
    ui.global::<Shell>().invoke_user_input();
}

/// 画面变了、只要一帧:缩略图到货这类。
pub fn wake(ui: &MainWindow) {
    ui.global::<Shell>().invoke_wake();
}

/// 状态变了、要一小段过渡:换歌、换封面。
pub fn settle(ui: &MainWindow) {
    ui.global::<Shell>().invoke_settle();
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// 开局那一刻算一次输入:首屏的动效先走,到了空闲时限才定格。
    #[test]
    fn a_fresh_pace_is_live_until_the_idle_limit() {
        let t0 = Instant::now();
        let pace = Pace::new(t0);

        assert!(pace.live(t0));
        assert!(pace.live(t0 + AMBIENT_IDLE - MS));
        assert!(
            !pace.live(t0 + AMBIENT_IDLE),
            "整整十秒没人碰,环境动效该定格了"
        );
    }

    /// 定格之后任何输入立即恢复,并且从那一刻起重新计满十秒。
    #[test]
    fn user_input_revives_ambient_motion() {
        let t0 = Instant::now();
        let pace = Pace::new(t0);
        let later = t0 + AMBIENT_IDLE * 3;
        assert!(!pace.live(later));

        pace.touch(later);

        assert!(pace.live(later));
        assert!(pace.live(later + AMBIENT_IDLE - MS));
        assert!(!pace.live(later + AMBIENT_IDLE));
    }

    /// 换歌换封面只放行一小段:走完过渡就定格,不当成一次完整的输入。
    #[test]
    fn a_state_change_only_grants_a_short_settle() {
        let t0 = Instant::now();
        let pace = Pace::new(t0);
        let idle = t0 + AMBIENT_IDLE * 3;

        pace.settle(idle);

        assert!(pace.live(idle + SETTLE - MS));
        assert!(!pace.live(idle + SETTLE));
    }

    /// 活跃期里来一次换歌,不能把剩下的活跃期截短。
    #[test]
    fn a_state_change_never_shortens_a_live_window() {
        let t0 = Instant::now();
        let pace = Pace::new(t0);

        pace.settle(t0);

        assert!(pace.live(t0 + AMBIENT_IDLE - MS));
    }

    /// 过渡中(busy)无论活不活跃都要下一帧;环境动效只在活跃期内要;
    /// 什么都没有就不要 —— 这是「静止零重绘」的那一格。
    #[test]
    fn demand_decides_the_next_frame() {
        let still = Demand::default();
        let ambient = Demand {
            busy: false,
            ambient: true,
        };
        let busy = Demand {
            busy: true,
            ambient: false,
        };

        assert_eq!(still.next_frame(true), false);
        assert_eq!(ambient.next_frame(true), true);
        assert_eq!(ambient.next_frame(false), false);
        assert_eq!(busy.next_frame(false), true);
    }

    /// 三个回调各自落到活跃期上:输入续十秒,换歌只续一小段,wake 不动活跃期。
    #[test]
    fn the_shell_callbacks_drive_the_pace() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        let pace = bind(&ui);
        pace.live_until.set(Instant::now());

        wake(&ui);
        assert!(
            !pace.live(Instant::now()),
            "wake 只要一帧,不开活跃期"
        );

        settle(&ui);
        assert!(pace.live(Instant::now()));
        assert!(!pace.live(Instant::now() + SETTLE * 2));

        ui.global::<Shell>().invoke_user_input();
        assert!(pace.live(Instant::now() + SETTLE * 2));
        assert!(
            !pace.live(Instant::now() + AMBIENT_IDLE * 2)
        );
    }
}
