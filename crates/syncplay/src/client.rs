//! 把信令与名册编排成一个能用的客户端。
//!
//! 界面开机连上([`Client::start`]),之后收名册、组的全局状态(#142)与各台的执行事实,
//! 发校时与本机的执行事实。点歌、切歌这些意图不走这里,走 HTTP(`api::group_*`)。
//! 重连、退避、版本协商,都关在这里。
//!
//! 全部跑在自己的后台 runtime 上,与 `api`、`audio` 同一个模式:调用方是 Slint 的
//! UI 线程,那里没有 tokio 反应堆,也一秒钟都不能被阻塞。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use contract::{DeviceDto, ServerSignal};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::clock::{Clock, monotonic_ns};
use crate::{Signalling, SyncError};

/// 校时的结论，编排循环写、界面读(把共同计划里的服务端时刻换算到本机)。
pub type SharedClock = Arc<Mutex<Clock>>;

/// 多久校一次时。半秒一次往返：一分钟的拟合窗口里有一百二十个样本，挑得出最短的那些;
/// 一次往返是两条几十字节的消息，服务端不必为此记任何状态。
const PING_EVERY: Duration = Duration::from_millis(500);

/// 客户端向外抛出的事件。
///
/// 回调在**后台线程**上被调用,不在 UI 线程 —— 要改界面得自己切回去。
pub enum Event {
    /// 名册变了。含本机在内,过滤交给 [`crate::Roster`]。
    Roster(Vec<DeviceDto>),
    /// 某一步失败了。给界面一行能显示的话,而不是让它停在一个永远不会变的状态上。
    Failed(String),
    /// 服务端不认这个 token(带着的就是被拒的那一个)。它仍是当前会话的话,
    /// 界面该把人送回登录页 —— 信令不会自己重试,换一个 token 之前再连也只是
    /// 再得到一个 401;已经换过就与当前会话无关(#131)。
    Unauthorized(String),
    /// 对端讲的不是同一版协议。
    ///
    /// 与 [`Self::Failed`] 分开是整条协商的意义所在:掉线等一等会自己好,
    /// 版本不对等多久都不会好,得去升级其中一端 —— 两者在界面上说同一句话
    /// 的话,用户照着那句话做不了任何事(`docs/adr/0031`)。
    ///
    /// `theirs` 为 `None` 表示对端旧到根本不报版本。
    Incompatible { ours: u32, theirs: Option<u32> },
    /// 信令连上了(每次重连都有一条)。组状态随后就到。
    Connected,
    /// 信令断了,编排循环正在重连。出声设备据此停下(#142 的掉线规则)。
    Disconnected,
    /// 组的全局播放状态(#142)。组散了是 `None`。
    GroupState(Option<Box<contract::GroupStateDto>>),
    /// 组里某台出声设备的执行事实。
    DeviceReport {
        from: String,
        report: contract::DeviceReportDto,
    },
}

/// 界面发给编排循环的指令。
enum Command {
    /// 出声设备的执行事实(#142)。
    ReportDevice(contract::DeviceReportDto),
}

/// 一个连着信令服务器的客户端。
///
/// 丢掉它,编排循环随之结束(指令通道断开),所有连接跟着关。
pub struct Client {
    commands: mpsc::UnboundedSender<Command>,
    clock: SharedClock,
    /// 对端讲的不是同一版协议。共享给编排循环:判定发生在收到下行消息的那一刻。
    incompatible: Arc<AtomicBool>,
}

impl Client {
    /// 连上信令服务器并开始编排。
    ///
    /// 立即返回:连接是在后台建的。连不上会走 [`Event::Failed`],而不是让调用方等。
    /// `token` 每次建连时现取一次,而不是启动时取一次:登录态会变 ——
    /// 开机时还没登录、中途被服务端吊销、重新登录换了一个,都要能自己接上。
    pub fn start(
        base_url: &str,
        device: DeviceDto,
        token: impl Fn() -> Option<String>
        + Send
        + Sync
        + 'static,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let base_url = base_url.to_owned();
        let events: Arc<dyn Fn(Event) + Send + Sync> =
            Arc::new(events);
        let token: Arc<
            dyn Fn() -> Option<String> + Send + Sync,
        > = Arc::new(token);

        let incompatible = Arc::new(AtomicBool::new(false));
        let clock = SharedClock::default();

        runtime().spawn(run(
            base_url,
            device,
            token,
            events,
            inbox,
            Arc::clone(&incompatible),
            Arc::clone(&clock),
        ));

        Self {
            commands,
            clock,
            incompatible,
        }
    }

    /// 一个谁也不连的客户端:通道建了,编排循环没起。
    ///
    /// 给那些需要一个 `Client` 才装得起来、却与信令毫无关系的调用方用 ——
    /// 主要是测试。所有指令方法都是 `let _ = send`,接收端一开始就没有,
    /// 于是每一个都成了空操作,不会 panic,也不会有后台任务。
    ///
    /// **不要在生产路径上用它。** 真要连的地方走 [`Self::start`];这里之所以
    /// 不是 `#[cfg(test)]`,是因为用它的测试在别的 crate 里(见
    /// `ui::sync::remote::detached`),那个属性在这里对它们不生效。
    pub fn detached() -> Self {
        let (commands, _) = mpsc::unbounded_channel();
        Self {
            commands,
            clock: SharedClock::default(),
            incompatible: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 对端讲的是不是同一版协议。界面据此把这一种失败与普通掉线分开说。
    pub fn is_incompatible(&self) -> bool {
        self.incompatible.load(Ordering::Relaxed)
    }

    /// 出声设备报本机的执行事实(#142)。断着的时候丢掉:下一秒还有一条。
    pub fn report_device(
        &self,
        report: contract::DeviceReportDto,
    ) {
        let _ = self
            .commands
            .send(Command::ReportDevice(report));
    }

    /// 校时的结论。界面拿它把计划里的服务端时刻换算成本机单调时钟。
    pub fn clock(&self) -> SharedClock {
        Arc::clone(&self.clock)
    }
}

/// 后台多线程 runtime。
///
/// 与 `api`、`audio` 各自那个同构、同理由(`docs/adr/0002`),但**必须是另一个** ——
/// 三个 crate 谁也不依赖谁。
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Runtime::new()
            .expect("failed to start tokio runtime")
    })
}

/// 第一次重连前等多久。
const RETRY_MIN: Duration = Duration::from_secs(1);

/// 重连间隔的上限。
///
/// 退避不是为了省本机那次 TCP 连接,是为了别在服务端刚倒下的时候,几十台
/// 设备一起每三秒敲一次门。上限留在一分钟:服务端回来之后,最迟一分钟接上。
const RETRY_MAX: Duration = Duration::from_secs(60);

/// 没有登录态可用时,隔多久回头看一眼。
///
/// 这一段不建连、不发包,只是问一句"登录了吗" —— 所以可以密一点。
const RELOGIN_POLL: Duration = Duration::from_secs(3);

/// 一条连接活多久才算「真的连上过」。
///
/// 只看 `Signalling::connect` 返回 `Ok` 是不够的:**服务端接完就关时那一步
/// 照样成功**。把退避按那个判据清零,这个循环就以网络往返的速度空转 ——
/// 回环上实测五秒一万九千次(#109 F-R3),而两端的日志里一行都不会有:
/// 建连成功不打日志,`serve` 返回也不打日志。
///
/// 服务端那道 `signal_connect` 限流按**账号**分桶,所以烧额度的不是自己一台:
/// 桶里 30 个、每两秒恢复一个,几秒钟就能把同账号的另一台设备锁在门外,
/// 而它看到的只是一句 429。
///
/// 会走到「接完就关」的路不止一条,而且都不是异常情况:同 id 的第二个实例
/// 把前一个顶掉、版本协商拒掉一个旧端、消息超过 `MAX_SIGNAL_BYTES` 让服务端
/// 读循环跳出、滚动发布时 pod 换人。十秒是分界:握手加入册在局域网上是几十
/// 毫秒的事,而一条正常会话以分钟计。
const HEALTHY_AFTER: Duration = Duration::from_secs(10);

/// 服务端说要等多久就等多久,但不超过这个数。
///
/// 照 `Retry-After` 等是对的 —— 还欠多少额度只有服务端算得出来。设上界是
/// 因为那个数由对端给:配置写错、或者前面那层 CDN 回一个自己的数,一个离谱的值
/// 不该让一台设备分钟级地连不上。自家服务端的桶两秒回一个额度,正常的欠账
/// 从不超过两秒,三十秒已经是它的十几倍(#118:原先十分钟)。
const MAX_THROTTLE_WAIT: Duration = Duration::from_secs(30);

/// 被限流时等多久:照服务端给的秒数,读不出来就按自己的退避,都不超过上界。
fn throttle_wait(
    retry_after: Option<Duration>,
    backoff: Duration,
) -> Duration {
    // 0 是 governor 把不到一秒的欠账 `as_secs()` 截出来的,照它等就是以网络
    // 往返的速度重敲,所以抬到退避下限。
    retry_after
        .unwrap_or(backoff)
        .clamp(RETRY_MIN, MAX_THROTTLE_WAIT)
}

/// 下一次的等待时长:翻倍,到上限为止。
fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RETRY_MAX)
}

/// 给等待时长加上 ±25% 的抖动。
///
/// 不引随机数 crate:这里只要"两台设备不会掐着同一个毫秒一起重连",
/// 挂钟的纳秒位已经足够乱。
fn jittered(base: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    // 75% ~ 125%
    let factor = 75 + u64::from(nanos % 51);
    Duration::from_millis(
        (base.as_millis() as u64).saturating_mul(factor)
            / 100,
    )
}

/// 编排循环:连上、干活、断了就重连,直到 [`Client`] 被丢掉。
///
/// **断线必须能自愈。** 服务端重启一次就让遥控永久失效,是开发时每天都会撞上的事,
/// 而症状只是状态行上一句不再变化的错误 —— 谁都看不出它其实还能救。
async fn run(
    base_url: String,
    device: DeviceDto,
    token: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    events: Arc<dyn Fn(Event) + Send + Sync>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    incompatible: Arc<AtomicBool>,
    clock: SharedClock,
) {
    let mut backoff = RETRY_MIN;
    // 上一个被服务端拒掉的 token。它没换之前不必再试 —— 结果只会一样。
    let mut rejected: Option<String> = None;

    loop {
        // 还没登录,或者手上只有那个已经被拒的 token:等它变,别空转建连。
        let Some(credential) = token().filter(|current| {
            rejected.as_deref() != Some(current.as_str())
        }) else {
            tokio::time::sleep(RELOGIN_POLL).await;
            continue;
        };

        let signalling = match Signalling::connect(
            &base_url,
            device.clone(),
            &credential,
        )
        .await
        {
            Ok(signalling) => signalling,
            Err(SyncError::Unauthorized) => {
                // 不重试:界面把人送回登录页,下一个 token 到位时上面那一句
                // 会自己把它捡起来。
                rejected = Some(credential.clone());
                events(Event::Unauthorized(credential));
                continue;
            }
            // 429:额度用完了。**照服务端给的秒数等**,而不是按自己那套
            // 退避 —— 它算得出还欠多少,我们算不出,按 1、2、4 秒撞过去
            // 只会把闸撞得更死(#109 F-R3)。这一拍不推进退避:等的长度
            // 已经由对端定了,再叠一层就是等两次。
            Err(SyncError::Throttled { retry_after }) => {
                let wait =
                    throttle_wait(retry_after, backoff);
                log::warn!(
                    "建连被限流,等 {} 秒再试",
                    wait.as_secs()
                );
                events(Event::Failed(
                    SyncError::Throttled { retry_after }
                        .to_string(),
                ));
                tokio::time::sleep(jittered(wait)).await;
                continue;
            }
            Err(error) => {
                log::warn!("信令连不上: {error}");
                events(Event::Failed(error.to_string()));
                tokio::time::sleep(jittered(backoff)).await;
                backoff = next_backoff(backoff);
                continue;
            }
        };

        // token 被收下了,下次不必跳过它。**退避不在这里清零** ——
        // 那要等这条连接证明自己活得下去,见 `HEALTHY_AFTER`。
        rejected = None;
        let connected_at = Instant::now();
        // 连上、断开各一行 info:「那台设备到底在不在线」是查遥控问题的
        // 第一问,从前客户端日志里一个字都没有(#113)。
        log::info!("信令已连上");
        events(Event::Connected);

        if !serve(
            signalling,
            &events,
            &mut commands,
            &incompatible,
            &clock,
        )
        .await
        {
            return;
        }

        log::info!(
            "信令断开(连了 {} 秒)",
            connected_at.elapsed().as_secs()
        );
        events(Event::Disconnected);

        // 活够了才算连上过一次,退避从头来。
        if connected_at.elapsed() >= HEALTHY_AFTER {
            backoff = RETRY_MIN;
        }
        // 版本对不上不会靠重试变好 —— 得有一端升级。直接退到顶,别拿
        // 同账号其他设备的建连额度去撞一堵不会开的门。
        if incompatible.load(Ordering::Relaxed) {
            backoff = RETRY_MAX;
        }
        // **断开之后一定要等**。代价是一次普通掉线要多等一秒才重连,
        // 换来的是任何一种「接完就关」都不会变成风暴。
        tokio::time::sleep(jittered(backoff)).await;
        backoff = next_backoff(backoff);
    }
}

/// 在一条已连上的信令上干活。返回是否还该重连。
///
/// `false` 意味着指令通道断了 —— [`Client`] 被丢掉了,整个客户端该收工。
async fn serve(
    mut signalling: Signalling,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    incompatible: &AtomicBool,
    clock: &SharedClock,
) -> bool {
    let sender = signalling.sender();
    // 发出去还没回的校时：id → 本机发出时刻。连接断了就作废(新连接从头校)。
    let mut pings: HashMap<u64, i64> = HashMap::new();
    let mut next_ping: u64 = 0;
    let mut ticker = tokio::time::interval(PING_EVERY);
    ticker.set_missed_tick_behavior(
        tokio::time::MissedTickBehavior::Delay,
    );
    // 这条连接上还没见过握手应答。见到 `Roster` 时它要是还立着,对端就是
    // 一个不认识版本协商的旧服务端 —— 判据是**谁先到**:新服务端在入册之前
    // 发 `Welcome`,而 `Roster` 是入册之后的第一条下行(`docs/adr/0031`)。
    let mut awaiting_welcome = true;

    loop {
        let step = tokio::select! {
            incoming = signalling.next() => {
                let Some(message) = incoming else {
                    return true;
                };
                verify_handshake(
                    &message,
                    &mut awaiting_welcome,
                    incompatible,
                    events,
                );
                if let ServerSignal::TimePong { id, server_us, epoch } = message {
                    // 收到的那一刻先读钟，再做别的:慢一步都算进往返里。
                    let received = monotonic_ns();
                    if let Some(sent) = pings.remove(&id) {
                        lock(clock).add(epoch, sent, server_us, received);
                    }
                    Ok(())
                } else {
                    accept(message, events);
                    Ok(())
                }
            }
            _ = ticker.tick() => {
                // 丢了的往返不会再回:超过两秒的一并忘掉，表不会越攒越大。
                let now = monotonic_ns();
                pings.retain(|_, sent| now - *sent < 2_000_000_000);
                next_ping += 1;
                pings.insert(next_ping, monotonic_ns());
                sender.time_ping(next_ping).await
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    return false;
                };
                match command {
                    Command::ReportDevice(report) => {
                        sender.report_device(report).await
                    }
                }
            }
        };

        // 一条信令处理失败不该终止会话:某条发不出去不影响后面的消息。报出去,接着跑。
        if let Err(error) = step {
            events(Event::Failed(error.to_string()));
        }
    }
}

/// 握手协商的客户端这一半:看这条下行是不是把版本这件事定下来了。
///
/// 两条判据,对应两种对端:
///
/// - 收到 `Welcome`:对端是新服务端,直接比版本号。
/// - 在见到 `Welcome` **之前**先收到 `Roster`:对端是旧服务端 —— 它不认识
///   版本协商,也就永远不会发那一条,而 `Roster` 是入册后的第一条下行。
///
/// 判定只做一次(`awaiting_welcome` 落下就不再抬起):重连会新起一条连接、
/// 新起一个判定,而同一条连接上 `Roster` 会来很多次。
fn verify_handshake(
    message: &ServerSignal,
    awaiting_welcome: &mut bool,
    incompatible: &AtomicBool,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
) {
    if !*awaiting_welcome {
        return;
    }
    let server_version = match message {
        ServerSignal::Welcome { protocol_version } => {
            Some(*protocol_version)
        }
        // 旧服务端:它压根不会发 Welcome,而名册已经到了。
        ServerSignal::Roster { .. } => None,
        _ => return,
    };
    *awaiting_welcome = false;

    if server_version == Some(contract::PROTOCOL_VERSION) {
        // 对端升上来了:把标志落回去,否则接管会一直被自己拒掉,
        // 而重连也会一直停在最长的那一档退避上。
        incompatible.store(false, Ordering::Relaxed);
        return;
    }
    log::warn!(
        "协议版本对不上:本机 {},对端 {}",
        contract::PROTOCOL_VERSION,
        server_version.map_or_else(
            || "太旧,不报版本".to_owned(),
            |version| version.to_string()
        )
    );
    incompatible.store(true, Ordering::Relaxed);
    events(Event::Incompatible {
        ours: contract::PROTOCOL_VERSION,
        theirs: server_version,
    });
}

/// 一条下行落到事件上。
fn accept(
    message: ServerSignal,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
) {
    match message {
        ServerSignal::Roster { devices } => {
            events(Event::Roster(devices));
        }
        ServerSignal::Error { code, message } => {
            events(Event::Failed(format!(
                "{code}: {message}"
            )));
        }
        ServerSignal::GroupState { state } => {
            events(Event::GroupState(state));
        }
        ServerSignal::DeviceReport { from, report } => {
            events(Event::DeviceReport { from, report });
        }
        // 握手应答在 `verify_handshake` 里已经读过了;校时的回话在 `serve` 里就地收下。
        ServerSignal::Welcome { .. }
        | ServerSignal::TimePong { .. } => {}
    }
}

/// 取锁。锁里只有校时样本的增删，中毒了就是别处出了大问题。
fn lock(
    clock: &SharedClock,
) -> std::sync::MutexGuard<'_, Clock> {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 退避翻倍,并停在上限上 —— 不会一路涨到几小时。
    #[test]
    fn backoff_doubles_up_to_the_ceiling() {
        assert_eq!(next_backoff(RETRY_MIN), RETRY_MIN * 2);
        assert_eq!(next_backoff(RETRY_MAX), RETRY_MAX);
        assert_eq!(
            next_backoff(RETRY_MAX / 2 + RETRY_MIN),
            RETRY_MAX,
            "越过上限要被压回上限"
        );
    }

    /// 限流时照服务端的秒数等,但不许是 0,也不许到分钟级。
    ///
    /// governor 的 `Retry-After` 是 `as_secs()` 截出来的,不到一秒就写 0 ——
    /// 照 0 等就是以网络往返的速度重敲。上界压在一分钟以内:同账号几台设备
    /// 共用一个建连桶,哪个中间件回一个离谱的数,都不该让一台设备分钟级地
    /// 连不上(#118 验收:没有任何一台落进分钟级的 429 退避)。
    #[test]
    fn throttle_wait_is_neither_zero_nor_minutes() {
        assert_eq!(
            throttle_wait(Some(Duration::ZERO), RETRY_MIN),
            RETRY_MIN,
            "0 秒要抬到退避下限"
        );
        assert_eq!(
            throttle_wait(
                Some(Duration::from_secs(2)),
                RETRY_MIN
            ),
            Duration::from_secs(2),
            "正常的数照办"
        );
        assert!(
            throttle_wait(
                Some(Duration::from_secs(600)),
                RETRY_MIN
            ) < Duration::from_secs(60),
            "离谱的数要压到一分钟以内"
        );
        assert_eq!(
            throttle_wait(None, Duration::from_secs(8)),
            Duration::from_secs(8),
            "读不出秒数就按自己的退避"
        );
    }

    /// 抖动不会把等待变成 0,也不会离原值太远。
    ///
    /// 抖成 0 的话退避就白做了:一群设备仍然会一起敲门。
    #[test]
    fn jitter_stays_within_a_quarter_of_the_base() {
        let base = Duration::from_secs(8);

        for _ in 0..50 {
            let actual = jittered(base);
            assert!(
                actual >= base.mul_f32(0.75)
                    && actual <= base.mul_f32(1.25),
                "抖动出界: {actual:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // 握手协商的客户端这一半(#109 AC-7)
    //
    // 这一半在**新客户端撞上旧服务端**时才起作用,而那一组没法靠一个
    // 进程内的新服务端演出来 —— 新服务端永远发 `Welcome`。判据只有一条:
    // 先到的是 `Roster` 就说明对端不认识版本协商。
    // -----------------------------------------------------------------

    /// 事件回调与它收下的那一摞,写成别名 —— 直接写出来会撞
    /// `clippy::type_complexity`。
    type Events = Arc<dyn Fn(Event) + Send + Sync>;
    type Seen = Arc<std::sync::Mutex<Vec<Event>>>;

    /// 收集 `verify_handshake` 报出来的事件。
    fn spy() -> (Events, Seen) {
        let seen: Seen =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let events: Events = Arc::new(move |event| {
            sink.lock().expect("锁中毒").push(event);
        });
        (events, seen)
    }

    /// 版本对得上:一声不吭。
    #[test]
    fn a_matching_welcome_says_nothing() {
        let (events, seen) = spy();
        let mut awaiting = true;
        let incompatible = AtomicBool::new(false);

        verify_handshake(
            &ServerSignal::Welcome {
                protocol_version:
                    contract::PROTOCOL_VERSION,
            },
            &mut awaiting,
            &incompatible,
            &events,
        );

        assert!(seen.lock().expect("锁中毒").is_empty());
        assert!(!incompatible.load(Ordering::Relaxed));
        assert!(!awaiting, "判定只做一次,做完就落下");
    }

    /// 对端报了个别的版本:标不兼容,并把两个号都带出去。
    ///
    /// 带号是为了界面上那句话能说清该升哪一端 —— 少了它,用户看到的
    /// 与「网络不好」一样只能干等。
    #[test]
    fn a_mismatched_welcome_reports_both_versions() {
        let (events, seen) = spy();
        let mut awaiting = true;
        let incompatible = AtomicBool::new(false);
        let theirs = contract::PROTOCOL_VERSION + 1;

        verify_handshake(
            &ServerSignal::Welcome {
                protocol_version: theirs,
            },
            &mut awaiting,
            &incompatible,
            &events,
        );

        // `Event` 不派生 `PartialEq`,只能逐条比。
        let seen = seen.lock().expect("锁中毒");
        assert!(
            matches!(
                seen.as_slice(),
                [Event::Incompatible { ours, theirs: Some(reported) }]
                    if *ours == contract::PROTOCOL_VERSION
                        && *reported == theirs
            ),
            "该报一条带两个版本号的不兼容"
        );
        assert!(incompatible.load(Ordering::Relaxed));
    }

    /// 旧服务端:它压根不发 `Welcome`,名册直接就来了。
    ///
    /// 这一组是 AC-7 里唯一「新客户端 + 旧服务端」的判据。少了它,新客户端
    /// 连上旧服务端会一切看着正常,直到第一次遥控时队列 404。
    #[test]
    fn a_roster_before_any_welcome_means_an_old_server() {
        let (events, seen) = spy();
        let mut awaiting = true;
        let incompatible = AtomicBool::new(false);

        verify_handshake(
            &ServerSignal::Roster {
                devices: Vec::new(),
            },
            &mut awaiting,
            &incompatible,
            &events,
        );

        let seen = seen.lock().expect("锁中毒");
        assert!(
            matches!(
                seen.as_slice(),
                [Event::Incompatible { ours, theirs: None }]
                    if *ours == contract::PROTOCOL_VERSION
            ),
            "旧服务端报不出版本,theirs 该是 None,实得 {} 条",
            seen.len()
        );
        assert!(incompatible.load(Ordering::Relaxed));
    }

    /// 判定只做一次:同一条连接上 `Roster` 会来很多次。
    ///
    /// 每次都判的话,一台正常连着的新客户端会在第二条名册到达时
    /// 突然自称版本不对。
    #[test]
    fn the_verdict_is_reached_only_once_per_connection() {
        let (events, seen) = spy();
        let mut awaiting = true;
        let incompatible = AtomicBool::new(false);

        verify_handshake(
            &ServerSignal::Welcome {
                protocol_version:
                    contract::PROTOCOL_VERSION,
            },
            &mut awaiting,
            &incompatible,
            &events,
        );
        verify_handshake(
            &ServerSignal::Roster {
                devices: Vec::new(),
            },
            &mut awaiting,
            &incompatible,
            &events,
        );

        assert!(
            seen.lock().expect("锁中毒").is_empty(),
            "对上之后再来名册不该翻案"
        );
    }
}
