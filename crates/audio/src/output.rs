//! 输出后端的门面：开一条声卡流，从混音器拉采样，每块把呈现时刻报给同步层。
//! 见 `output/README.md`。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
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
#[cfg(target_os = "linux")]
mod rtkit;
#[cfg(not(target_os = "android"))]
use self::cpal as backend;

/// 持流的那条线程多久醒一次，看流是不是断了要重开。
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// 反复断流时最多每四秒重开一次。
const REOPEN_BACKOFF_MAX: Duration = Duration::from_secs(4);
/// 错误日志窗口，也用作恢复初始重试预算所需的健康运行时间。
const ERROR_WINDOW: Duration = Duration::from_secs(1);

/// 持流线程的重试节奏。
#[derive(Default)]
struct ReopenBackoff {
    /// 上次尝试时刻；没有尝试过就允许立即开流。
    attempted_at: Option<Instant>,
    /// 本次尝试后至少等多久。
    delay: Duration,
}

impl ReopenBackoff {
    /// 此刻是否允许尝试开流。
    fn ready(&self, now: Instant) -> bool {
        self.attempted_at.is_none_or(|at| {
            now.duration_since(at) >= self.delay
        })
    }
    /// 记录一次尝试，失败与刚开好就断的流共用预算。
    fn attempted(&mut self, now: Instant) {
        self.delay = if self.attempted_at.is_none() {
            WATCH_EVERY
        } else {
            (self.delay * 2).min(REOPEN_BACKOFF_MAX)
        };
        self.attempted_at = Some(now);
    }
    /// 连续健康运行后恢复初始重试间隔。
    fn healthy(&mut self, now: Instant) {
        if self.attempted_at.is_some_and(|at| {
            now.duration_since(at) >= ERROR_WINDOW
        }) {
            *self = Self::default();
        }
    }
}

/// 由持流线程合并回调错误与重开失败。
struct ErrorWindow {
    /// 上次汇总的时刻和两个累计计数。
    since: Instant,
    errors: u64,
    failures: u64,
}

impl ErrorWindow {
    /// 从当前累计值开始，避免把旧错误重复报出。
    fn new(now: Instant, errors: u64) -> Self {
        Self {
            since: now,
            errors,
            failures: 0,
        }
    }
    /// 只在窗口结束时交出有变化的计数。
    fn take(
        &mut self,
        now: Instant,
        errors: u64,
        failures: u64,
    ) -> Option<(u64, u64)> {
        if now.duration_since(self.since) < ERROR_WINDOW {
            return None;
        }
        let delta = (
            errors.saturating_sub(self.errors),
            failures.saturating_sub(self.failures),
        );
        self.since = now;
        self.errors = errors;
        self.failures = failures;
        (delta != (0, 0)).then_some(delta)
    }
}

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

/// 把混音器的采样填进声卡的缓冲。锁拿不到(不该发生：同一时刻只有一条流)就填静音、返回假。
pub(crate) fn fill(
    source: &SharedMixer,
    out: &mut [f32],
) -> bool {
    match source.try_lock() {
        Ok(mut source) => {
            for sample in out.iter_mut() {
                *sample = source.next().unwrap_or(0.0);
            }
            true
        }
        Err(_) => {
            out.fill(0.0);
            false
        }
    }
}

/// 送声线程的调度策略不是实时时，该打的那行警告;是实时返回 `None`。
///
/// `policy` 是回调线程上 `sched_getscheduler` 读到的值，`None` 表示开流后一块都没拉到、
/// 没读成。普通策略的送声线程要和所有普通进程平分 CPU,满载(比如编译)时补缓冲晚一次
/// 就是一下卡顿(#192)。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn promotion_warning(
    policy: Option<i32>,
) -> Option<String> {
    // 策略值里可能带着 SCHED_RESET_ON_FORK 这一位(rtkit 升的线程都带),比之前先摘掉
    match policy.map(|p| p & !libc::SCHED_RESET_ON_FORK) {
        Some(libc::SCHED_FIFO | libc::SCHED_RR) => None,
        Some(other) => Some(format!(
            "送声线程没升上实时优先级(调度策略 {other}),CPU 占满时声音会卡;\
             查 rtkit 是否在跑"
        )),
        None => Some(
            "开流后没读到送声线程的调度策略，不知道它升没升上实时优先级".to_owned(),
        ),
    }
}

/// 等回调线程报上它的线程号(0 = 还没报),最多等 `wait`。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn wait_for_tid(
    tid: &AtomicI32,
    wait: Duration,
) -> Option<i32> {
    let deadline = Instant::now() + wait;
    loop {
        let seen = tid.load(Ordering::Relaxed);
        if seen != 0 {
            return Some(seen);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 把送声线程升实时的结局写成一行日志：`Ok` 记 info,`Err` 记警告。
///
/// `promote` 去请 rtkit,`policy` 事后查那条线程的调度策略。线程号没报上来就两样都不做。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn promotion_outcome(
    tid: Option<i32>,
    promote: impl FnOnce(i32) -> Result<(), String>,
    policy: impl FnOnce(i32) -> i32,
) -> Result<String, String> {
    let Some(tid) = tid else {
        return Err(
            promotion_warning(None).unwrap_or_default()
        );
    };
    promote(tid).map_err(|reason| {
        format!("送声线程没升上实时优先级({reason}),CPU 占满时声音会卡")
    })?;
    let policy = policy(tid);
    match promotion_warning((policy >= 0).then_some(policy))
    {
        Some(warning) => Err(warning),
        None => Ok("送声线程已是实时优先级".to_owned()),
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

/// 持流线程的等待循环：收到关闭就走;暂停够久就关流，要出声了再开;流断了就重开;
/// 每次醒来顺手把同步源攒下的诊断写进日志。
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
    let mut logged_underruns = shared.report().underruns;
    let mut backoff = ReopenBackoff::default();
    let mut error_window =
        ErrorWindow::new(Instant::now(), 0);
    let mut open_failures = 0;
    let mut last_open_error = None;
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
        // 回调线程不写日志(#192),它攒下的诊断与欠载由这里替它写
        for line in shared.take_notes() {
            log::warn!("{line}");
        }
        let underruns = shared.report().underruns;
        if underruns > logged_underruns {
            log::warn!(
                "声卡欠载 {} 次(累计 {underruns})",
                underruns - logged_underruns
            );
            logged_underruns = underruns;
        }
        let now = Instant::now();
        if let Some((errors, failures)) = error_window.take(
            now,
            shared.report().other_errors,
            open_failures,
        ) {
            log::warn!(
                "近 {}s 输出出错 {errors} 次，重开失败 {failures} 次，最近开流错误: {}",
                ERROR_WINDOW.as_secs(),
                last_open_error.as_deref().unwrap_or("无")
            );
        }
        if stream.is_some()
            && !broken.load(Ordering::Relaxed)
        {
            backoff.healthy(now);
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
                // 先关旧的：同一时刻只许一条流拉混音器
                stream = None;
                // 清掉失效的呈现配对，组时间线继续沿用原锚点。
                shared.output_closed();
                if !backoff.ready(now) {
                    continue;
                }
                backoff.attempted(now);
                broken.store(false, Ordering::Relaxed);
                match open() {
                    Ok(fresh) => stream = Some(fresh),
                    // 流留在关着：还要出声就按退避期限再开
                    Err(error) => {
                        open_failures += 1;
                        last_open_error =
                            Some(error.to_string());
                    }
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
    use similar_asserts::assert_eq;

    std::thread_local! {
        /// 每条测试线程只收自己的日志，避免并行测试串扰。
        static CAPTURED: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
    }

    /// 只收本线程明确启用的日志，其余测试不受影响。
    struct TestLogger;
    impl log::Log for TestLogger {
        /// 级别由 log 的全局过滤器判断。
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        /// 只格式化当前测试主动收集的记录。
        fn log(&self, record: &log::Record<'_>) {
            CAPTURED.with(|capture| {
                if let Some(lines) =
                    capture.borrow_mut().as_mut()
                {
                    lines.push(record.args().to_string());
                }
            });
        }
        /// 内存记录没有待刷出的缓冲。
        fn flush(&self) {}
    }

    const SEC: Duration = Duration::from_secs(1);

    /// 真 watch 持续开流失败时也只按窗写日志，Stop 不会被退避阻塞。
    #[test]
    fn persistent_open_failures_are_summarized_and_stop_remains_responsive()
     {
        static LOGGER: TestLogger = TestLogger;
        static INIT: std::sync::Once =
            std::sync::Once::new();
        INIT.call_once(|| {
            log::set_logger(&LOGGER)
                .expect("install test logger");
            log::set_max_level(log::LevelFilter::Warn);
        });
        CAPTURED.with(|capture| {
            *capture.borrow_mut() = Some(Vec::new())
        });
        let shared = SyncShared::new();
        let (tx, rx) = mpsc::channel();
        tx.send(Signal::Poke).expect("poke");
        let stop = std::thread::spawn(move || {
            // 用真实等待验证 watch 的通道和计时集成；窗口边界由上面的可控时钟测试覆盖。
            std::thread::sleep(Duration::from_millis(1300));
            tx.send(Signal::Stop)
                .expect("watch still running");
        });
        let mut attempts = 0;
        let start = Instant::now();
        watch(
            &rx,
            &shared,
            &AtomicBool::new(false),
            None::<()>,
            || {
                attempts += 1;
                Err(AudioError::Device(
                    "test device unavailable".into(),
                ))
            },
            |_| {
                panic!(
                    "active playback must not discard PCM"
                )
            },
        );
        stop.join().expect("stop thread");
        let lines = CAPTURED.with(|capture| {
            capture.borrow_mut().take().expect("capture")
        });
        assert!(
            attempts >= 2,
            "failure path was not repeated"
        );
        assert!(
            !lines.is_empty(),
            "failure summary missing"
        );
        assert!(
            lines.len() < attempts,
            "each failure still logs: {lines:?}"
        );
        assert!(
            lines.len() as u64 <= start.elapsed().as_secs(),
            "more than one summary per window: {lines:?}"
        );
        assert!(
            lines.iter().all(|line| line
                .contains("test device unavailable")),
            "last device failure lost: {lines:?}"
        );
    }

    /// 重开风暴按指数退避，Poke 也不能绕过期限，健康一秒后复位。
    #[test]
    fn repeated_reopens_back_off_and_a_healthy_stream_resets_the_budget()
     {
        let mut backoff = ReopenBackoff::default();
        let mut now = Instant::now();
        for millis in
            [250, 500, 1000, 2000, 4000, 4000, 4000]
        {
            assert!(backoff.ready(now));
            backoff.attempted(now);
            assert!(!backoff.ready(
                now + Duration::from_millis(millis - 1)
            ));
            now += Duration::from_millis(millis);
            assert!(backoff.ready(now));
        }
        backoff.healthy(now);
        backoff.attempted(now);
        assert!(
            backoff.ready(now + Duration::from_millis(250))
        );
    }

    /// 百万次回调错误和持续开流失败按窗汇总，空窗不产生日志。
    #[test]
    fn error_and_reopen_failure_logs_grow_with_windows_instead_of_callbacks()
     {
        let now = Instant::now();
        let mut window = ErrorWindow::new(now, 0);
        for count in 1..=1_000_000 {
            assert_eq!(
                window.take(
                    now + Duration::from_millis(500),
                    count,
                    count
                ),
                None
            );
        }
        assert_eq!(
            window.take(now + SEC, 1_000_000, 1_000_000),
            Some((1_000_000, 1_000_000))
        );
        assert_eq!(
            window.take(
                now + 2 * SEC,
                1_000_000,
                1_000_000
            ),
            None
        );
        assert_eq!(
            window.take(
                now + 3 * SEC,
                1_000_001,
                1_000_001
            ),
            Some((1, 1))
        );
        assert_eq!(
            window.take(
                now + 3 * SEC,
                1_000_001,
                1_000_001
            ),
            None
        );
    }

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

    /// 送声线程升上了实时(FIFO/RR):不吭声。
    #[test]
    fn a_realtime_audio_thread_needs_no_warning() {
        assert_eq!(
            promotion_warning(Some(libc::SCHED_FIFO)),
            None
        );
        assert_eq!(
            promotion_warning(Some(libc::SCHED_RR)),
            None
        );
        // rtkit 升的线程带着 SCHED_RESET_ON_FORK 这一位
        assert_eq!(
            promotion_warning(Some(
                libc::SCHED_RR | libc::SCHED_RESET_ON_FORK
            )),
            None
        );
    }

    /// 没升上去(rtkit 拒了、没装)或者一块都没拉到：警告，说清楚满载时会卡。
    #[test]
    fn an_ordinary_or_unknown_audio_thread_is_warned_about()
    {
        let ordinary =
            promotion_warning(Some(libc::SCHED_OTHER))
                .expect("普通策略要警告");
        assert!(ordinary.contains("实时"), "{ordinary}");
        assert!(promotion_warning(None).is_some());
    }

    /// 回调报了线程号就拿到;等满了还没报，给 `None`。
    #[test]
    fn the_callback_thread_id_is_awaited_up_to_a_limit() {
        let tid = AtomicI32::new(0);
        assert_eq!(
            wait_for_tid(&tid, Duration::from_millis(30)),
            None
        );
        tid.store(42, Ordering::Relaxed);
        assert_eq!(
            wait_for_tid(&tid, Duration::ZERO),
            Some(42)
        );
    }

    /// 线程号、升级结果与之后查到的策略，合起来决定打哪一行;任何一步不成都是警告，不出错。
    #[test]
    fn the_promotion_outcome_names_what_went_wrong() {
        let rr = |_| libc::SCHED_RR;
        let ok = |_| Ok(());
        assert!(promotion_outcome(Some(7), ok, rr).is_ok());

        let no_tid =
            promotion_outcome(None, ok, rr).unwrap_err();
        assert!(no_tid.contains("没读到"), "{no_tid}");

        let refused = promotion_outcome(
            Some(7),
            |_| Err("rtkit 拒绝".to_owned()),
            rr,
        )
        .unwrap_err();
        assert!(
            refused.contains("rtkit 拒绝"),
            "{refused}"
        );

        let still_other =
            promotion_outcome(Some(7), ok, |_| {
                libc::SCHED_OTHER
            })
            .unwrap_err();
        assert!(
            still_other.contains("调度策略 0"),
            "{still_other}"
        );
    }
}
