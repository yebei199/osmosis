//! 用户动作的分段打点(#121)。
//!
//! 一个动作(进每日推荐、打开歌单、点歌)在发起那一刻领一个 id,之后每到一个
//! 阶段打一行 `act#<id> <动作> <阶段> +<本段>ms total=<累计>ms`。同一个 id 的
//! 几行按时间排开,就是这一下的分段耗时;夹在它们中间的 `api:` 行是它发出的请求。
//!
//! 最后一段 `drawn` 在渲染循环里打:模型写进去之后,下一次 `AfterRendering`
//! 才是用户看见它的那一刻。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use web_time::Instant;

/// 一个正在进行的用户动作。
pub(crate) struct Action {
    id: u64,
    name: &'static str,
    started: Instant,
    last: Cell<Instant>,
}

impl Action {
    /// 发起一个动作,领 id,打第一行 `begin`。
    pub(crate) fn begin(name: &'static str) -> Rc<Self> {
        // ponytail: 进程级计数器只用来发号,不承载别的状态
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now = Instant::now();
        let action = Rc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            name,
            started: now,
            last: Cell::new(now),
        });
        action.mark("begin");
        action
    }

    /// 打一行:这一段(距上一个阶段)与累计(距发起)各多少毫秒。
    pub(crate) fn mark(&self, stage: &str) {
        let now = Instant::now();
        let lap = now - self.last.replace(now);
        log::info!(
            "act#{} {} {stage} +{}ms total={}ms",
            self.id,
            self.name,
            lap.as_millis(),
            (now - self.started).as_millis(),
        );
    }
}

/// 等下一帧画出来的那些动作。渲染循环每画完一帧交一次 [`Frames::drawn`]。
#[derive(Clone, Default)]
pub(crate) struct Frames(Rc<RefCell<Vec<Rc<Action>>>>);

impl Frames {
    /// 模型刚写进去:下一帧画完时给它打 `drawn`。
    pub(crate) fn after_next_frame(
        &self,
        action: Rc<Action>,
    ) {
        self.0.borrow_mut().push(action);
    }

    /// 一帧画完了。
    pub(crate) fn drawn(&self) {
        for action in self.0.take() {
            action.mark("drawn");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, Once};

    use similar_asserts::assert_eq;

    use super::{Action, Frames};

    // `log` 一个进程只认一个 logger:装一次,各用例按自己独有的动作名筛自己那几行。
    static LINES: Mutex<Vec<String>> =
        Mutex::new(Vec::new());

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            LINES
                .lock()
                .unwrap_or_else(|poisoned| {
                    poisoned.into_inner()
                })
                .push(record.args().to_string());
        }

        fn flush(&self) {}
    }

    fn capture() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = log::set_logger(&Capture);
            log::set_max_level(log::LevelFilter::Info);
        });
    }

    fn lines_of(name: &str) -> Vec<String> {
        let marker = format!(" {name} ");
        LINES
            .lock()
            .unwrap_or_else(|poisoned| {
                poisoned.into_inner()
            })
            .iter()
            .filter(|line| line.contains(&marker))
            .cloned()
            .collect()
    }

    /// 行首的 `act#<id>`。
    fn id_of(line: &str) -> &str {
        line.split_whitespace().next().unwrap_or_default()
    }

    /// 一个动作的每一段都带着同一个 id,按发生的次序排开,每段都有数字 ——
    /// 验收要的「按 id 串出分段耗时」就是这件事。
    #[test]
    fn stages_of_one_action_line_up_under_its_id() {
        capture();
        let action = Action::begin("probe-stages");
        action.mark("response");
        action.mark("model");

        let lines = lines_of("probe-stages");
        let stages: Vec<&str> = lines
            .iter()
            .map(|line| {
                line.split_whitespace()
                    .nth(2)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(stages, ["begin", "response", "model"]);
        assert!(
            id_of(&lines[0]).starts_with("act#"),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|line| id_of(line) == id_of(&lines[0])),
            "同一个动作的几行 id 不一致: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| line.contains("ms total=")),
            "有一段没带耗时: {lines:?}"
        );
    }

    /// 两个动作领到的 id 不同 —— 并发的两次加载才分得开。
    #[test]
    fn two_actions_get_different_ids() {
        capture();
        let _first = Action::begin("probe-first");
        let _second = Action::begin("probe-second");

        let first = lines_of("probe-first");
        let second = lines_of("probe-second");
        assert_eq!((first.len(), second.len()), (1, 1));
        assert_ne!(id_of(&first[0]), id_of(&second[0]));
    }

    /// 等帧的动作在下一帧画完时恰好打一次 `drawn`,之后的帧不再打。
    #[test]
    fn a_waiting_action_is_marked_drawn_exactly_once() {
        capture();
        let frames = Frames::default();
        frames
            .after_next_frame(Action::begin("probe-drawn"));

        frames.drawn();
        frames.drawn();

        let drawn: Vec<String> = lines_of("probe-drawn")
            .into_iter()
            .filter(|line| line.contains(" drawn "))
            .collect();
        assert_eq!(drawn.len(), 1, "{drawn:?}");
    }
}
