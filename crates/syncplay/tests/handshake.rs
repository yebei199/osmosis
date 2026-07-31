//! 两台「设备」之间真的建起 WebRTC 连接。
//!
//! 两端都在本测试进程里,对着一个同样起在进程里的信令服务端握手 —— 不需要外部进程,
//! 也不需要第二台机器:ICE 走回环上的 host candidate,这正是同播的目标场景
//! (自家局域网,不打洞)。
//!
//! 分两层验证。前几条只管链路:SDP 一来一回、ICE 真的连通、RTP 包过得去 ——
//! 写进轨里的是任意字节,因为传输层不看载荷内容。最后一条则跑**整条同播链路**:
//! PCM → Opus → RTP → 解码 → PCM,任何一环的采样率或声道数错位都会让它红。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use bytes::Bytes;
use contract::{DeviceDto, ServerSignal};
use server::signaling::{self, SharedRoster};
use syncplay::{Envelope, Peer, PeerRole, Signalling};
use webrtc::media::Sample;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

/// 起一个只有信令路由的服务端,端口交给系统分配。
async fn start_signalling_server() -> SocketAddr {
    let app = Router::new()
        .route("/signal", get(signaling::handler))
        .with_state(SharedRoster::default());

    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上端口");
    let addr = listener.local_addr().expect("取不到地址");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("服务端挂了");
    });
    addr
}

fn device(id: &str) -> DeviceDto {
    DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    }
}

/// 连上信令服务器,并把设备放进名册。
#[tokio::test]
async fn hello_puts_device_in_roster() {
    let addr = start_signalling_server().await;

    let mut host = Signalling::connect(
        &format!("ws://{addr}"),
        device("host"),
    )
    .await
    .expect("连不上信令服务器");

    let first = tokio::time::timeout(
        Duration::from_secs(5),
        host.next(),
    )
    .await
    .expect("等名册超时")
    .expect("连接已关闭");

    assert!(
        matches!(first, ServerSignal::Roster { ref devices }
            if devices.iter().any(|d| d.id == "host")),
        "自报家门后应出现在名册里,实得 {first:?}"
    );
}

/// SDP 一来一回,两端的协商都走完。
#[tokio::test]
async fn offer_answer_completes_negotiation() {
    let host = Peer::new(PeerRole::Host)
        .await
        .expect("建不了主控连接");
    let listener = Peer::new(PeerRole::Listener)
        .await
        .expect("建不了听众连接");

    let offer =
        host.create_offer().await.expect("生成 offer 失败");
    assert!(matches!(offer, Envelope::Offer { .. }));

    let answer = listener
        .accept(offer)
        .await
        .expect("听众收 offer 失败")
        .expect("收下 offer 就该回 answer");
    assert!(matches!(answer, Envelope::Answer { .. }));

    host.accept(answer).await.expect("主控收 answer 失败");
}

/// ICE 真的连通 —— 这是「能不能推流」唯一的硬判据。
///
/// 协商成功但 ICE 没通的情况是存在的(候选没交换、防火墙挡住),
/// 那时 SDP 一切正常,只是声音永远不来。
#[tokio::test]
async fn both_peers_reach_connected() {
    let (host, listener) = connected_pair().await;

    assert_eq!(
        host.state(),
        RTCPeerConnectionState::Connected
    );
    assert_eq!(
        listener.state(),
        RTCPeerConnectionState::Connected
    );
}

/// 媒体真的从主控流到了听众。
///
/// 轨必须在 offer 之前就加进主控 —— 没有它,协商出来的 SDP 里没有媒体行,
/// 连接照样能"成功",而听众永远等不到任何东西。
///
/// 判据是听众的 `on_track`。它是**媒体驱动**的:RTP 包到达才触发,协商完成不算。
/// 所以这里必须真往轨里写 —— 写的是任意字节而非合法 Opus 帧,因为要证明的是
/// 「包过得去」,不是「解得开」。解码是 3c 的事,那时才需要真的编码器。
#[tokio::test]
async fn media_flows_from_host_to_listener() {
    let host = Peer::new(PeerRole::Host)
        .await
        .expect("建不了主控连接");
    let listener = Peer::new(PeerRole::Listener)
        .await
        .expect("建不了听众连接");

    let got_track = Arc::new(AtomicBool::new(false));
    let flag = got_track.clone();
    // on_track 必须在协商之前挂:回调是在轨到达那一刻查的,之后再挂就错过了。
    listener.on_track(move |_track| {
        flag.store(true, Ordering::SeqCst);
    });

    negotiate(&host, &listener).await;

    let track =
        host.track().expect("主控该有一条轨").clone();
    let writer = tokio::spawn(async move {
        // 连接建起来之前写会被丢掉,所以持续写而不是写一次。
        loop {
            let _ = track
                .write_sample(&Sample {
                    data: Bytes::from_static(&[0u8; 160]),
                    duration: Duration::from_millis(20),
                    ..Default::default()
                })
                .await;
            tokio::time::sleep(Duration::from_millis(20))
                .await;
        }
    });

    let arrived =
        wait_until(Duration::from_secs(15), || {
            got_track.load(Ordering::SeqCst)
        })
        .await;
    writer.abort();

    assert!(arrived, "听众没收到任何媒体");
}

/// ICE 候选是经信令通道单独发过去的(trickle),不是塞在 SDP 里。
#[tokio::test]
async fn ice_candidates_cross_the_signalling_channel() {
    let host = Peer::new(PeerRole::Host)
        .await
        .expect("建不了主控连接");
    let listener = Peer::new(PeerRole::Listener)
        .await
        .expect("建不了听众连接");

    let offer =
        host.create_offer().await.expect("生成 offer 失败");
    let answer = listener
        .accept(offer)
        .await
        .expect("听众收 offer 失败")
        .expect("该回 answer");
    host.accept(answer).await.expect("主控收 answer 失败");

    let candidate = tokio::time::timeout(
        Duration::from_secs(5),
        host.next_outgoing(),
    )
    .await
    .expect("等 ICE 候选超时")
    .expect("候选流已结束");

    assert!(
        matches!(candidate, Envelope::Candidate { .. }),
        "本端应产出 ICE 候选,实得 {candidate:?}"
    );
}

/// 主控断开,听众侧的连接不再是 Connected。
///
/// 「会话不存在没有主控的形态」(`docs/adr/0008`)—— 听众据此回到单机状态,
/// 而不是停在一个再也不会有声音的「播放中」上。
#[tokio::test]
async fn host_disconnect_ends_the_session() {
    let (host, listener) = connected_pair().await;

    host.close().await.expect("关不掉主控连接");

    let ended = wait_until(Duration::from_secs(10), || {
        listener.state()
            != RTCPeerConnectionState::Connected
    })
    .await;

    assert!(
        ended,
        "主控关掉后听众仍停在 {:?}",
        listener.state()
    );
}

/// 建起一对已经连通的 Peer。
async fn connected_pair() -> (Peer, Peer) {
    let host = Peer::new(PeerRole::Host)
        .await
        .expect("建不了主控连接");
    let listener = Peer::new(PeerRole::Listener)
        .await
        .expect("建不了听众连接");

    negotiate(&host, &listener).await;

    let connected =
        wait_until(Duration::from_secs(15), || {
            host.state()
                == RTCPeerConnectionState::Connected
                && listener.state()
                    == RTCPeerConnectionState::Connected
        })
        .await;
    assert!(
        connected,
        "两端没连通: 主控 {:?}, 听众 {:?}",
        host.state(),
        listener.state()
    );

    (host, listener)
}

/// 走完 offer/answer,并把两端的 ICE 候选源源不断地转给对方。
///
/// 候选转发跑在后台任务里而不是先收集完再转:trickle ICE 的候选是**边连边出**的,
/// 等它出完再转,连接就只能在候选收集超时之后才建起来。
async fn negotiate(host: &Peer, listener: &Peer) {
    let offer =
        host.create_offer().await.expect("生成 offer 失败");
    let answer = listener
        .accept(offer)
        .await
        .expect("听众收 offer 失败")
        .expect("收下 offer 就该回 answer");
    host.accept(answer).await.expect("主控收 answer 失败");

    spawn_candidate_relay(host, listener);
    spawn_candidate_relay(listener, host);
}

/// 把 `from` 产出的 ICE 候选一条条喂给 `to`。
///
/// 中继要独立于测试主线程跑,所以两端各 clone 一份搬进任务 ——
/// `Peer` 的 clone 指向同一条连接,候选流也还是那一个。
fn spawn_candidate_relay(from: &Peer, to: &Peer) {
    let from = from.clone();
    let to = to.clone();
    tokio::spawn(async move {
        while let Some(candidate) =
            from.next_outgoing().await
        {
            if to.accept(candidate).await.is_err() {
                break;
            }
        }
    });
}

/// 轮询到条件成立或超时。
///
/// WebRTC 的状态是异步推进的,没有"等到某状态"的 API,只能轮询。
/// 给上界而不是死等:条件永不成立时,测试该失败而不是挂住。
async fn wait_until(
    limit: Duration,
    mut done: impl FnMut() -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// **整条同播链路**:主控的 PCM 经 Opus + RTP 到达听众,解回来仍是有能量的信号。
///
/// 前面那条 `media_flows_from_host_to_listener` 只证明字节过得去;这条证明
/// 过去的是**能放的声音** —— 编码器、RTP 打包、解码器三者的参数必须全部对上,
/// 任何一处采样率或声道数错位,解出来的都是噪声或静音。
#[tokio::test]
async fn pcm_survives_the_whole_syncplay_path() {
    use std::sync::mpsc;

    use audio::codec::{
        FRAME_SAMPLES_PER_CHANNEL, SYNC_CHANNELS,
        SYNC_SAMPLE_RATE,
    };
    use syncplay::pump;

    let host = Peer::new(PeerRole::Host)
        .await
        .expect("建不了主控连接");
    let listener = Peer::new(PeerRole::Listener)
        .await
        .expect("建不了听众连接");

    // 听众:收到轨就开一条解码泵,把 PCM 送进通道。
    let (pcm_tx, pcm_rx) = mpsc::sync_channel(48_000);
    listener.on_track(move |track| {
        pump::spawn_listener(track, pcm_tx.clone());
    });

    negotiate(&host, &listener).await;

    // 主控:一路 440Hz 正弦当作"正在播放的音乐"。
    let (samples_tx, samples_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for i in 0.. {
            let t = (i / SYNC_CHANNELS as usize) as f32
                / SYNC_SAMPLE_RATE as f32;
            let sample =
                (t * 440.0 * core::f32::consts::TAU).sin()
                    * 0.5;
            if samples_tx.send(sample).is_err() {
                return;
            }
            // 每凑一帧歇一下,免得瞬间灌爆编码器 —— 真实播放也是这个节奏。
            if i % (FRAME_SAMPLES_PER_CHANNEL
                * SYNC_CHANNELS as usize)
                == 0
            {
                std::thread::sleep(Duration::from_millis(
                    5,
                ));
            }
        }
    });
    pump::spawn_host(
        samples_rx,
        host.track().expect("主控该有一条轨").clone(),
    );

    // 收够一帧的量就够判断:再多只是等得久。
    let mut received = Vec::new();
    let got_audio =
        wait_until(Duration::from_secs(20), || {
            received.extend(pcm_rx.try_iter());
            received.len() >= FRAME_SAMPLES_PER_CHANNEL
        })
        .await;
    assert!(
        got_audio,
        "听众只收到 {} 个采样",
        received.len()
    );

    let energy: f32 =
        received.iter().map(|s| s * s).sum::<f32>()
            / received.len() as f32;
    assert!(
        energy > 0.001,
        "解出来的信号能量塌了({energy}):多半是采样率或声道数在某一环对不上"
    );
}
