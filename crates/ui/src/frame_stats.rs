//! 一帧耗时的记账,以及可选的 FPS 读数定时器。

use slint::ComponentHandle;

use crate::MainWindow;

/// 每帧耗时的记账窗口(帧)。约两秒一行,与 render3d 的采样窗口对齐,便于两边日志对读。
const FRAME_ACCT_WINDOW: u32 = 120;

/// 把一帧切成三段:我们的回调、Slint 的渲染、以及空等。
///
/// 时间轴:`BeforeRendering` → 回调(组装参数 + 驱动渲染器)→ Slint 画整个界面 →
/// `AfterRendering` → 空等下一次 vsync → 下一个 `BeforeRendering`。
///
/// 只量到「回调外」是不够的:那一段里「在画」和「干等」混在一起,而两者的优化方向
/// 相反 —— 前者要减工作量,后者说明我们没超预算、该去看浏览器的呈现策略。
/// `AfterRendering` 正好落在二者的分界上。
#[derive(Default)]
pub(crate) struct FrameAccounting {
    /// 本帧进入回调的时刻,兼作 `AfterRendering` 的计时基准。
    start: Option<web_time::Instant>,
    /// 上一帧进入回调的时刻。首帧为 `None`,不计周期。
    last_start: Option<web_time::Instant>,
    /// 窗口内累计:(回调内, 回调进入→画完, 整帧周期),毫秒。
    totals: (f64, f64, f64),
    frames: u32,
}

impl FrameAccounting {
    /// 记录本帧起点,顺带累加与上一帧的间隔(即整帧周期)。
    pub(crate) fn begin_frame(&mut self) {
        let now = web_time::Instant::now();
        if let Some(prev) = self.last_start {
            self.totals.2 +=
                (now - prev).as_secs_f64() * 1000.0;
            self.frames += 1;
        }
        self.last_start = Some(now);
        self.start = Some(now);
    }

    /// 回调返回时调用,累加回调自身的耗时。
    pub(crate) fn end_callback(&mut self) {
        if let Some(start) = self.start {
            self.totals.0 +=
                start.elapsed().as_secs_f64() * 1000.0;
        }
    }

    /// Slint 画完时调用(`AfterRendering`);满一个窗口就打一行均值并清零。
    pub(crate) fn end_rendering(&mut self) {
        let Some(start) = self.start else { return };
        self.totals.1 +=
            start.elapsed().as_secs_f64() * 1000.0;
        if self.frames < FRAME_ACCT_WINDOW {
            return;
        }
        let n = f64::from(self.frames);
        let (callback, drawn, period) = (
            self.totals.0 / n,
            self.totals.1 / n,
            self.totals.2 / n,
        );
        log::info!(
            "ui: 近 {} 帧 —— 整帧 {period:.2}ms({:.0}fps)= 回调 {callback:.2}ms + Slint 渲染 {:.2}ms + 空等 {:.2}ms",
            self.frames,
            1000.0 / period,
            drawn - callback,
            period - drawn,
        );
        self.totals = (0.0, 0.0, 0.0);
        self.frames = 0;
    }
}

/// 帧率计。恒编译,开关在运行期([`fps_enabled`])—— 关掉时调用方不建定时器,这里零成本。
///
/// 帧数由调用方在渲染通知回调里累加**真实发生的**帧,每个采样周期算出帧率推给 UI。
/// 计数器不在这里接进渲染通知:一个窗口只能装一个通知回调,而 3D 路径要拿它当帧驱动
/// (见 [`run_with_renderer`](crate::run_with_renderer))—— 两边都装的话后者会顶掉前者,
/// 读数静默归零。故本模块只出计数器和采样定时器,由谁装通知、在哪儿 `bump` 交给调用方。
///
/// 刻意不主动请求重绘 —— Slint 是惰性渲染,空闲时本就不重绘,读数会自动趴到
/// ~1(交互/动画时才飙高),这正是诚实的即时帧率,也不会白耗电。3D 页每帧自请求重绘,
/// 这里自然就读到满帧。
pub(crate) mod fps {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use slint::{ComponentHandle, Timer, TimerMode};

    use crate::MainWindow;

    /// 采样周期。帧率 = 本周期内累计的帧数 / 周期秒数。
    const SAMPLE_PERIOD: Duration =
        Duration::from_millis(500);

    /// 启动帧率计,返回(待调用方每帧累加的计数器, 采样定时器)。
    ///
    /// [`Timer`] 必须由调用方持有到事件循环结束,否则会被立即析构、不再触发。
    pub(crate) fn start(
        ui: &MainWindow,
    ) -> (Rc<Cell<u32>>, Timer) {
        let frames = Rc::new(Cell::new(0u32));

        let weak_fps = ui.as_weak();
        let timer = Timer::default();
        let frames_sample = frames.clone();
        timer.start(
            TimerMode::Repeated,
            SAMPLE_PERIOD,
            move || {
                let counted = frames_sample.replace(0);
                if let Some(ui) = weak_fps.upgrade() {
                    ui.set_fps(
                        counted as f32
                            / SAMPLE_PERIOD.as_secs_f32(),
                    );
                }
            },
        );
        (frames, timer)
    }
}
