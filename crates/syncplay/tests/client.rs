//! [`Client`] 这一层的验证:界面调三个方法,声音就该到对面。
//!
//! 与 `handshake.rs` 的分工:那边一步步手搓 offer/answer/候选,证明**零件**能用;
//! 这边只调 `start`/`feed`/`push`,证明**编排**是对的 —— 谁该当主控、候选往哪转、
//! 轨该绑给谁,全部交给 `Client` 自己决定。
//!
//! 界面真正剩下的只有"把事件搬到 Slint 的模型上",本文件覆盖它下面的一切。

use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use audio::codec::{
    BRANCH_CAPACITY, SYNC_CHANNELS, SYNC_SAMPLE_RATE,
};
use axum::Router;
use axum::routing::get;
use rodio::Sample;
use server::signaling::{self, SharedRoster};
use syncplay::{Client, DeviceDto, Event};

/// 等一件事发生的上界。WebRTC 建连在回环上是百毫秒级,给足余量。
const PATIENCE: Duration = Duration::from_secs(20);

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

/// 起一个客户端,并把它抛出的事件收进一条通道。
///
/// 事件回调在后台线程上跑,而断言在测试线程上 —— 通道是两者之间唯一的桥。
fn start_client(
    addr: SocketAddr,
    id: &str,
) -> (Client, mpsc::Receiver<Event>) {
    let (events, received) = mpsc::channel();
    let client = Client::start(
        &format!("ws://{addr}"),
        device(id),
        move |event| {
            let _ = events.send(event);
        },
    );
    (client, received)
}

/// 等一条满足条件的事件,超时就失败。
///
/// 事件是流水:名册会推很多次,中间还夹着别的。逐条查而不是只看第一条。
fn wait_for<T>(
    events: &mpsc::Receiver<Event>,
    what: &str,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> T {
    let deadline = Instant::now() + PATIENCE;
    while let Some(left) =
        deadline.checked_duration_since(Instant::now())
    {
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if let Event::Failed(message) = &event {
            panic!("等 {what} 时出错: {message}");
        }
        if let Some(found) = pick(event) {
            return found;
        }
    }
    panic!("等 {what} 超时");
}

/// 从事件里挑出"这些设备都在名册里了"。
///
/// 收**一组** id 而不是一个:名册是整份推的,三台设备几乎同时连上时,
/// 服务端可能只推一条含全部三台的名册。逐台分开等的话,第一次等就把那条唯一的
/// 名册消费掉了,第二次于是永远等不到 —— 而三台设备其实早就都在线。
fn roster_has(
    ids: &[&str],
) -> impl FnMut(Event) -> Option<()> {
    let ids: Vec<String> =
        ids.iter().map(|id| (*id).to_owned()).collect();
    move |event| match event {
        Event::Roster(devices)
            if ids.iter().all(|id| {
                devices.iter().any(|d| &d.id == id)
            }) =>
        {
            Some(())
        }
        _ => None,
    }
}

/// 一路不会停的 440Hz 正弦,采样率与声道数已经是同播链路的规格。
///
/// 用有界通道 + 阻塞发送:泵读多快就产多快,自己给自己限速,
/// 不必在测试里估算该产多少个采样。
fn tone() -> mpsc::Receiver<Sample> {
    let (samples, received) =
        mpsc::sync_channel(BRANCH_CAPACITY);

    std::thread::spawn(move || {
        for i in 0u64.. {
            let t = (i / u64::from(SYNC_CHANNELS)) as f32
                / SYNC_SAMPLE_RATE as f32;
            let value =
                (t * 440.0 * core::f32::consts::TAU).sin()
                    * 0.5;
            // 接收端没了就收工 —— 测试结束时泵和客户端都会被丢掉。
            if samples.send(value).is_err() {
                return;
            }
        }
    });

    received
}

/// 两台设备各自连上,彼此都能在名册事件里看到对方。
///
/// 这是界面上"可推送的设备"那一列的唯一来源:名册推不到,列表就永远是空的,
/// 而连接本身看起来一切正常。
#[tokio::test(flavor = "multi_thread")]
async fn clients_see_each_other_in_the_roster() {
    let addr = start_signalling_server().await;

    let (_host, host_events) = start_client(addr, "host");
    let (_listener, listener_events) =
        start_client(addr, "listener");

    wait_for(
        &host_events,
        "主控看到听众",
        roster_has(&["listener"]),
    );
    wait_for(
        &listener_events,
        "听众看到主控",
        roster_has(&["host"]),
    );
}

/// **端到端**:主控 `feed` + `push`,听众那边就出来一路有声音的采样。
///
/// 中间的一切 —— 谁发 offer、候选怎么转、Opus 怎么编解、RTP 怎么走 —— 都没有
/// 出现在测试里,因为界面也不该知道。判据是最终那路采样**有能量**:
/// 链路上任何一环错位,收到的都是静音或噪声。
#[tokio::test(flavor = "multi_thread")]
async fn push_delivers_playable_audio() {
    let addr = start_signalling_server().await;

    let (host, host_events) = start_client(addr, "host");
    let (_listener, listener_events) =
        start_client(addr, "listener");

    // 等听众入册再推:名册里还没有它的时候推,信令会被服务端退回。
    wait_for(
        &host_events,
        "听众入册",
        roster_has(&["listener"]),
    );

    host.feed(tone());

    // 真实用法是先放歌、过一阵再挑设备推。这段等待守的正是那个间隔:
    // 没人在听的时候主控那条泵仍在跑,若它因为"写了个寂寞"而提前收工,
    // 后来的听众就永远等不到声音 —— 而 feed 与 push 紧挨着调的话查不出来。
    tokio::time::sleep(Duration::from_secs(1)).await;
    host.push("listener");

    let source = wait_for(
        &listener_events,
        "收到主控推来的音频",
        |event| match event {
            Event::Listening { source, .. } => Some(source),
            _ => None,
        },
    );

    let heard = listen(source);
    let energy = heard.iter().map(|s| s * s).sum::<f32>()
        / heard.len() as f32;
    assert!(
        energy > 0.01,
        "听到的信号能量塌了({energy}),多半是静音或声道错位"
    );
}

/// 一路声音同时到达两台听众 —— 星型拓扑(`docs/adr/0008`)。
///
/// 两条连接共用同一条轨,所以这条测试真正守住的是:轨被绑了第二次之后,
/// **第一台**听众没有因此掉线。绑定表写错时最典型的症状就是后来者顶掉先来者。
#[tokio::test(flavor = "multi_thread")]
async fn one_source_reaches_two_listeners() {
    let addr = start_signalling_server().await;

    let (host, host_events) = start_client(addr, "host");
    let (_first, first_events) =
        start_client(addr, "first");
    let (_second, second_events) =
        start_client(addr, "second");

    wait_for(
        &host_events,
        "两台听众都入册",
        roster_has(&["first", "second"]),
    );

    host.feed(tone());
    host.push("first");
    host.push("second");

    // `Listening` 由 on_track 触发,而它是**媒体驱动**的:RTP 真的到了才有。
    // 两边都拿到,就说明同一条轨确实同时发给了两条连接。
    for (events, who) in [
        (&first_events, "第一台"),
        (&second_events, "第二台"),
    ] {
        wait_for(events, who, |event| match event {
            Event::Listening { .. } => Some(()),
            _ => None,
        });
    }
}

/// 从一路音频里收够能判断有没有声音的采样。
///
/// [`audio::ChannelSource`] 在没数据时给静音而不是结束(网络抖动不该终止播放),
/// 所以这里必须跳过 0 而不是遇 0 即停,并给个上界免得永远转下去。
fn listen(
    source: audio::ChannelSource,
) -> Vec<rodio::Sample> {
    // 100ms 的量:足够算出稳定的能量,又不必等太久。
    let enough = SYNC_SAMPLE_RATE as usize
        * SYNC_CHANNELS as usize
        / 10;
    let deadline = Instant::now() + PATIENCE;
    let mut heard = Vec::with_capacity(enough);

    for sample in source {
        if sample == 0.0 {
            if Instant::now() > deadline {
                break;
            }
            // 静音说明这一刻还没有新包到。空转会把一个核吃满,让泵更难跟上。
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        heard.push(sample);
        if heard.len() >= enough {
            break;
        }
    }

    assert!(
        heard.len() >= enough,
        "只收到 {} 个采样,不足以判断有没有声音",
        heard.len()
    );
    heard
}

/// **有听众退出过之后,推给下一个还能出声。**
///
/// 真机上抓到的回归:听众退出会关连接,死绑定留在共享轨上;
/// `write_sample` 对它报错,主控的泵一碰就自杀 —— 此后推给谁都是无声,
/// 而 ICE 一路显示 connected,现象离病因极远。
#[tokio::test(flavor = "multi_thread")]
async fn push_survives_a_listener_that_left() {
    let addr = start_signalling_server().await;

    let (host, host_events) = start_client(addr, "host");
    let (first, first_events) = start_client(addr, "first");
    let (_second, second_events) =
        start_client(addr, "second");

    wait_for(
        &host_events,
        "两台听众都入册",
        roster_has(&["first", "second"]),
    );

    host.feed(tone());
    host.push("first");
    wait_for(
        &first_events,
        "第一台收到音频",
        |event| match event {
            Event::Listening { .. } => Some(()),
            _ => None,
        },
    );

    // 第一台退出 —— 它的连接被关掉,死绑定就是这么来的。
    first.leave();
    tokio::time::sleep(Duration::from_secs(1)).await;

    host.push("second");
    let source = wait_for(
        &second_events,
        "第二台收到音频",
        |event| match event {
            Event::Listening { source, .. } => Some(source),
            _ => None,
        },
    );

    let heard = listen(source);
    let energy = heard.iter().map(|s| s * s).sum::<f32>()
        / heard.len() as f32;
    assert!(
        energy > 0.01,
        "前任听众退出后,推给下一个必须仍有声音(能量 {energy})"
    );
}
