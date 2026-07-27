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

use audio::ChannelSource;
use contract::{DeviceDto, ServerSignal};
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
}

/// 界面发给编排循环的指令。
enum Command {
    /// 本机现在正在放这路采样。
    Feed(blocking::Receiver<Sample>),
    /// 把当前这路采样推给这台设备。
    Push(String),
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
    pub fn start(
        base_url: &str,
        device: DeviceDto,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let base_url = base_url.to_owned();
        let events: Arc<dyn Fn(Event) + Send + Sync> =
            Arc::new(events);

        runtime()
            .spawn(run(base_url, device, events, inbox));

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

/// 编排循环:一边收服务端来信,一边收界面指令,直到任何一边断掉。
async fn run(
    base_url: String,
    device: DeviceDto,
    events: Arc<dyn Fn(Event) + Send + Sync>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let mut signalling = match Signalling::connect(
        &base_url, device,
    )
    .await
    {
        Ok(signalling) => signalling,
        Err(error) => {
            events(Event::Failed(error.to_string()));
            return;
        }
    };

    // 整个会话共用一条轨:所有听众听的是同一路声音,编码因此只做一遍。
    let track = audio_track();
    let sender = signalling.sender();
    let mut peers: HashMap<String, Peer> = HashMap::new();

    loop {
        let step = tokio::select! {
            incoming = signalling.next() => {
                let Some(message) = incoming else { return };
                accept(message, &mut peers, &sender, &events)
                    .await
            }
            command = commands.recv() => {
                let Some(command) = command else { return };
                dispatch(
                    command, &track, &mut peers, &sender,
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
