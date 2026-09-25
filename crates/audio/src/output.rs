//! 输出后端的门面：开一条声卡流，从混音器拉采样，每块把呈现时刻报给同步层。
//! 见 `output/README.md`。

use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rodio::mixer::{Mixer, MixerSource};

use crate::AudioError;
use crate::sync::SyncShared;

#[cfg(target_os = "android")]
mod aaudio;
#[cfg(target_os = "android")]
use aaudio as backend;

pub mod route;

#[cfg(not(target_os = "android"))]
mod cpal;
#[cfg(not(target_os = "android"))]
use self::cpal as backend;

/// 持流的那条线程多久醒一次，看流是不是断了要重开。
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// 暂停多久之后关流。
///
/// 流开着就得一直送静音，声卡与 PipeWire 图因此不休眠(#138)。关早了也不好：重开要几十到上百
/// 毫秒，暂停一下马上接着放的那种会多等这一截;组里跟随端重开后还要先静音追赶一下。30 秒远长于
/// 「切个应用回个消息」那种短暂停，又短到不至于让硬件白白醒着。
pub(crate) const CLOSE_AFTER_IDLE: Duration = Duration::from_secs(30);

/// 持流线程这一次醒来该对流做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Keep,
    /// 流关着，而现在要出声了。
    Open,
    /// 用不着声卡已经够久。
    Close,
    /// 流断了(设备没了、换了路由)。
    Reopen,
}

/// 记着声卡从什么时候起用不着了，据此决定开流、关流、重开。
#[derive(Debug, Default)]
pub(crate) struct Keeper {
    idle_since: Option<Instant>,
}

impl Keeper {
    /// `open`:流此刻开着没有;`idle`:见 `SyncShared::idle`;`broken`:后端报流断了。
    pub(crate) fn step(
        &mut self,
        now: Instant,
        open: bool,
        idle: bool,
        broken: bool,
    ) -> Step {
        Step::Keep
    }
}

/// 一条活着的输出。drop 掉就关流、收线程。
pub(crate) struct Output {
    pub mixer: Mixer,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 回调从这里拉采样。同一时刻只有一条流在用它;重开时新流接着拉同一个混音器。
pub(crate) type SharedMixer = Arc<Mutex<MixerSource>>;

/// 把混音器的采样填进声卡的缓冲。锁拿不到(不该发生：同一时刻只有一条流)就填静音。
pub(crate) fn fill(source: &SharedMixer, out: &mut [f32]) {
    match source.try_lock() {
        Ok(mut source) => {
            for sample in out.iter_mut() {
                *sample = source.next().unwrap_or(0.0);
            }
        }
        Err(_) => out.fill(0.0),
    }
}

/// 开默认输出设备。流关在一条专属线程里：流对象不跨线程，断了也在那条线程上重开。
pub(crate) fn open(
    shared: Arc<SyncShared>,
) -> Result<Output, AudioError> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("audio-output".to_owned())
        .spawn(move || {
            backend::run(shared, &ready_tx, &stop_rx)
        })
        .map_err(|e| AudioError::Device(e.to_string()))?;
    let mixer = ready_rx.recv().map_err(|_| {
        AudioError::Device(
            "输出线程没开出设备就退出了".to_owned(),
        )
    })??;
    Ok(Output {
        mixer,
        stop: stop_tx,
        thread: Some(thread),
    })
}

/// 持流线程的等待循环：收到关闭就走，流断了就交给 `reopen` 重开。
pub(crate) fn watch(
    stop: &mpsc::Receiver<()>,
    broken: &dyn Fn() -> bool,
    mut reopen: impl FnMut(),
) {
    loop {
        match stop.recv_timeout(WATCH_EVERY) {
            Ok(())
            | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if broken() {
                    reopen();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    /// 在放就一直开着。
    #[test]
    fn a_playing_output_stays_open() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(keeper.step(t0, true, false, false), Step::Keep);
        assert_eq!(keeper.step(t0 + 600 * SEC, true, false, false), Step::Keep);
    }

    /// 暂停满 30 秒才关，差一点都不关。
    #[test]
    fn it_closes_only_after_being_idle_long_enough() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(keeper.step(t0, true, true, false), Step::Keep);
        assert_eq!(
            keeper.step(t0 + CLOSE_AFTER_IDLE - SEC, true, true, false),
            Step::Keep
        );
        assert_eq!(
            keeper.step(t0 + CLOSE_AFTER_IDLE, true, true, false),
            Step::Close
        );
    }

    /// 中途放过一下，暂停的时长从头算。
    #[test]
    fn playing_in_between_restarts_the_idle_clock() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        keeper.step(t0, true, true, false);
        keeper.step(t0 + 20 * SEC, true, false, false);
        keeper.step(t0 + 21 * SEC, true, true, false);
        assert_eq!(
            keeper.step(t0 + 40 * SEC, true, true, false),
            Step::Keep,
            "第二段暂停才 19 秒"
        );
        assert_eq!(
            keeper.step(t0 + 21 * SEC + CLOSE_AFTER_IDLE, true, true, false),
            Step::Close
        );
    }

    /// 关着的流：还用不着就不开，一要出声就开。
    #[test]
    fn a_closed_output_opens_as_soon_as_it_is_needed() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(keeper.step(t0, false, true, false), Step::Keep);
        assert_eq!(
            keeper.step(t0 + 90 * SEC, false, true, false),
            Step::Keep,
            "关着的流不会被当成要关"
        );
        assert_eq!(keeper.step(t0 + 91 * SEC, false, false, false), Step::Open);
    }

    /// 断了的流：还要出声就重开;已经暂停够久就干脆关掉，不去重开一条马上要关的流。
    #[test]
    fn a_broken_output_reopens_unless_it_is_due_to_close() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(keeper.step(t0, true, false, true), Step::Reopen);
        assert_eq!(keeper.step(t0 + SEC, true, true, true), Step::Reopen);
        assert_eq!(
            keeper.step(t0 + SEC + CLOSE_AFTER_IDLE, true, true, true),
            Step::Close
        );
    }
}
