//! 信令的端到端测试:真的开 WebSocket 连接,真的收发。
//!
//! 与 `live_bangdream.rs` 不同,这些**不需要外部进程** —— 服务端在测试进程里起,
//! 端口交给系统分配。所以它们不带 `#[ignore]`,每次 `cargo test` 都跑。
//!
//! 单测证明的是「路由函数把消息投给了谁」;这里证明的是「两条真实连接之间
//! 消息确实过去了」—— 序列化、帧类型、收发两端拆分,任何一环错了单测都看不见。

use std::net::SocketAddr;

use contract::{ClientSignal, DeviceDto, ServerSignal};
use futures_util::{SinkExt, StreamExt};
use server::syncplay::signaling::{self, Timing};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 在随机端口上起一个只有信令路由的服务端,返回它的地址。
///
/// 端口写 0 让系统分配:写死端口的话两个测试并发跑就会互相撞,
/// 而那种失败每次落在不同的测试上,看起来像随机的 flaky。
///
/// 走不鉴权的测试路由:这里验的是"消息有没有过去",账号从哪来是
/// `signal_auth.rs` 的事,不必为此起一个数据库。
async fn start_server() -> SocketAddr {
    start_server_with(Timing::default()).await
}

/// 同上,但自己指定时限 —— 超时那几条不能用生产的秒级数字,
/// 否则每次 `cargo test` 都要为它们干等十几秒。
async fn start_server_with(timing: Timing) -> SocketAddr {
    let app =
        signaling::unauthenticated_test_router(timing);

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

/// 连上去并自报家门,返回连接。账号是测试路由的缺省值。
async fn connect(addr: SocketAddr, id: &str) -> Socket {
    connect_as(addr, 1, id).await
}

/// 同上,但指定账号 —— 分桶那几条要两个账号才验得出来。
///
/// 握手走完才返回:服务端在入册之前先回一条 `Welcome`(`docs/adr/0031`),
/// 这个 helper 把它吃掉,于是后面每条测试看到的第一条仍然是它关心的那条。
async fn connect_as(
    addr: SocketAddr,
    account: i64,
    id: &str,
) -> Socket {
    let (mut socket, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal?account={account}"),
    )
    .await
    .expect("连不上信令端点");

    let hello = ClientSignal::Hello {
        device: DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        },
        protocol_version: contract::PROTOCOL_VERSION,
    };
    socket
        .send(Message::text(
            serde_json::to_string(&hello)
                .expect("序列化失败"),
        ))
        .await
        .expect("发不出 Hello");

    // 握手应答先来,入册的名册在它后面。吃掉它,后面每条测试看到的第一条
    // 仍然是它自己关心的那条。
    let welcome = next_signal(&mut socket).await;
    assert!(
        matches!(
            welcome,
            ServerSignal::Welcome { protocol_version }
                if protocol_version == contract::PROTOCOL_VERSION
        ),
        "握手第一条该是 Welcome,收到的是 {welcome:?}"
    );

    socket
}

/// 讲旧协议的客户端在**入册之前**就被拒(AC-7)。
///
/// 判据不是「它收到了一条错误」—— 旧端根本解不出新消息。判据是**它没进
/// 任何人的名册**:入册是取得控制权的前提,所以它连遥控的机会都没有。
///
/// 旧客户端的 `Hello` 里压根没有 `protocol_version` 字段,所以这里发的是
/// 一份手搓的旧格式 JSON,不是把常量改小 —— 后者测不到契约那个
/// `#[serde(default)]`,而少了它服务端会把整条消息静默丢掉,连接挂在
/// 超时上,「版本不对」就此与「网络不好」长得一模一样。
#[tokio::test]
async fn an_old_client_is_refused_before_it_joins_the_roster()
 {
    let addr = start_server().await;

    // 先放一台新客户端进去,它是观察名册的那双眼睛。
    let mut watcher = connect(addr, "watcher").await;
    assert!(matches!(
        next_signal(&mut watcher).await,
        ServerSignal::Roster { ref devices } if devices.len() == 1
    ));

    let (mut old, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal?account=1"),
    )
    .await
    .expect("连不上信令端点");
    old.send(Message::text(
        r#"{"type":"hello","device":{"id":"old","name":"旧端"}}"#,
    ))
    .await
    .expect("发不出旧格式 Hello");

    // 旧端这一侧:收到一条它读不懂的应答,然后连接就关了。
    let answered = next_signal(&mut old).await;
    assert!(
        matches!(answered, ServerSignal::Welcome { .. }),
        "该先回一条 Welcome,收到的是 {answered:?}"
    );
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        old.next(),
    )
    .await
    .expect("等连接关闭超时");
    assert!(
        closed.is_none()
            || matches!(
                closed,
                Some(Ok(Message::Close(_)))
            ),
        "版本对不上该关掉连接,却还能收到 {closed:?}"
    );

    // 观察者这一侧:名册**从头到尾没有变过**。旧端要是入了册,
    // 这里会收到一条两台设备的 Roster。
    let quiet = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        watcher.next(),
    )
    .await;
    assert!(
        quiet.is_err(),
        "旧端不该入册,而名册动了:{quiet:?}"
    );
}

/// 同播时代(协议 3)的客户端同样在入册之前被拒(#137)。
///
/// 删同播是删变体,不兼容:协议 3 的客户端还会发 SDP 转发,新服务端一律不认。
/// 升版本号之后,老端看到的是一次明确的版本拒绝,而不是「推给某台设备没反应」。
/// 版本号手写成 3,不写 `PROTOCOL_VERSION - 1`:要钉住的是「最后一个带同播的
/// 版本」这件具体的事,下一次升版本不该让这条测试悄悄改测别的东西。
#[tokio::test]
async fn a_client_from_the_syncplay_era_is_refused() {
    let addr = start_server().await;

    let mut watcher = connect(addr, "watcher").await;
    let _ = next_signal(&mut watcher).await;

    let (mut old, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal?account=1"),
    )
    .await
    .expect("连不上信令端点");
    old.send(Message::text(
        r#"{"type":"hello","device":{"id":"v3","name":"同播时代的端"},"protocol_version":3}"#,
    ))
    .await
    .expect("发不出 Hello");

    let answered = next_signal(&mut old).await;
    assert!(
        matches!(
            answered,
            ServerSignal::Welcome { protocol_version }
                if protocol_version != 3
        ),
        "该回一条报新版本号的 Welcome,收到的是 {answered:?}"
    );
    let quiet = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        watcher.next(),
    )
    .await;
    assert!(
        quiet.is_err(),
        "协议 3 的端不该入册,而名册动了:{quiet:?}"
    );
}

/// 一条 SDP 转发送到服务端,哪里也去不了(#137)。
///
/// 服务端对解不出来的消息一律丢弃,所以判据是目标那一侧什么都没收到。
#[tokio::test]
async fn a_webrtc_relay_message_reaches_nobody() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    let _ = next_signal(&mut a).await;
    let mut b = connect(addr, "b").await;
    let _ = next_signal(&mut a).await;
    let _ = next_signal(&mut b).await;

    a.send(Message::text(
        r#"{"type":"signal","to":"b","payload":"v=0"}"#,
    ))
    .await
    .expect("发不出消息");

    let leaked = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        next_signal(&mut b),
    )
    .await;
    assert!(
        leaked.is_err(),
        "SDP 转发还被送到了目标设备: {leaked:?}"
    );
}

/// 读下一条服务端消息。
async fn next_signal(socket: &mut Socket) -> ServerSignal {
    loop {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.next(),
        )
        .await
        .expect("等服务端消息超时")
        .expect("连接已关闭")
        .expect("读连接出错");

        if let Message::Text(text) = message {
            return serde_json::from_str(&text)
                .expect("服务端消息不是认识的形状");
        }
    }
}

/// 两台设备连上后,彼此都出现在对方的名册里。
#[tokio::test]
async fn two_devices_see_each_other() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    // a 独自在线时先收到一份只有自己的名册。
    assert!(matches!(
        next_signal(&mut a).await,
        ServerSignal::Roster { ref devices } if devices.len() == 1
    ));

    let mut b = connect(addr, "b").await;

    // b 上线让名册变化,两边都该被推到。
    let ServerSignal::Roster { devices } =
        next_signal(&mut a).await
    else {
        panic!("a 没收到更新后的名册");
    };
    let ids: Vec<&str> =
        devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"], "a 看到的名册不对");

    let ServerSignal::Roster { devices } =
        next_signal(&mut b).await
    else {
        panic!("b 没收到名册");
    };
    assert_eq!(devices.len(), 2, "b 看到的名册不对");
}

/// 一台设备断开,另一台**被主动推**新名册。
///
/// 关键是"主动" —— 让客户端轮询的话,下线到被发现之间有一段空窗,
/// 而那段时间里去遥控它必然失败,看起来却像「按了没反应」。
#[tokio::test]
async fn disconnect_updates_the_other_device() {
    let addr = start_server().await;

    let mut a = connect(addr, "a").await;
    let _ = next_signal(&mut a).await;
    let b = connect(addr, "b").await;
    let _ = next_signal(&mut a).await;

    drop(b);

    let ServerSignal::Roster { devices } =
        next_signal(&mut a).await
    else {
        panic!("a 没收到断开后的名册");
    };
    assert_eq!(
        devices.len(),
        1,
        "b 断开后名册里不该还有它"
    );
    assert_eq!(devices[0].id, "a");
}

/// 连上了却一直不自报家门,会被断开。
///
/// 鉴权只保证对端有账号,不保证它还打算说话:没有这道超时,
/// 一个连上就沉默的客户端能白占一个连接槽,而名册里看不见它。
#[tokio::test]
async fn silent_connection_is_dropped_after_the_hello_timeout()
 {
    let addr = start_server_with(Timing {
        hello: std::time::Duration::from_millis(100),
        ..Timing::default()
    })
    .await;

    let (mut socket, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/signal"),
    )
    .await
    .expect("连不上信令端点");

    // 一句 Hello 都不发。服务端该在时限到了之后关掉这条连接 ——
    // 读到流末尾(或读出错)都算关掉了。
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            while let Some(message) = socket.next().await {
                if message.is_err() {
                    return;
                }
            }
        },
    )
    .await;

    assert!(
        closed.is_ok(),
        "沉默的连接没有被断开,它会一直占着"
    );
}

/// 两个账号各两台设备:各自的名册只有自己那两台。
///
/// 不分桶的话,四台设备互相可见 —— 而设备名多半就是主机名,
/// 等于把别人机器的名字摆到界面上。
#[tokio::test]
async fn each_account_only_sees_its_own_two_devices() {
    let addr = start_server().await;

    let mut alice1 = connect_as(addr, 1, "a1").await;
    let mut alice2 = connect_as(addr, 1, "a2").await;
    // 连着即可,断言看的是 bob2 那份名册 —— 但它得活着,否则 bob 那桶只剩一台。
    let _bob1 = connect_as(addr, 2, "b1").await;
    let mut bob2 = connect_as(addr, 2, "b2").await;

    // 各自等到自己那份两台的名册。别人上线不该再推给自己,
    // 所以这里读到的下一条就该是终态。
    let ServerSignal::Roster { devices } =
        next_signal(&mut alice2).await
    else {
        panic!("alice2 没收到名册");
    };
    let ids: Vec<&str> =
        devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["a1", "a2"], "alice 看到的名册不对");

    let ServerSignal::Roster { devices } =
        next_signal(&mut bob2).await
    else {
        panic!("bob2 没收到名册");
    };
    let ids: Vec<&str> =
        devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["b1", "b2"], "bob 看到的名册不对");

    // alice1 那边一路读下来也只该见过自己账号的设备。
    let ServerSignal::Roster { devices } =
        next_signal(&mut alice1).await
    else {
        panic!("alice1 没收到名册");
    };
    assert!(
        devices.iter().all(|d| d.id.starts_with('a')),
        "alice1 的名册里混进了别人的设备: {devices:?}"
    );
}

/// 跨账号接管得到 `Error`,而不是把控制请求送到对面。
///
/// 当初拿同播的 SDP 转发验这件事;转发删掉之后(#137),账号桶的边界靠遥控
/// 这条仍会点名目标设备的消息来验。
#[tokio::test]
async fn claiming_a_device_of_another_account_is_refused() {
    let addr = start_server().await;

    let mut alice = connect_as(addr, 1, "a1").await;
    let _ = next_signal(&mut alice).await;
    let mut bob = connect_as(addr, 2, "b1").await;
    let _ = next_signal(&mut bob).await;

    say(
        &mut alice,
        &ClientSignal::ClaimControl {
            target: "b1".to_owned(),
            resume: None,
        },
    )
    .await;

    let received = next_signal(&mut alice).await;
    assert!(
        matches!(
            received,
            ServerSignal::Error { ref code, .. }
                if code == "device_offline"
        ),
        "实得 {received:?}"
    );

    // bob 那边一个字都不该收到。给它一点时间,再确认收件箱是空的。
    let leaked = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        next_signal(&mut bob),
    )
    .await;
    assert!(
        leaked.is_err(),
        "别人账号的设备收到了不该收到的东西: {leaked:?}"
    );
}

/// 不回 Pong 的连接会被清出名册,而且别人**被推到**这个变化。
///
/// 没有这道探活,一条被路由器悄悄丢掉的连接要等 TCP 自己发现 —— 十几分钟里
/// 名册一直说它在线,谁去遥控它谁卡在那儿。
#[tokio::test]
async fn a_device_that_stops_answering_pings_is_dropped() {
    let addr = start_server_with(Timing {
        ping_every: std::time::Duration::from_millis(50),
        ..Timing::default()
    })
    .await;

    let mut a = connect(addr, "a").await;
    let _ = next_signal(&mut a).await;
    // 连上之后**再也不轮询它**:tungstenite 只在被轮询时才回 Pong,
    // 于是服务端那边的回音就此断了,而 TCP 连接还好端端地开着。
    let _mute = connect(addr, "b").await;

    let ServerSignal::Roster { devices } =
        next_signal(&mut a).await
    else {
        panic!("a 没收到 b 上线的名册");
    };
    assert_eq!(devices.len(), 2, "b 该先在线");

    let dropped = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                if let ServerSignal::Roster { devices } =
                    next_signal(&mut a).await
                    && devices.len() == 1
                {
                    return devices;
                }
            }
        },
    )
    .await
    .expect("不回 Pong 的设备没有被清出名册");

    assert_eq!(dropped[0].id, "a");
}

/// 同一台设备重连:新连接入册之后旧连接才收工,名册里仍然有它。
///
/// 这是重连最常见的时序。旧连接的清理不看代次的话,它会把刚上线的新连接
/// 一起带走 —— 设备自己以为在线,别人却怎么也找不到它。
#[tokio::test]
async fn reconnecting_the_same_device_keeps_it_in_the_roster()
 {
    let addr = start_server().await;

    let mut watcher = connect(addr, "watcher").await;
    let _ = next_signal(&mut watcher).await;

    let old = connect(addr, "device").await;
    let _ = next_signal(&mut watcher).await;

    // 同一个 device id 再连一次。旧连接此时还开着。
    let _new = connect(addr, "device").await;
    let _ = next_signal(&mut watcher).await;

    // 现在才关掉旧的那条。
    drop(old);

    // 给旧连接的清理留出时间,再看名册里还有没有这台设备。
    let still_there = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        async {
            loop {
                if let ServerSignal::Roster { devices } =
                    next_signal(&mut watcher).await
                    && !devices
                        .iter()
                        .any(|d| d.id == "device")
                {
                    return devices;
                }
            }
        },
    )
    .await;

    assert!(
        still_there.is_err(),
        "旧连接收工时把重连上来的那条从名册里带走了: {still_there:?}"
    );
}

/// 超过单条消息上限的帧会让那条连接被关掉,而不是让服务端替它攒下整块内存。
///
/// 遥控的命令与上报都是几百字节。axum 的默认上限是 64 MiB ——
/// 那意味着一条连接能让服务端为它单独攒出 64 MiB。
#[tokio::test]
async fn an_oversized_message_closes_the_connection() {
    let addr = start_server().await;

    let mut watcher = connect(addr, "watcher").await;
    let _ = next_signal(&mut watcher).await;
    let mut fat = connect(addr, "fat").await;
    let ServerSignal::Roster { devices } =
        next_signal(&mut watcher).await
    else {
        panic!("watcher 没收到名册");
    };
    assert_eq!(devices.len(), 2, "fat 该先在线");

    // 128 KiB,是上限的两倍。上限在解析之前就起作用,所以内容是不是一条
    // 合法的信令无关紧要。
    fat.send(Message::text("x".repeat(128 * 1024)))
        .await
        .expect("发不出超大消息");

    let dropped = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                if let ServerSignal::Roster { devices } =
                    next_signal(&mut watcher).await
                    && devices.len() == 1
                {
                    return devices;
                }
            }
        },
    )
    .await
    .expect("超大消息之后那条连接没有被关掉");

    assert_eq!(dropped[0].id, "watcher");
}

/// 发一条客户端消息。
async fn say(socket: &mut Socket, message: &ClientSignal) {
    socket
        .send(Message::text(
            serde_json::to_string(message)
                .expect("序列化失败"),
        ))
        .await
        .expect("发不出消息");
}

/// 读到第一条满足条件的消息为止,名册之类的旁白跳过。
async fn next_where(
    socket: &mut Socket,
    wanted: impl Fn(&ServerSignal) -> bool,
) -> ServerSignal {
    loop {
        let message = next_signal(socket).await;
        if wanted(&message) {
            return message;
        }
    }
}

/// 被控端每秒上报的那一条。内容无所谓,服务端不看。
fn report() -> ClientSignal {
    ClientSignal::State {
        state: Box::new(contract::RemoteStateDto {
            track: None,
            position_ms: 1_000,
            state: contract::RemotePlayState::Playing,
            queue_id: None,
            revision: None,
            applied_revision: None,
            entry_id: None,
            queue_len: 0,
            volume: 1.0,
            epoch: 1,
            state_seq: 1,
            operation: None,
            fault: None,
            route: None,
        }),
    }
}

/// phone 接管 pc,等两边都确认,返回代次。
async fn phone_claims_pc(
    phone: &mut Socket,
    pc: &mut Socket,
) -> u64 {
    say(
        phone,
        &ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        },
    )
    .await;
    next_where(pc, |m| {
        matches!(m, ServerSignal::ControlledBy { .. })
    })
    .await;
    let ServerSignal::ControlGranted { generation } =
        next_where(phone, |m| {
            matches!(m, ServerSignal::ControlGranted { .. })
        })
        .await
    else {
        unreachable!()
    };
    generation
}

/// 遥控器消失(#111 的 force-stop):被控端照常每秒上报,满一个租约之后
/// 拿到 `NotControlled`,而不是在那之前 —— 早了就是把短暂断网当成离线。
#[tokio::test]
async fn a_vanished_controller_stops_locking_the_target() {
    let lease = std::time::Duration::from_millis(300);
    let addr = start_server_with(Timing {
        lease,
        ..Timing::default()
    })
    .await;
    let mut pc = connect(addr, "pc").await;
    let mut phone = connect(addr, "phone").await;
    phone_claims_pc(&mut phone, &mut pc).await;

    drop(phone);
    let gone = std::time::Instant::now();

    let freed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                say(&mut pc, &report()).await;
                let answer = tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    next_where(&mut pc, |m| {
                        matches!(
                            m,
                            ServerSignal::NotControlled
                        )
                    }),
                )
                .await;
                if answer.is_ok() {
                    return gone.elapsed();
                }
            }
        },
    )
    .await
    .expect("遥控器消失之后,被控端一直没被放开");

    assert!(
        freed >= lease,
        "租约没满就放开了被控端:{freed:?} < {lease:?}"
    );
}

/// 遥控器短暂断网:服务端已经把它出册、租约已经起算,它在租约内带着代次
/// 续上。之后再过好几个租约,被控端的上报照旧转给它,没有一条 `NotControlled`。
#[tokio::test]
async fn a_controller_back_within_the_lease_keeps_control()
{
    let lease = std::time::Duration::from_millis(300);
    let addr = start_server_with(Timing {
        lease,
        ..Timing::default()
    })
    .await;
    let mut pc = connect(addr, "pc").await;
    let mut phone = connect(addr, "phone").await;
    let generation =
        phone_claims_pc(&mut phone, &mut pc).await;

    drop(phone);
    // pc 看到 phone 出册,才说明服务端那边租约真的起算了。
    next_where(&mut pc, |m| {
        matches!(m, ServerSignal::Roster { devices } if devices.len() == 1)
    })
    .await;

    let mut phone = connect(addr, "phone").await;
    say(
        &mut phone,
        &ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: Some(generation),
        },
    )
    .await;
    let resumed = next_where(&mut phone, |m| {
        !matches!(m, ServerSignal::Roster { .. })
    })
    .await;
    assert_eq!(
        resumed,
        ServerSignal::ControlGranted { generation },
        "租约内回来却没续上"
    );

    tokio::time::sleep(lease * 3).await;
    say(&mut pc, &report()).await;

    assert!(
        matches!(
            next_where(&mut phone, |m| {
                !matches!(m, ServerSignal::Roster { .. })
            })
            .await,
            ServerSignal::State { .. }
        ),
        "续上之后被控端的上报没转给遥控器"
    );
    let unlocked = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        next_where(&mut pc, |m| {
            matches!(m, ServerSignal::NotControlled)
        }),
    )
    .await;
    assert!(
        unlocked.is_err(),
        "续上了的遥控关系被租约清掉了"
    );
}

/// 遥控器断网超过十五秒会自己回本机、交出持权(#118),重连时就不再续。
/// 它若在服务端探活发现旧连接死掉**之前**就连回来,旧连接的收尾被当成
/// 「被顶替」,什么都不动 —— 于是租约根本没起算,被控端照样永远挂着横幅。
/// 以新连接重新入册也得算会话断过:不续,满租约就清。
#[tokio::test]
async fn a_controller_that_rejoins_without_resuming_loses_control()
 {
    let lease = std::time::Duration::from_millis(300);
    let addr = start_server_with(Timing {
        lease,
        ..Timing::default()
    })
    .await;
    let mut pc = connect(addr, "pc").await;
    let mut phone = connect(addr, "phone").await;
    phone_claims_pc(&mut phone, &mut pc).await;

    // 旧连接不关:服务端还没发现它死了,新连接就把它顶替掉。
    let _rejoined = connect(addr, "phone").await;
    let rejoined_at = std::time::Instant::now();

    let freed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                say(&mut pc, &report()).await;
                let answer = tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    next_where(&mut pc, |m| {
                        matches!(
                            m,
                            ServerSignal::NotControlled
                        )
                    }),
                )
                .await;
                if answer.is_ok() {
                    return rejoined_at.elapsed();
                }
            }
        },
    )
    .await
    .expect(
        "遥控器换了条连接、不再续权,被控端却一直没被放开",
    );

    assert!(
        freed >= lease,
        "租约没满就放开了被控端:{freed:?} < {lease:?}"
    );
    drop(phone);
}
