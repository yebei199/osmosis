//! 播放组走一遍真信令(#137 ⑤):校时收敛、组通告、共同计划经服务端转到跟随端。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use contract::{
    DeviceDto, GroupPlanDto, LoopModeDto, TrackDto,
};
use server::syncplay::signaling;
use syncplay::clock::monotonic_ns;
use syncplay::{Client, Event};

const TOKEN: &str = "test-token";
const PATIENCE: Duration = Duration::from_secs(5);

async fn start_server() -> SocketAddr {
    let app = signaling::unauthenticated_test_router(
        signaling::Timing::default(),
    );
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

fn spawn_client(
    addr: SocketAddr,
    id: &str,
) -> (Client, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let client = Client::start(
        &format!("ws://{addr}"),
        DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        },
        || Some(TOKEN.to_owned()),
        move |event| {
            let _ = tx
                .lock()
                .expect("事件通道锁中毒")
                .send(event);
        },
    );
    (client, rx)
}

async fn wait_for<T>(
    rx: &mpsc::Receiver<Event>,
    what: &str,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if let Ok(event) = rx.try_recv()
            && let Some(picked) = pick(&event)
        {
            return picked;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "等 {what} 超时"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn plan(seq: u64) -> GroupPlanDto {
    GroupPlanDto {
        seq,
        clock_epoch: server::syncplay::clock::epoch(),
        queue_id: 3,
        revision: 1,
        entry_id: 10,
        track: TrackDto {
            platform: "netease".to_owned(),
            id: "1".to_owned(),
            title: "歌".to_owned(),
            alias: None,
            artists: vec![],
            cover: None,
            duration_ms: 200_000,
        },
        anchor_us: 1_000_000,
        position_us: 0,
        playing: true,
        start_us: 1_000_000,
        next: None,
        valid_until_us: 201_000_000,
        play_order: None,
        round: 0,
        shuffled: false,
        loop_mode: LoopModeDto::Off,
    }
}

/// 客户端自己与服务端往返校时：同一台机器上，换算出来的本机时刻与真实的差不过一两毫秒。
#[tokio::test]
async fn the_clock_converges_against_the_server() {
    let addr = start_server().await;
    let (client, _rx) = spawn_client(addr, "phone");
    tokio::time::sleep(Duration::from_secs(3)).await;

    let clock = client.clock();
    let clock = clock.lock().expect("校时锁中毒");
    let epoch = clock.epoch().expect("三秒里该有往返了");
    assert_eq!(epoch, server::syncplay::clock::epoch());
    let server_us = server::syncplay::clock::now_us();
    let local = monotonic_ns();
    let converted = clock
        .to_local_ns(epoch, server_us)
        .expect("纪元对得上就换算得了");
    assert!(
        (converted - local).abs() < 2_000_000,
        "换算差了 {}µs",
        (converted - local) / 1_000
    );
}

/// 本机在放时加入 pc:本机是主端，它发的计划经服务端转到 pc;组的样子两边都知道。
#[tokio::test]
async fn the_masters_plan_reaches_the_follower() {
    let addr = start_server().await;
    let (phone, phone_rx) = spawn_client(addr, "phone");
    let (_pc, pc_rx) = spawn_client(addr, "pc");
    wait_for(&phone_rx, "名册", |event| match event {
        Event::Roster(devices) => devices
            .iter()
            .any(|d| d.id == "pc")
            .then_some(()),
        _ => None,
    })
    .await;

    phone.begin_outputs(
        "op",
        vec!["phone".to_owned(), "pc".to_owned()],
        Some("phone".to_owned()),
    );
    wait_for(&phone_rx, "OutputsBegun", |event| {
        matches!(event, Event::OutputsBegun { .. })
            .then_some(())
    })
    .await;
    phone.commit_outputs("op", None);
    // 任期从服务端启动那一刻的挂钟数起(#142),不是 1;以提交的回话为准。
    let committed =
        wait_for(&phone_rx, "OutputsCommitted", |event| {
            match event {
                Event::OutputsCommitted {
                    term, ..
                } => Some(*term),
                _ => None,
            }
        })
        .await;

    let (term, master) =
        wait_for(&pc_rx, "组通告", |event| match event {
            Event::Group {
                term,
                master,
                members,
            } if members.len() == 2
                && *term == committed =>
            {
                Some((*term, master.clone()))
            }
            _ => None,
        })
        .await;
    assert_eq!(master.as_deref(), Some("phone"));

    phone.publish_plan(term, plan(1));

    let (from, seq) =
        wait_for(&pc_rx, "共同计划", |event| match event
        {
            Event::GroupPlan { from, plan, .. } => {
                Some((from.clone(), plan.seq))
            }
            _ => None,
        })
        .await;
    assert_eq!((from.as_str(), seq), ("phone", 1));
}
