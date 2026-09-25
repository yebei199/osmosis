//! 输出后端的门面：开一条声卡流，从混音器拉采样，每块把呈现时刻报给同步层。
//! 见 `output/README.md`。

use std::sync::atomic::{AtomicBool, Ordering};
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
pub(crate) const CLOSE_AFTER_IDLE: Duration =
    Duration::from_secs(30);

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
        if idle {
            self.idle_since.get_or_insert(now);
        } else {
            self.idle_since = None;
        }
        let due = self.idle_since.is_some_and(|since| {
            now.duration_since(since) >= CLOSE_AFTER_IDLE
        });
        match (open, idle) {
            (false, false) => Step::Open,
            (false, true) => Step::Keep,
            (true, _) if due => Step::Close,
            (true, _) if broken => Step::Reopen,
            (true, _) => Step::Keep,
        }
    }
}

/// 流关着、又用不着声卡的时候，持流线程多久替声卡拉一次混音器。
///
/// rodio 的 `Player::clear` 要等拉采样的那一方把旧源摘掉才返回(`sleep_until_end`),流关着没人拉，
/// 点歌、停止就会把调用线程(界面线程)永远卡住。用不着声卡时同步源只吐静音、不消耗媒体，所以照
/// 实时节奏空拉是安全的。20ms 让一次 `clear` 最多多等这么久。
pub(crate) const IDLE_PULL_EVERY: Duration =
    Duration::from_millis(20);

/// 发给持流线程的信号。
pub(crate) enum Signal {
    /// 收工：关流、线程退出。
    Stop,
    /// 状态变了(按了播放、拖了进度条、换了时间线),马上看一眼要不要开流，不等下一次醒。
    Poke,
}

/// 一条活着的输出。drop 掉就关流、收线程。
pub(crate) struct Output {
    pub mixer: Mixer,
    signals: mpsc::Sender<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl Output {
    /// 叫持流线程马上看一眼：暂停久了流是关着的，要出声时别让用户多等一个醒来周期。
    pub(crate) fn poke(&self) {
        let _ = self.signals.send(Signal::Poke);
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.signals.send(Signal::Stop);
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
        signals: stop_tx,
        thread: Some(thread),
    })
}

/// 持流线程的等待循环：收到关闭就走;暂停够久就关流，要出声了再开;流断了就重开。
///
/// `first` 是已经开好的那条流，`open` 开一条新的(桌面每次重新找默认设备)。流 drop 掉就是关了。
/// `pull` 从混音器拉走这么长一段采样丢掉，见 [`IDLE_PULL_EVERY`]。
pub(crate) fn watch<S>(
    signals: &mpsc::Receiver<Signal>,
    shared: &SyncShared,
    broken: &AtomicBool,
    first: Option<S>,
    mut open: impl FnMut() -> Result<S, AudioError>,
    mut pull: impl FnMut(Duration),
) {
    let mut stream = first;
    let mut keeper = Keeper::default();
    let mut pulled_at = Instant::now();
    loop {
        let wait = if stream.is_none() && shared.idle() {
            IDLE_PULL_EVERY
        } else {
            WATCH_EVERY
        };
        match signals.recv_timeout(wait) {
            Ok(Signal::Stop)
            | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return;
            }
            Ok(Signal::Poke)
            | Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let step = keeper.step(
            Instant::now(),
            stream.is_some(),
            shared.idle(),
            broken.load(Ordering::Relaxed),
        );
        match step {
            Step::Keep => {}
            Step::Close => {
                stream = None;
                shared.output_closed();
                log::info!(
                    "暂停超过 {}s,关掉输出流",
                    CLOSE_AFTER_IDLE.as_secs()
                );
            }
            Step::Open | Step::Reopen => {
                if step == Step::Reopen {
                    log::warn!("输出流断了，重开");
                } else {
                    log::info!("要出声了，重开输出流");
                }
                // 先关旧的：同一时刻只许一条流拉混音器
                stream = None;
                broken.store(false, Ordering::Relaxed);
                match open() {
                    Ok(fresh) => stream = Some(fresh),
                    // 流留在关着：下一次醒来还要出声就再开
                    Err(error) => log::warn!(
                        "开输出流失败，稍后再试: {error}"
                    ),
                }
            }
        }
        let now = Instant::now();
        if stream.is_none() && shared.idle() {
            // 醒得晚了(系统忙)也只补一小段：拉的是静音，追上实时没有意义
            pull(
                now.duration_since(pulled_at)
                    .min(WATCH_EVERY),
            );
        }
        pulled_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    /// 在另一条线程上跑持流循环，流一开始是关着的(`None`)。返回叫停它的发送端和线程句柄。
    fn spawn_watch(
        shared: Arc<SyncShared>,
        opens: Arc<std::sync::atomic::AtomicU64>,
        pull: impl FnMut(Duration) + Send + 'static,
    ) -> (mpsc::Sender<Signal>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let broken = AtomicBool::new(false);
            watch(
                &rx,
                &shared,
                &broken,
                None::<()>,
                || {
                    opens.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
                pull,
            );
        });
        (tx, thread)
    }

    /// 流关着、暂停着：持流线程照实时节奏替声卡拉混音器，不开流。
    #[test]
    fn a_closed_idle_output_keeps_pulling_the_mixer() {
        let shared = SyncShared::new();
        shared.pause();
        let opens =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let pulled = Arc::new(Mutex::new(Duration::ZERO));
        let sink = pulled.clone();
        let (tx, thread) =
            spawn_watch(shared, opens.clone(), move |d| {
                *sink.lock().unwrap() += d;
            });
        std::thread::sleep(Duration::from_millis(300));
        let _ = tx.send(Signal::Stop);
        thread.join().unwrap();
        assert_eq!(
            opens.load(Ordering::Relaxed),
            0,
            "用不着声卡就不开流"
        );
        assert!(
            *pulled.lock().unwrap()
                >= Duration::from_millis(100),
            "300ms 里至少替声卡拉掉 100ms,实际 {:?}",
            pulled.lock().unwrap()
        );
    }

    /// 流关着、要出声了：开流，不空拉 —— 在放的时候空拉会把媒体白白吃掉。
    #[test]
    fn a_closed_output_that_is_needed_opens_instead_of_pulling()
     {
        let shared = SyncShared::new();
        let opens =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let pulled = Arc::new(Mutex::new(Duration::ZERO));
        let sink = pulled.clone();
        let (tx, thread) =
            spawn_watch(shared, opens.clone(), move |d| {
                *sink.lock().unwrap() += d;
            });
        std::thread::sleep(Duration::from_millis(300));
        let _ = tx.send(Signal::Stop);
        thread.join().unwrap();
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert_eq!(*pulled.lock().unwrap(), Duration::ZERO);
    }

    /// 回归(#138):暂停久了流关着，这时候换歌(rodio `Player::clear`)不能把调用线程卡死。
    /// 从前关着的流没人拉混音器，`clear` 等旧源被摘掉，永远等不到，界面就冻住了。
    #[test]
    fn clearing_the_player_while_the_output_is_closed_does_not_hang()
     {
        let shared = SyncShared::new();
        shared.pause();
        let (mixer, source) = rodio::mixer::mixer(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(48_000).unwrap(),
        );
        let source: SharedMixer =
            Arc::new(Mutex::new(source));
        let player = rodio::Player::connect_new(&mixer);
        player.append(crate::sync::SyncSource::new(
            crate::sync::SourceFeed(
                rodio::source::SineWave::new(440.0),
            ),
            shared.clone(),
        ));
        player.play();

        let opens =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (tx, thread) =
            spawn_watch(shared, opens, move |d| {
                let frames =
                    (d.as_secs_f64() * 48_000.0) as usize;
                fill(&source, &mut vec![0.0; frames * 2]);
            });
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            player.clear();
            let _ = done_tx.send(());
        });
        let cleared =
            done_rx.recv_timeout(Duration::from_secs(2));
        let _ = tx.send(Signal::Stop);
        thread.join().unwrap();
        assert!(cleared.is_ok(), "流关着时 clear 卡住了");
    }

    /// 在放就一直开着。
    #[test]
    fn a_playing_output_stays_open() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(
            keeper.step(t0, true, false, false),
            Step::Keep
        );
        assert_eq!(
            keeper.step(t0 + 600 * SEC, true, false, false),
            Step::Keep
        );
    }

    /// 暂停满 30 秒才关，差一点都不关。
    #[test]
    fn it_closes_only_after_being_idle_long_enough() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(
            keeper.step(t0, true, true, false),
            Step::Keep
        );
        assert_eq!(
            keeper.step(
                t0 + CLOSE_AFTER_IDLE - SEC,
                true,
                true,
                false
            ),
            Step::Keep
        );
        assert_eq!(
            keeper.step(
                t0 + CLOSE_AFTER_IDLE,
                true,
                true,
                false
            ),
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
            keeper.step(
                t0 + 21 * SEC + CLOSE_AFTER_IDLE,
                true,
                true,
                false
            ),
            Step::Close
        );
    }

    /// 关着的流：还用不着就不开，一要出声就开。
    #[test]
    fn a_closed_output_opens_as_soon_as_it_is_needed() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(
            keeper.step(t0, false, true, false),
            Step::Keep
        );
        assert_eq!(
            keeper.step(t0 + 90 * SEC, false, true, false),
            Step::Keep,
            "关着的流不会被当成要关"
        );
        assert_eq!(
            keeper.step(t0 + 91 * SEC, false, false, false),
            Step::Open
        );
    }

    /// 断了的流：还要出声就重开;已经暂停够久就干脆关掉，不去重开一条马上要关的流。
    #[test]
    fn a_broken_output_reopens_unless_it_is_due_to_close() {
        let t0 = Instant::now();
        let mut keeper = Keeper::default();
        assert_eq!(
            keeper.step(t0, true, false, true),
            Step::Reopen
        );
        assert_eq!(
            keeper.step(t0 + SEC, true, true, true),
            Step::Reopen
        );
        assert_eq!(
            keeper.step(
                t0 + SEC + CLOSE_AFTER_IDLE,
                true,
                true,
                true
            ),
            Step::Close
        );
    }
}
