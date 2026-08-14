use std::sync::{Arc, Mutex, mpsc};

use ui::MediaControls as _;

use super::*;

/// 一首用来测的歌。id 里那些字符是重点:平台 id 常带 `-` 与 `.`。
pub(super) fn now_playing() -> ui::NowPlaying {
    ui::NowPlaying {
        status: ui::MediaStatus::Playing,
        track_id: "ne-1962165898.v2".to_owned(),
        title: "尼古喵喵".to_owned(),
        artists: vec![
            "一个狼人".to_owned(),
            "另一个".to_owned(),
        ],
        duration_ms: 240_000,
        art_url: Some(
            "https://cdn.example/a.jpg".to_owned(),
        ),
        art: None,
        shuffle: false,
        loop_mode: ui::LoopMode::Off,
    }
}

/// 一对能被记下来的 hooks:命令进 channel,位置恒定好断言。
fn recording_hooks()
-> (ui::MediaHooks, mpsc::Receiver<ui::MediaCommand>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    (
        ui::MediaHooks {
            command: Arc::new(move |command| {
                tx.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .send(command)
                    .ok();
            }),
            position: Arc::new(|| {
                core::time::Duration::from_secs(7)
            }),
        },
        rx,
    )
}

/// 一条只给这次测试用的总线。进程随 guard 一起收掉。
struct TempBus {
    child: std::process::Child,
    address: String,
}

impl TempBus {
    fn start() -> Self {
        let mut child = std::process::Command::new(
            "dbus-daemon",
        )
        .args([
            "--session",
            "--print-address",
            "--nofork",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect(
            "起不了 dbus-daemon —— 在 nix-shell slint.nix 里跑",
        );

        use std::io::BufRead;
        let mut address = String::new();
        std::io::BufReader::new(
            child.stdout.as_mut().expect("stdout"),
        )
        .read_line(&mut address)
        .expect("读不到临时总线的地址");

        Self {
            child,
            address: address.trim().to_owned(),
        }
    }
}

impl Drop for TempBus {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// 总线上真的能看见这个播放器。
///
/// 前面几条测的都是「我们打算说什么」,这条测「说出去之后对面听到了什么」——
/// 名字有没有占上、两个接口在不在、推进去的 Metadata 读回来是不是同一份。
#[test]
fn a_real_bus_sees_the_player() {
    let bus = TempBus::start();
    let (hooks, _commands) = recording_hooks();

    let mpris = serve(Some(&bus.address), hooks)
        .expect("接不上临时总线");
    mpris.publish(&now_playing());

    let client =
        zbus::blocking::connection::Builder::address(
            bus.address.as_str(),
        )
        .unwrap()
        .build()
        .unwrap();

    let dbus = zbus::blocking::fdo::DBusProxy::new(&client)
        .unwrap();
    assert!(
        dbus.name_has_owner(BUS_NAME.try_into().unwrap())
            .unwrap(),
        "名字没占上,外壳扫不到"
    );

    let xml =
        zbus::blocking::fdo::IntrospectableProxy::builder(
            &client,
        )
        .destination(BUS_NAME)
        .unwrap()
        .path(OBJECT_PATH)
        .unwrap()
        .build()
        .unwrap()
        .introspect()
        .unwrap();
    assert!(xml.contains(PLAYER_IFACE));
    assert!(xml.contains("\"org.mpris.MediaPlayer2\""));

    let player = zbus::blocking::Proxy::new(
        &client,
        BUS_NAME,
        OBJECT_PATH,
        PLAYER_IFACE,
    )
    .unwrap();

    let status: String =
        player.get_property("PlaybackStatus").unwrap();
    assert_eq!(status, "Playing");

    let meta: HashMap<String, OwnedValue> =
        player.get_property("Metadata").unwrap();
    assert_eq!(
        String::try_from(
            meta["xesam:title"].try_clone().unwrap()
        )
        .unwrap(),
        "尼古喵喵"
    );

    // 位置由 hooks 给,不由这里记 —— 恒 7 秒,换算成微秒。
    let position: i64 =
        player.get_property("Position").unwrap();
    assert_eq!(position, 7_000_000);
}

/// bar 上按的键落到命令通道里。
///
/// 反方向。远端调 `Next`,`MediaHooks.command` 那个闭包要收到
/// `MediaCommand::Next` —— 中间隔着 zbus 的执行器线程。
#[test]
fn a_bar_button_reaches_the_command_sink() {
    let bus = TempBus::start();
    let (hooks, commands) = recording_hooks();

    let mpris = serve(Some(&bus.address), hooks)
        .expect("接不上临时总线");
    mpris.publish(&now_playing());

    let client =
        zbus::blocking::connection::Builder::address(
            bus.address.as_str(),
        )
        .unwrap()
        .build()
        .unwrap();
    let player = zbus::blocking::Proxy::new(
        &client,
        BUS_NAME,
        OBJECT_PATH,
        PLAYER_IFACE,
    )
    .unwrap();

    player.call::<_, _, ()>("Next", &()).unwrap();
    assert_eq!(
        commands.recv().unwrap(),
        ui::MediaCommand::Next
    );

    // 相对跳转的单位换算也要过这条真路径:MPRIS 给微秒,seam 收毫秒。
    player
        .call::<_, _, ()>("Seek", &(10_000_000i64,))
        .unwrap();
    assert_eq!(
        commands.recv().unwrap(),
        ui::MediaCommand::SeekBy(10_000)
    );

    // 循环键写的是 LoopStatus 属性,方言在这一端翻:Playlist = 列表循环。
    player.set_property("LoopStatus", "Playlist").unwrap();
    assert_eq!(
        commands.recv().unwrap(),
        ui::MediaCommand::SetLoop(ui::LoopMode::All)
    );
}
