//! 把信令、名册与若干条连接编排成一个能用的同播客户端。
//!
//! 界面只需要三个动作:开机连上([`Client::start`])、我正在放这路声音
//! ([`Client::feed`])、把它推给那台设备([`Client::push`])。剩下的 —— 谁发 offer、
//! 候选往哪转、轨绑在哪条连接上 —— 都关在这里。
//!
//! **角色是行为决定的**(`docs/adr/0008`):调 [`Client::push`] 的那一端成为主控,
//! 收到陌生设备来信的那一端成为听众。没有"设为主控"这样的开关。
//!
//! 全部跑在自己的后台 runtime 上,与 `api`、`audio` 同一个模式:调用方是 Slint 的
//! UI 线程,那里没有 tokio 反应堆,也一秒钟都不能被阻塞。

use std::collections::HashMap;
use std::sync::mpsc as blocking;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use audio::ChannelSource;
use contract::{
    DeviceDto, RemoteCommand, RemoteStateDto, ServerSignal,
};
use rodio::Sample;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::signalling::SignalSender;
use crate::{
    Envelope, Peer, PeerRole, Signalling, SyncError,
    audio_track, pump,
};

/// 客户端向外抛出的事件。
///
/// 回调在**后台线程**上被调用,不在 UI 线程 —— 要改界面得自己切回去。
pub enum Event {
    /// 名册变了。含本机在内,过滤交给 [`crate::Roster`]。
    Roster(Vec<DeviceDto>),
    /// 本机成了听众:这就是对面推来的声音,送进播放器即可出声。
    ///
    /// 交出音频源而不只是通知一声:声音只能从这里取,拿不到它就只知道
    /// "有人在推",听不到任何东西。
    Listening { host: String, source: ChannelSource },
    /// 某一步失败了。给界面一行能显示的话,而不是让它停在一个永远不会变的状态上。
    Failed(String),
    /// 服务端不认这个登录态。界面该把人送回登录页 —— 同播不会自己重试,
    /// 换一个 token 之前再连也只是再得到一个 401。
    Unauthorized,

    // ── 遥控器模式(`docs/adr/0030`)。与上面几条共用同一条信令连接。 ──
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
    RemoteState { from: String, state: RemoteStateDto },
    /// 遥控器要一次完整状态,立刻回一条 [`Client::report`]。
    SnapshotRequest,
}

/// 界面发给编排循环的指令。
enum Command {
    /// 本机现在正在放这路采样。
    Feed(blocking::Receiver<Sample>),
    /// 把当前这路采样推给这台设备。
    Push(String),
    /// 关掉所有连接,回到单机。
    Leave,

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
    /// 向当前持权的那台设备要一次快照。
    Snapshot,
}

/// 本机作为**遥控器**持有的那份控制权。
///
/// 活在 [`run`] 的作用域里而不是 [`serve`] 里 —— 它要跨重连活下来,
/// 而 `peers` 那一类是每条信令各自的东西。
struct Held {
    target: String,
    /// 服务端给的代次。还没拿到就是 `None`(刚发出去、答复没回来)。
    generation: Option<u64>,
}

/// 一个连着信令服务器、随时可以推流的同播客户端。
///
/// 丢掉它,编排循环随之结束(指令通道断开),所有连接跟着关。
pub struct Client {
    commands: mpsc::UnboundedSender<Command>,
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

        runtime().spawn(run(
            base_url, device, token, events, inbox,
        ));

        Self { commands }
    }

    /// 一个谁也不连的客户端:通道建了,编排循环没起。
    ///
    /// 给那些需要一个 `Client` 才装得起来、却与同播毫无关系的调用方用 ——
    /// 主要是测试。所有指令方法都是 `let _ = send`,接收端一开始就没有,
    /// 于是每一个都成了空操作,不会 panic,也不会有后台任务。
    ///
    /// **不要在生产路径上用它。** 真要连的地方走 [`Self::start`];这里之所以
    /// 不是 `#[cfg(test)]`,是因为用它的测试在别的 crate 里(见
    /// `ui::syncplay::detached`),那个属性在这里对它们不生效。
    pub fn detached() -> Self {
        let (commands, _) = mpsc::unbounded_channel();
        Self { commands }
    }

    /// 告诉客户端本机正在放的是这路采样。
    ///
    /// 每换一首歌调一次。已经在推流时换歌,听众听到的会跟着换 ——
    /// 轨是共用的,换的只是往里灌东西的那条泵。
    pub fn feed(
        &self,
        samples: blocking::Receiver<Sample>,
    ) {
        let _ = self.commands.send(Command::Feed(samples));
    }

    /// 把本机正在放的声音推给这台设备。
    ///
    /// 可以对多台设备各调一次:星型拓扑,它们听的是同一路声音。
    pub fn push(&self, to: &str) {
        let _ = self
            .commands
            .send(Command::Push(to.to_owned()));
    }

    /// 退出同播:关掉本机的所有连接,回到单机。
    ///
    /// 听众按下任何播放键都会走到这里(`CONTEXT.md`「听众」):
    /// 连接一关,对端的泵在下一次写轨时收到错误,自己收工。
    pub fn leave(&self) {
        let _ = self.commands.send(Command::Leave);
    }

    /// 接管这台设备:本机当它的遥控器。
    ///
    /// 这是用户**主动**按下的那一次,顶掉当前的遥控器。重连之后的自动重发
    /// 由编排循环自己做,走的是另一条路(见 [`Held`])。
    pub fn claim(&self, target: &str) {
        let _ = self
            .commands
            .send(Command::Claim(target.to_owned()));
    }

    /// 输出设备选回本机:忘掉持权记录,不知会任何人。
    pub fn release_control(&self) {
        let _ = self.commands.send(Command::ReleaseControl);
    }

    /// 被控端按了「退出被遥控」。
    pub fn exit_controlled(&self) {
        let _ = self.commands.send(Command::ExitControlled);
    }

    /// 把一条命令发给正在被本机遥控的那台设备。没有持权就地丢掉 ——
    /// 界面那时本就不该让人按下去。
    pub fn command(&self, cmd: RemoteCommand) {
        let _ = self.commands.send(Command::Send(cmd));
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
    pub fn request_snapshot(&self) {
        let _ = self.commands.send(Command::Snapshot);
    }
}

/// 后台多线程 runtime。
///
/// 与 `api`、`audio` 各自那个同构、同理由(`docs/adr/0002`),但**必须是另一个** ——
/// 三个 crate 谁也不依赖谁。多线程是硬要求:听众那条泵在 async 里阻塞读 RTP,
/// 主控那条泵是普通线程,单线程 runtime 上它们会互等。
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
/// **断线必须能自愈。** 服务端重启一次就让同播永久失效,是开发时每天都会撞上的事,
/// 而症状只是状态行上一句不再变化的错误 —— 谁都看不出它其实还能救。
async fn run(
    base_url: String,
    device: DeviceDto,
    token: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    events: Arc<dyn Fn(Event) + Send + Sync>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    // 轨在重连之间**保持不变**:WebRTC 是点对点的,信令断了不影响已经建好的连接,
    // 而重建一条轨会让还在推的那条泵写进一个没人订阅的地方。
    let track = audio_track();

    let mut backoff = RETRY_MIN;
    // 上一个被服务端拒掉的 token。它没换之前不必再试 —— 结果只会一样。
    let mut rejected: Option<String> = None;
    // 本机遥控着谁。**跨重连保留** —— 断线不该让用户重新挑一次设备
    // (`docs/adr/0030`)。理由与上面那条轨相同:重连的是信令,不是遥控关系。
    let mut held: Option<Held> = None;

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
                rejected = Some(credential);
                events(Event::Unauthorized);
                continue;
            }
            Err(error) => {
                events(Event::Failed(error.to_string()));
                tokio::time::sleep(jittered(backoff)).await;
                backoff = next_backoff(backoff);
                continue;
            }
        };

        // 连上了就把退避清零:下一次断开多半是另一回事。
        backoff = RETRY_MIN;
        rejected = None;

        if !serve(
            signalling,
            &track,
            &events,
            &mut commands,
            &mut held,
        )
        .await
        {
            return;
        }
    }
}

/// 在一条已连上的信令上干活。返回是否还该重连。
///
/// `false` 意味着指令通道断了 —— [`Client`] 被丢掉了,整个客户端该收工。
async fn serve(
    mut signalling: Signalling,
    track: &Arc<TrackLocalStaticSample>,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    held: &mut Option<Held>,
) -> bool {
    let sender = signalling.sender();
    // 连接是每条信令各自的,不跨重连保留:重连之后对端会重新邀请。
    let mut peers: HashMap<String, Peer> = HashMap::new();

    // 重连之后**先确认还持不持权**,再由界面去要快照(`docs/adr/0030`)。
    // 带着手上那个代次:槽位已经换人时服务端只会回一条撤权,而不是让这台
    // 刚恢复网络的设备把接管者顶掉(产品规则:旧遥控器自动重连不夺回)。
    if let Some(current) = held.as_ref() {
        let _ = sender
            .claim(&current.target, current.generation)
            .await;
    }

    loop {
        let step = tokio::select! {
            incoming = signalling.next() => {
                let Some(message) = incoming else {
                    return true;
                };
                accept(
                    message, &mut peers, &sender, events, held,
                )
                .await
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    return false;
                };
                dispatch(
                    command, track, &mut peers, &sender, held,
                )
                .await
            }
        };

        // 一条信令处理失败不该终止会话:另一台设备版本不对、某条连接建不起来,
        // 都不影响其余的连接继续工作。报出去,接着跑。
        if let Err(error) = step {
            events(Event::Failed(error.to_string()));
        }
    }
}

/// 处理一条服务端来信。
async fn accept(
    message: ServerSignal,
    peers: &mut HashMap<String, Peer>,
    sender: &SignalSender,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
    held: &mut Option<Held>,
) -> Result<(), SyncError> {
    match message {
        ServerSignal::Roster { devices } => {
            events(Event::Roster(devices));
            Ok(())
        }
        ServerSignal::Signal { from, payload } => {
            let envelope = Envelope::decode(&payload)?;

            // 第一次收到某台设备的信令,就是它在邀请本机当听众。
            //
            // 主控先发 offer 再发候选,而服务端对同一条连接是先进先出的,
            // 所以这里第一条必定是 offer。乱序到达的候选会因为找不到远端描述
            // 而被 webrtc 拒掉,那时报错即可 —— 重发的机制不在本层。
            if !peers.contains_key(&from) {
                let peer = listen_to(&from, events).await?;
                relay_candidates(&peer, &from, sender);
                peers.insert(from.clone(), peer);
            }

            let peer = &peers[&from];
            if let Some(reply) =
                peer.accept(envelope).await?
            {
                sender.send(&from, &reply).await?;
            }
            Ok(())
        }
        ServerSignal::Error { code, message } => {
            Err(SyncError::Signalling(format!(
                "{code}: {message}"
            )))
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
            events(Event::ControlledBy { device });
            Ok(())
        }
        ServerSignal::NotControlled => {
            events(Event::NotControlled);
            Ok(())
        }
    }
}

/// 建一条听众连接,并把到达的轨接成一路能播的音频。
async fn listen_to(
    host: &str,
    events: &Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<Peer, SyncError> {
    let peer = Peer::new(PeerRole::Listener).await?;

    // on_track 必须在协商**之前**挂上:轨是在 set_remote_description 期间到达的。
    let host = host.to_owned();
    let events = events.clone();
    peer.on_track(move |track| {
        let (samples, received) =
            blocking::sync_channel(pump::LISTENER_BUFFER);
        pump::spawn_listener(track, samples);
        events(Event::Listening {
            host: host.clone(),
            source: ChannelSource::new(received),
        });
    });

    Ok(peer)
}

/// 处理一条界面指令。
async fn dispatch(
    command: Command,
    track: &Arc<TrackLocalStaticSample>,
    peers: &mut HashMap<String, Peer>,
    sender: &SignalSender,
    held: &mut Option<Held>,
) -> Result<(), SyncError> {
    match command {
        // 旧泵不用显式停:上一首的支路随播放器换歌而断,它自己就收工了。
        Command::Feed(samples) => {
            pump::spawn_host(samples, track.clone());
            Ok(())
        }
        Command::Push(to) => {
            let peer = Peer::host_on(track.clone()).await?;
            let offer = peer.create_offer().await?;
            sender.send(&to, &offer).await?;
            relay_candidates(&peer, &to, sender);
            peers.insert(to, peer);
            Ok(())
        }
        // 逐个关而不是只 clear:drop 一个 Peer 不会关连接(它是 Arc 的一份克隆),
        // 不显式 close 的话对端还以为本机在听,一直白推。
        Command::Leave => {
            for (_, peer) in peers.drain() {
                let _ = peer.close().await;
            }
            Ok(())
        }

        // 主动接管:先记下目标,代次等服务端的 ControlGranted 回来再填。
        // 先记是必须的 —— 答复到达时要靠它认出这份权是谁的。
        Command::Claim(target) => {
            *held = Some(Held {
                target: target.clone(),
                generation: None,
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
        Command::Snapshot => match held.as_ref() {
            Some(current) => {
                sender.snapshot(&current.target).await
            }
            None => Ok(()),
        },
    }
}

/// 把一条连接产出的 ICE 候选源源不断地转给对端。
///
/// 独立任务而非收集完再发(trickle ICE):等候选出完再转,每次建连都要先干等
/// 几百毫秒到几秒,而那段时间里界面上什么都没发生。
fn relay_candidates(
    peer: &Peer,
    to: &str,
    sender: &SignalSender,
) {
    let peer = peer.clone();
    let to = to.to_owned();
    let sender = sender.clone();

    tokio::spawn(async move {
        while let Some(envelope) =
            peer.next_outgoing().await
        {
            if sender.send(&to, &envelope).await.is_err() {
                return;
            }
        }
    });
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
}
