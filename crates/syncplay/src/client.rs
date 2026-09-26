//! 把信令与名册编排成一个能用的遥控客户端(`docs/adr/0030`)。
//!
//! 界面开机连上([`Client::start`]),之后只发遥控的几个动作:接管、命令、上报、
//! 要快照。重连、退避、断线时该清的状态、版本协商,都关在这里。
//!
//! 全部跑在自己的后台 runtime 上,与 `api`、`audio` 同一个模式:调用方是 Slint 的
//! UI 线程,那里没有 tokio 反应堆,也一秒钟都不能被阻塞。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use contract::{
    DeviceDto, GroupPlanDto, RemoteCommand, RemoteStateDto,
    ServerSignal,
};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::clock::{Clock, monotonic_ns};
use crate::signalling::SignalSender;
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
    /// 本机拿到了 `target` 的控制权。界面该把输出设备切过去,并要一次快照。
    ControlGranted { target: String, generation: u64 },
    /// 本机的控制权没了 —— 被别的遥控器顶掉,或者被控端自己退出了。
    ControlRevoked { by: String },
    /// 本机被这台设备接管了:进锁定态,挂「正被 xx 遥控」。
    ControlledBy { device: DeviceDto },
    /// 本机其实没有被谁遥控 —— 服务端槽位上查不到。界面该解锁、撤横幅。
    NotControlled,
    /// 遥控器发来一条命令。本机此刻是被控端。
    Command { cmd: RemoteCommand },
    /// 被控端报来的状态。本机此刻是遥控器。
    ///
    /// 装箱的理由同 `contract::ServerSignal::State`:它比别的变体大出两百多字节,
    /// 不装箱的话每条事件都按它占位。
    RemoteState {
        from: String,
        state: Box<RemoteStateDto>,
    },
    /// 遥控器要一次完整状态,立刻回一条 [`Client::report`]。
    SnapshotRequest,
    /// 对端讲的不是同一版协议。
    ///
    /// 与 [`Self::Failed`] 分开是整条协商的意义所在:掉线等一等会自己好,
    /// 版本不对等多久都不会好,得去升级其中一端 —— 两者在界面上说同一句话
    /// 的话,用户照着那句话做不了任何事(`docs/adr/0031`)。
    ///
    /// `theirs` 为 `None` 表示对端旧到根本不报版本。
    Incompatible { ours: u32, theirs: Option<u32> },
    /// 信令断了,编排循环正在重连。
    ///
    /// 断着的时候服务端的消息过不来,所以靠消息才清的本地状态得在这一刻自己清:
    /// 「正被遥控」的锁留着的话,本机在断网期间连歌都点不了(#118)。
    Disconnected,
    /// 接管 `target` 没成:服务端拒了,或者答复回来之前信令就断了。
    ///
    /// 界面按下去时已经把输出乐观地切了过去,这一条让它切回本机 —— 那台设备
    /// 一条上报都不会发来,过期与失联的判定于是永远不触发(#118)。
    ClaimFailed { target: String, reason: String },
    /// 服务端登记下了一次换输出(`BeginOutputs`),带回本机的控制代次。
    OutputsBegun {
        operation_id: String,
        generation: u64,
    },
    /// 服务端确认输出集合换好了,带回新的主端任期。
    OutputsCommitted { operation_id: String, term: u64 },
    /// 换输出没登记上:输出不在线、或者就是本机、或者信令在答复之前断了。
    OutputsFailed {
        operation_id: String,
        reason: String,
    },
    /// 服务端通告的播放组(#137 ⑤):任期、主端、成员。本机在不在成员里、是不是主端，
    /// 由收的那一侧自己看。
    Group {
        term: u64,
        master: Option<String>,
        members: Vec<String>,
    },
    /// 信令连上了(每次重连都有一条)。组状态随后就到。
    Connected,
    /// 组的全局播放状态(#142)。组散了是 `None`。
    GroupState(Option<Box<contract::GroupStateDto>>),
    /// 组里某台出声设备的执行事实。
    DeviceReport {
        from: String,
        report: contract::DeviceReportDto,
    },
    /// 主端发来的共同计划。
    GroupPlan {
        from: String,
        term: u64,
        plan: Box<GroupPlanDto>,
    },
}

/// 界面发给编排循环的指令。
enum Command {
    /// 接管这台设备(用户主动按下的那一次)。
    Claim(String),
    /// 本机不再遥控谁了。只忘掉本地那份持权记录,**不发信令** ——
    /// 遥控器一走了之,被控端仍然该接着放(手机没电不能让 pc1 停)。
    ReleaseControl,
    /// 被控端退出被遥控。
    ExitControlled,
    /// 把一条命令发给当前持权的那台设备。
    Send(RemoteCommand),
    /// 把本机的状态报给正在遥控本机的那台设备。
    Report(Box<RemoteStateDto>),
    /// 出声设备的执行事实(#142)。
    ReportDevice(contract::DeviceReportDto),
    /// 向当前持权的那台设备要一次快照。
    Snapshot,
    /// 把一条命令发给指定的设备。迁移时要同时叫得动源与目标,不能只认持权那一台。
    SendTo {
        to: String,
        cmd: RemoteCommand,
    },
    /// 开始一次换输出。
    BeginOutputs {
        operation_id: String,
        outputs: Vec<String>,
        master: Option<String>,
    },
    CommitOutputs(String, Option<Vec<String>>),
    AbortOutputs(String),
    /// 主端发布共同计划。
    PublishPlan {
        term: u64,
        plan: Box<GroupPlanDto>,
    },
}

/// 本机作为**遥控器**持有的那份控制权。
///
/// 活在 [`run`] 的作用域里而不是 [`serve`] 里 —— 它要跨重连活下来。
struct Held {
    /// 重连续权时报的那台 —— 组里的成员(迁移途中也可以是正被拉进来的那台)。
    target: String,
    /// 服务端给的代次。还没拿到就是 `None`(刚发出去、答复没回来)。
    generation: Option<u64>,
    /// 进行中的那一次换输出:操作号与换上之后的集合。提交时据此换 `target`。
    changing: Option<(String, Vec<String>)>,
}

/// 一个连着信令服务器的遥控客户端。
///
/// 丢掉它,编排循环随之结束(指令通道断开),所有连接跟着关。
pub struct Client {
    commands: mpsc::UnboundedSender<Command>,
    clock: SharedClock,
    /// 对端讲的不是同一版协议。置上之后,[`Self::claim`] 一律空转。
    ///
    /// 共享给编排循环:判定发生在收到下行消息的那一刻,而挡住接管要在
    /// 调用方那一刻(`docs/adr/0031`)。
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

    /// 接管这台设备:本机当它的遥控器。
    ///
    /// 这是用户**主动**按下的那一次,顶掉当前的遥控器。重连之后的自动重发
    /// 由编排循环自己做,走的是另一条路(见 [`Held`])。
    pub fn claim(&self, target: &str) {
        // 对端讲的不是同一版协议就不接管 —— **拒绝要落在取得控制权之前**
        // (`docs/adr/0031`)。新服务端会在入册前把旧客户端挡下,但反过来
        // 不成立:旧服务端根本不认识版本这回事,照样让新客户端接管,
        // 然后两边对着一堆解不出来的 JSON 静默丢弃,症状是「按了没反应」。
        if self.incompatible.load(Ordering::Relaxed) {
            log::warn!(
                "不接管 {target}:对端协议版本对不上"
            );
            return;
        }
        let _ = self
            .commands
            .send(Command::Claim(target.to_owned()));
    }

    /// 对端讲的是不是同一版协议。界面据此把这一种失败与普通掉线分开说。
    pub fn is_incompatible(&self) -> bool {
        self.incompatible.load(Ordering::Relaxed)
    }

    /// 输出设备选回本机:忘掉持权记录,不知会任何人。
    pub fn release_control(&self) {
        let _ = self.commands.send(Command::ReleaseControl);
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

    /// 被控端按了「退出被遥控」。
    pub fn exit_controlled(&self) {
        let _ = self.commands.send(Command::ExitControlled);
    }

    /// 把一条命令发给正在被本机遥控的那台设备。没有持权就地丢掉 ——
    /// 界面那时本就不该让人按下去。
    pub fn command(&self, cmd: RemoteCommand) {
        // 入队成不成要说出来。这条通道在编排循环收工之后就关了,而丢在这里的
        // 命令与「发出去但对面没收到」在界面上长得一模一样 —— 都是按了没反应。
        let summary = cmd.summary();
        match self.commands.send(Command::Send(cmd)) {
            Ok(()) => {
                log::info!("遥控命令入发送队列: {summary}")
            }
            Err(_) => log::warn!(
                "遥控命令没能入队: {summary}(发送通道已关)"
            ),
        }
    }

    /// 把本机的播放状态报给正在遥控本机的那台设备。
    ///
    /// 发去哪里由服务端从控制权槽位查:让被控端自己写目标的话,
    /// 它能把自己的播放位置每秒推给任何一台设备。
    pub fn report(&self, state: RemoteStateDto) {
        let _ = self
            .commands
            .send(Command::Report(Box::new(state)));
    }

    /// 向被控端要一次完整状态。
    ///
    /// 取得控制权、换目标、重连之后各要一次 —— 服务端不缓存状态
    /// (`docs/adr/0030`),「现在是什么样」只能问被控端本人。
    /// 把一条命令发给指定的设备(迁移时的源或目标)。服务端只转给遥控器所在
    /// 组里的设备,不在组里的一律回错。
    pub fn command_to(&self, to: &str, cmd: RemoteCommand) {
        let summary = cmd.summary();
        match self.commands.send(Command::SendTo {
            to: to.to_owned(),
            cmd,
        }) {
            Ok(()) => log::info!(
                "遥控命令入发送队列: {summary} -> {to}"
            ),
            Err(_) => log::warn!(
                "遥控命令没能入队: {summary} -> {to}(发送通道已关)"
            ),
        }
    }

    /// 开始一次换输出:`outputs` 是换上之后的输出集合,空集合是改回本机;`master` 是换过去
    /// 之后的主端。
    pub fn begin_outputs(
        &self,
        operation_id: &str,
        outputs: Vec<String>,
        master: Option<String>,
    ) {
        if self.incompatible.load(Ordering::Relaxed) {
            log::warn!(
                "不换输出(操作 {operation_id}):对端协议版本对不上"
            );
            return;
        }
        let _ = self.commands.send(Command::BeginOutputs {
            operation_id: operation_id.to_owned(),
            outputs,
            master,
        });
    }

    /// 提交那一次换输出。`outputs` 是真正跟上的那几台,`None` 是登记的整份。
    pub fn commit_outputs(
        &self,
        operation_id: &str,
        outputs: Option<Vec<String>>,
    ) {
        let _ = self.commands.send(Command::CommitOutputs(
            operation_id.to_owned(),
            outputs,
        ));
    }

    /// 主端发布一份共同计划(#137 ⑤)。
    pub fn publish_plan(
        &self,
        term: u64,
        plan: GroupPlanDto,
    ) {
        let _ = self.commands.send(Command::PublishPlan {
            term,
            plan: Box::new(plan),
        });
    }

    /// 校时的结论。界面拿它把计划里的服务端时刻换算成本机单调时钟。
    pub fn clock(&self) -> SharedClock {
        Arc::clone(&self.clock)
    }

    /// 放弃那一次换输出。
    pub fn abort_outputs(&self, operation_id: &str) {
        let _ = self.commands.send(Command::AbortOutputs(
            operation_id.to_owned(),
        ));
    }

    pub fn request_snapshot(&self) {
        let _ = self.commands.send(Command::Snapshot);
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

/// 断线时还没等到答复的那次接管:忘掉它,返回它的目标。
///
/// 没拿到代次的权重连时不续(见 [`resume_claim`]),留着只会让界面的输出永远
/// 指着那台设备。拿到过代次的不动 —— 那份要跨重连去续。
fn abandon_pending(
    held: &mut Option<Held>,
) -> Option<String> {
    if held.as_ref()?.generation.is_some() {
        return None;
    }
    held.take().map(|pending| pending.target)
}

/// 进行中的那一次换输出被拒了(或者答复之前信令断了):忘掉它,返回它的操作号。
///
/// 这一次之前就持着权的,权留着 —— 只是这一次没成;这一次才开始持权的
/// (从本机第一次换到别的设备),连持权记录一起忘掉。
fn abandon_change(
    held: &mut Option<Held>,
) -> Option<String> {
    let current = held.as_mut()?;
    let (operation_id, _) = current.changing.take()?;
    if current.generation.is_none() {
        *held = None;
    }
    Some(operation_id)
}

/// 服务端对 `ClaimControl` 的拒绝码(见 `server::syncplay::control` 的 `claim`)。
///
/// 报错不带是哪条请求引起的,所以认法是「手上有一次还没答复的接管,又来了
/// 一条接管才会回的码」。`device_offline` 转发命令时也会回,但那时持权早就
/// 拿到代次了,不会被当成接管失败。
const CLAIM_REJECTIONS: &[&str] =
    &["device_offline", "cannot_control_self"];

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
    // 本机遥控着谁。**跨重连保留** —— 断线不该让用户重新挑一次设备
    // (`docs/adr/0030`):重连的是信令,不是遥控关系。
    let mut held: Option<Held> = None;
    // 本机断线那一刻是不是正被遥控。同样跨重连保留,理由见 `serve` 开头。
    let mut controlled = false;

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
            &mut held,
            &mut controlled,
            &incompatible,
            (&device.id, &clock),
        )
        .await
        {
            return;
        }

        // 断着的时候服务端的消息过不来,靠消息才清的状态得在这里自己清。
        if let Some(operation_id) =
            abandon_change(&mut held)
        {
            events(Event::OutputsFailed {
                operation_id,
                reason: "信令断开,换输出没有等到答复"
                    .to_owned(),
            });
        }
        if let Some(target) = abandon_pending(&mut held) {
            events(Event::ClaimFailed {
                target,
                reason: "信令断开,接管没有等到答复"
                    .to_owned(),
            });
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
    held: &mut Option<Held>,
    controlled: &mut bool,
    incompatible: &AtomicBool,
    (me, clock): (&str, &SharedClock),
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

    // 重连之后**先确认还持不持权**,再由界面去要快照(`docs/adr/0030`)。
    // 带着手上那个代次:槽位已经换人时服务端只会回一条撤权,而不是让这台
    // 刚恢复网络的设备把接管者顶掉(产品规则:旧遥控器自动重连不夺回)。
    if let Some((target, generation)) =
        resume_claim(held.as_ref())
    {
        let _ =
            sender.claim(&target, Some(generation)).await;
    }
    // 断线前正被遥控:回来之后先退出。断线时界面已经解了锁(见
    // `Event::Disconnected`),断网期间本机可能已经在放别的歌;而服务端未必发现
    // 过旧连接死了 —— 重连顶替掉它时槽位原样留着。不退的话遥控器接着往一台
    // 不再听它的设备发命令,两端对「谁在遥控谁」各执一词(#118)。
    if core::mem::take(controlled) {
        let _ = sender.exit_controlled().await;
    }

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
                    accept(message, events, held, controlled)
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
                if matches!(command, Command::ExitControlled) {
                    *controlled = false;
                }
                dispatch(command, &sender, held, me).await
            }
        };

        // 一条信令处理失败不该终止会话:服务端回一条错误、某条命令发不出去,
        // 都不影响后面的消息。报出去,接着跑。
        if let Err(error) = step {
            events(Event::Failed(error.to_string()));
        }
    }
}

/// 重连时该不该重申控制权,以及带哪个代次。
///
/// 只有拿到过代次才续。代次是 `None` 意味着 `ControlGranted` 没回来过 ——
/// 这一份控制权服务端从没确认过。带着 `resume: None` 去 claim 的话,服务端
/// 会把它当成用户**主动**按下的接管(见 `server::syncplay::control` 里 `revoked` 那段
/// 过滤),于是一台刚恢复网络的设备会静默夺回一台它可能早就失去的设备,
/// 而接管者那边什么都没做过 —— 产品规则正好相反:旧遥控器自动重连不夺回
/// (#102 F-005)。
fn resume_claim(
    held: Option<&Held>,
) -> Option<(String, u64)> {
    let held = held?;
    Some((held.target.clone(), held.generation?))
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

/// 处理一条服务端来信。
fn accept(
    message: ServerSignal,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
    held: &mut Option<Held>,
    controlled: &mut bool,
) -> Result<(), SyncError> {
    match message {
        ServerSignal::Roster { devices } => {
            events(Event::Roster(devices));
            Ok(())
        }
        // 握手应答在 `verify_handshake` 里已经读过了,到这里没有别的事要做。
        ServerSignal::Welcome { .. } => Ok(()),
        ServerSignal::Error { code, message } => {
            let reason = format!("{code}: {message}");
            if CLAIM_REJECTIONS.contains(&code.as_str())
                && let Some(operation_id) =
                    abandon_change(held)
            {
                events(Event::OutputsFailed {
                    operation_id,
                    reason,
                });
                return Ok(());
            }
            if CLAIM_REJECTIONS.contains(&code.as_str())
                && let Some(target) = abandon_pending(held)
            {
                events(Event::ClaimFailed {
                    target,
                    reason,
                });
                return Ok(());
            }
            Err(SyncError::Signalling(reason))
        }
        ServerSignal::ControlGranted { generation } => {
            // 拿到代次才算真的持权。重连时要拿它去续,所以记下来。
            let Some(current) = held.as_mut() else {
                return Ok(());
            };
            current.generation = Some(generation);
            events(Event::ControlGranted {
                target: current.target.clone(),
                generation,
            });
            Ok(())
        }
        ServerSignal::ControlRevoked { by } => {
            // 失权就把记录清掉,否则重连时还会去续一份已经不存在的权。
            *held = None;
            events(Event::ControlRevoked { by });
            Ok(())
        }
        ServerSignal::Command { cmd } => {
            events(Event::Command { cmd });
            Ok(())
        }
        ServerSignal::State { from, state } => {
            events(Event::RemoteState { from, state });
            Ok(())
        }
        ServerSignal::SnapshotRequest => {
            events(Event::SnapshotRequest);
            Ok(())
        }
        ServerSignal::ControlledBy { device } => {
            *controlled = true;
            events(Event::ControlledBy { device });
            Ok(())
        }
        ServerSignal::NotControlled => {
            *controlled = false;
            events(Event::NotControlled);
            Ok(())
        }
        ServerSignal::OutputsBegun {
            operation_id,
            generation,
        } => {
            if let Some(current) = held.as_mut() {
                current.generation = Some(generation);
            }
            events(Event::OutputsBegun {
                operation_id,
                generation,
            });
            Ok(())
        }
        ServerSignal::OutputsCommitted {
            operation_id,
            term,
        } => {
            settle_change(held, &operation_id);
            events(Event::OutputsCommitted {
                operation_id,
                term,
            });
            Ok(())
        }
        // 校时的回话在 `serve` 里就地收下(要读收到那一刻的钟),到不了这里。
        ServerSignal::TimePong { .. } => Ok(()),
        ServerSignal::Group {
            term,
            master,
            members,
        } => {
            events(Event::Group {
                term,
                master,
                members,
            });
            Ok(())
        }
        ServerSignal::GroupPlan { from, term, plan } => {
            events(Event::GroupPlan { from, term, plan });
            Ok(())
        }
        ServerSignal::GroupState { state } => {
            events(Event::GroupState(state));
            Ok(())
        }
        ServerSignal::DeviceReport { from, report } => {
            events(Event::DeviceReport { from, report });
            Ok(())
        }
    }
}

/// 持权记录该认组里哪一台：命令发给主端，所以主端排第一;本机自己不算(本机是主端时
/// 命令走本机那条路，续权要认一台远端成员)。
fn remote_first(
    outputs: &[String],
    master: Option<&str>,
    me: &str,
) -> Vec<String> {
    let mut remote: Vec<String> = outputs
        .iter()
        .filter(|id| *id != me)
        .cloned()
        .collect();
    if let Some(at) = remote
        .iter()
        .position(|id| Some(id.as_str()) == master)
    {
        let master = remote.remove(at);
        remote.insert(0, master);
    }
    remote
}

/// 取锁。锁里只有校时样本的增删，中毒了就是别处出了大问题。
fn lock(
    clock: &SharedClock,
) -> std::sync::MutexGuard<'_, Clock> {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 那一次换输出提交了:持权记录跟着换到新集合;新集合是空的(改回本机)就
/// 什么都不再持。
fn settle_change(
    held: &mut Option<Held>,
    operation_id: &str,
) {
    let Some(current) = held.as_mut() else {
        return;
    };
    let Some((_, outputs)) = current
        .changing
        .take_if(|(pending, _)| pending == operation_id)
    else {
        return;
    };
    match outputs.into_iter().next() {
        Some(first) => current.target = first,
        None => *held = None,
    }
}

/// 处理一条界面指令。
async fn dispatch(
    command: Command,
    sender: &SignalSender,
    held: &mut Option<Held>,
    me: &str,
) -> Result<(), SyncError> {
    match command {
        // 主动接管:先记下目标,代次等服务端的 ControlGranted 回来再填。
        // 先记是必须的 —— 答复到达时要靠它认出这份权是谁的。
        Command::Claim(target) => {
            *held = Some(Held {
                target: target.clone(),
                generation: None,
                changing: None,
            });
            sender.claim(&target, None).await
        }
        Command::ReleaseControl => {
            *held = None;
            Ok(())
        }
        Command::ExitControlled => {
            sender.exit_controlled().await
        }
        // 没持权就地丢掉:界面那时本就不该让人按下去,而往服务端发一条
        // 必然被拒的命令只会换回一条没人看的报错。
        Command::Send(cmd) => match held.as_ref() {
            Some(current) => {
                sender.command(&current.target, cmd).await
            }
            None => Ok(()),
        },
        Command::Report(state) => {
            sender.report(*state).await
        }
        Command::ReportDevice(report) => {
            sender.report_device(report).await
        }
        Command::Snapshot => match held.as_ref() {
            Some(current) => {
                sender.snapshot(&current.target).await
            }
            None => Ok(()),
        },
        Command::SendTo { to, cmd } => {
            sender.command(&to, cmd).await
        }
        Command::BeginOutputs {
            operation_id,
            outputs,
            master,
        } => {
            let remote = remote_first(
                &outputs,
                master.as_deref(),
                me,
            );
            match held.as_mut() {
                Some(current) => {
                    current.changing = Some((
                        operation_id.clone(),
                        remote,
                    ));
                }
                None => {
                    if let Some(first) = remote.first() {
                        *held = Some(Held {
                            target: first.clone(),
                            generation: None,
                            changing: Some((
                                operation_id.clone(),
                                remote.clone(),
                            )),
                        });
                    }
                }
            }
            sender
                .begin_outputs(
                    &operation_id,
                    outputs,
                    master,
                )
                .await
        }
        Command::CommitOutputs(operation_id, outputs) => {
            // 真正跟上的那一份比登记的少：持权记录按它换。
            if let (Some(current), Some(kept)) =
                (held.as_mut(), outputs.as_ref())
                && let Some((pending, remote)) =
                    current.changing.as_mut()
                && *pending == operation_id
            {
                remote.retain(|id| kept.contains(id));
            }
            sender
                .commit_outputs(&operation_id, outputs)
                .await
        }
        Command::PublishPlan { term, plan } => {
            sender.publish_plan(term, *plan).await
        }
        Command::AbortOutputs(operation_id) => {
            // 作罢之后这一次就不再算进行中:之前就持着权的留着权,
            // 这一次才开始持权的连记录一起忘掉。
            abandon_change(held);
            sender.abort_outputs(&operation_id).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// 持权认主端(命令发给它);本机自己是主端时认一台远端成员，重连续权才有对象。
    #[test]
    fn the_held_target_is_the_remote_master_or_another_remote_member()
     {
        assert_eq!(
            remote_first(
                &owned(&["pc", "pad"]),
                Some("pad"),
                "phone"
            ),
            owned(&["pad", "pc"])
        );
        assert_eq!(
            remote_first(
                &owned(&["phone", "pc"]),
                Some("phone"),
                "phone"
            ),
            owned(&["pc"])
        );
        assert!(
            remote_first(&[], None, "phone").is_empty()
        );
    }

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

    /// 没拿到过代次就不重申控制权。
    ///
    /// 带 `resume: None` 去 claim 会被服务端当成用户主动接管,于是一台刚恢复
    /// 网络的旧遥控器把接管者顶掉 —— 而那边什么都没做过(#102 F-005)。
    #[test]
    fn a_claim_without_a_generation_is_not_resumed() {
        assert_eq!(resume_claim(None), None);
        assert_eq!(
            resume_claim(Some(&Held {
                target: "pc".to_owned(),
                generation: None,
                changing: None,
            })),
            None,
            "服务端从没确认过这一份权,重连时不许拿它去夺回"
        );
        assert_eq!(
            resume_claim(Some(&Held {
                target: "pc".to_owned(),
                generation: Some(7),
                changing: None,
            })),
            Some(("pc".to_owned(), 7)),
            "确认过的才续,并且带上代次"
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

    /// 断线时答复还没回来的那次接管算失败;已经确认过的留着去续。
    ///
    /// 没拿到代次的那份权重连时不续(见 `resume_claim`),留着它只会让界面的
    /// 输出永远指着那台设备 —— 一条上报都不会来。
    #[test]
    fn a_pending_claim_is_abandoned_when_the_link_drops() {
        let mut pending = Some(Held {
            target: "pc".to_owned(),
            generation: None,
            changing: None,
        });
        assert_eq!(
            abandon_pending(&mut pending),
            Some("pc".to_owned())
        );
        assert!(pending.is_none(), "放弃了就要忘掉");

        let mut granted = Some(Held {
            target: "pc".to_owned(),
            generation: Some(3),
            changing: None,
        });
        assert_eq!(abandon_pending(&mut granted), None);
        assert!(
            granted.is_some(),
            "确认过的权跨重连保留,重连时拿代次去续"
        );

        assert_eq!(abandon_pending(&mut None), None);
    }

    fn changing(
        target: &str,
        generation: Option<u64>,
        outputs: &[&str],
    ) -> Option<Held> {
        Some(Held {
            target: target.to_owned(),
            generation,
            changing: Some((
                "op".to_owned(),
                outputs
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect(),
            )),
        })
    }

    /// 换输出提交之后,重连续权报的是新成员;改回本机(空集合)就什么都不再持。
    #[test]
    fn a_committed_change_moves_the_held_target() {
        let mut moved = changing("a", Some(3), &["b"]);
        settle_change(&mut moved, "op");
        let moved = moved.expect("换到 b 之后仍持权");
        assert_eq!(moved.target, "b");
        assert_eq!(moved.generation, Some(3));
        assert!(moved.changing.is_none());

        let mut home = changing("a", Some(3), &[]);
        settle_change(&mut home, "op");
        assert!(home.is_none(), "改回本机就不再持权");
    }

    /// 别的操作的提交不动持权记录 —— 迟到的旧提交不能把新的那一次换掉。
    #[test]
    fn a_commit_for_another_operation_changes_nothing() {
        let mut held = changing("a", Some(3), &["b"]);

        settle_change(&mut held, "older");

        let held = held.expect("持权记录该还在");
        assert_eq!(held.target, "a");
        assert!(held.changing.is_some());
    }

    /// 换输出被拒:之前就持着的权留着,这一次才开始持的连记录一起忘掉。
    #[test]
    fn a_rejected_change_keeps_only_a_confirmed_hold() {
        let mut confirmed = changing("a", Some(3), &["b"]);
        assert_eq!(
            abandon_change(&mut confirmed),
            Some("op".to_owned())
        );
        let confirmed = confirmed.expect("确认过的权留着");
        assert_eq!(confirmed.target, "a");
        assert!(confirmed.changing.is_none());

        let mut fresh = changing("b", None, &["b"]);
        assert_eq!(
            abandon_change(&mut fresh),
            Some("op".to_owned())
        );
        assert!(
            fresh.is_none(),
            "这一次才开始持的权一起忘掉"
        );

        assert_eq!(abandon_change(&mut None), None);
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
