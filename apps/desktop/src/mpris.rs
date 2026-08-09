//! Linux 这一端的系统媒体控件:session bus 上的 MPRIS。
//!
//! 占住 `org.mpris.MediaPlayer2.osmosis` 这个名字,实现 `org.mpris.MediaPlayer2`
//! 与 `org.mpris.MediaPlayer2.Player` 两个接口。DMS/quickshell、waybar、GNOME 的
//! 锁屏控件认的都是它。
//!
//! 只说方言,不记状态 —— 该记的都在 `ui::media` 那一侧(见 `docs/adr/0020`)。
//! 这里存的那份 `NowPlaying` 不是第二个真相,是**上一句话的副本**:D-Bus 的属性
//! 是被拉的,对面什么时候来问不由我们决定,总得有个东西接得住那一问。

/// 别的桌面(Windows / macOS)还没有实现。
#[cfg(not(target_os = "linux"))]
pub fn start(
    _hooks: ui::MediaHooks,
) -> Box<dyn ui::MediaControls> {
    Box::new(ui::NoControls)
}

#[cfg(target_os = "linux")]
pub use linux::start;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    /// 总线上的名字。外壳按这个前缀扫,后缀是谁不重要,但得是我们自己。
    const BUS_NAME: &str = "org.mpris.MediaPlayer2.osmosis";
    /// 规范钉死的对象路径,两个接口都挂在它上面。
    const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
    const PLAYER_IFACE: &str =
        "org.mpris.MediaPlayer2.Player";
    const PROPERTIES_IFACE: &str =
        "org.freedesktop.DBus.Properties";
    /// 曲目路径的前缀。规范只要求是合法 object path 且能区分曲目,前缀由我们定。
    const TRACK_PREFIX: &str = "/io/github/osmosis/track/";
    /// 规范给「什么都没放」留的那条专用路径。
    const NO_TRACK: &str =
        "/org/mpris/MediaPlayer2/TrackList/NoTrack";

    /// MPRIS 的时间单位是微秒,seam 那边统一是毫秒。
    const MICROS_PER_MILLI: i64 = 1_000;

    /// 起服务。连不上就退回什么都不做 —— 没有 MPRIS 不影响出声。
    ///
    /// 会连不上的情形都很正常:没有 session bus(纯 tty、容器里),或者名字
    /// 已经被另一个实例占了(启动锁挡不住 `--user` 之外的场景)。
    pub fn start(
        hooks: ui::MediaHooks,
    ) -> Box<dyn ui::MediaControls> {
        match serve(None, hooks) {
            Ok(mpris) => Box::new(mpris),
            Err(err) => {
                log::warn!(
                    "接不上 MPRIS,系统媒体控件里不会有这个播放器: {err}"
                );
                Box::new(ui::NoControls)
            }
        }
    }

    /// 两个接口共用的那一份。
    struct Shared {
        hooks: ui::MediaHooks,
        /// 上一次推出去的东西。属性被拉时从这里答。
        now: Mutex<ui::NowPlaying>,
    }

    impl Shared {
        fn send(&self, command: ui::MediaCommand) {
            (self.hooks.command)(command);
        }

        /// 锁中毒了也照常答:里面是一份纯数据的快照,毒不着它。
        fn now(&self) -> ui::NowPlaying {
            self.now
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    /// 已经接上总线的 MPRIS。`conn` 必须活着 —— 丢了它名字就还回去了。
    struct Mpris {
        conn: zbus::blocking::Connection,
        shared: Arc<Shared>,
    }

    impl ui::MediaControls for Mpris {
        fn publish(&self, now: &ui::NowPlaying) {
            *self
                .shared
                .now
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                now.clone();

            // 属性变更要自己吆喝一声,否则对面只有在下次主动来问时才知道 ——
            // 而 bar 上的卡片画完就不再问了,于是永远停在第一首歌。
            let mut changed: HashMap<&str, Value<'_>> =
                HashMap::new();
            changed.insert(
                "PlaybackStatus",
                Value::from(status_name(now.status)),
            );
            changed.insert(
                "Metadata",
                Value::from(metadata_of(now)),
            );
            changed.insert(
                "Shuffle",
                Value::from(now.shuffle),
            );

            if let Err(err) = self.conn.emit_signal(
                None::<()>,
                OBJECT_PATH,
                PROPERTIES_IFACE,
                "PropertiesChanged",
                &(PLAYER_IFACE, changed, &[] as &[&str]),
            ) {
                log::debug!(
                    "MPRIS 属性变更发不出去: {err}"
                );
            }
        }
    }

    /// 接上总线并注册两个接口。
    ///
    /// `address` 为 `None` 走会话总线;测试传一条临时总线的地址,免得往用户
    /// 自己的总线上摆一个假播放器。
    fn serve(
        address: Option<&str>,
        hooks: ui::MediaHooks,
    ) -> zbus::Result<Mpris> {
        let shared = Arc::new(Shared {
            hooks,
            now: Mutex::new(ui::NowPlaying::default()),
        });

        let builder = match address {
            Some(address) => {
                zbus::blocking::connection::Builder::address(
                    address,
                )?
            }
            None => {
                zbus::blocking::connection::Builder::session()?
            }
        };

        // 两个接口挂同一条路径,规范如此。`name` 放在最后:名字要不到时
        // 连接就不该建起来。
        let conn = builder
            .serve_at(OBJECT_PATH, Root)?
            .serve_at(OBJECT_PATH, Player(shared.clone()))?
            .name(BUS_NAME)?
            .build()?;

        Ok(Mpris { conn, shared })
    }

    /// 根接口:这个播放器是谁,能拿它怎么办。
    struct Root;

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl Root {
        /// 规范要求有,但我们没有「把窗口拉到前面」这条路 —— `CanRaise` 为假,
        /// 规矩的客户端不会调它。
        fn raise(&self) {}

        /// 同上。退出由用户在应用里做,不由 bar 上的按钮做。
        fn quit(&self) {}

        #[zbus(property)]
        fn can_quit(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn can_raise(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn has_track_list(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn identity(&self) -> String {
            "Osmosis".to_owned()
        }

        /// `.desktop` 文件的 stem。规范让报,外壳拿它回头找这个应用的条目。
        ///
        /// 与 `assets/io.github.osmosis.desktop` 的文件名绑死,改一个要同时改
        /// 另一个 —— 报一个不存在的 id,查它的外壳只会查空。
        ///
        /// **别指望它换来媒体卡片上的图标。** 各家外壳拿它做什么各不相同,
        /// 本机这条 DMS bar 压根不看:`Modules/DankBar/Widgets/Media.qml` 里那个
        /// 图标写死是 Material 的 `music_note`,所有播放器一个样;它唯一用到
        /// `desktopEntry` 的地方是排除名单的字符串匹配。GNOME 与 KDE 的媒体
        /// 控件确实按这个 id 取图标,报它是为了那些外壳。
        #[zbus(property)]
        fn desktop_entry(&self) -> String {
            "io.github.osmosis".to_owned()
        }

        #[zbus(property)]
        fn supported_uri_schemes(&self) -> Vec<String> {
            Vec::new()
        }

        #[zbus(property)]
        fn supported_mime_types(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// 播放器接口:正在放什么,以及外面能按什么。
    struct Player(Arc<Shared>);

    #[zbus::interface(
        name = "org.mpris.MediaPlayer2.Player"
    )]
    impl Player {
        fn play_pause(&self) {
            self.0.send(ui::MediaCommand::Toggle);
        }

        /// `Play` 与 `Pause` 原样转发,不在这里判「现在是不是在放」——
        /// 那道判断归 `ui::media`,后端记第二份状态迟早会与界面对不上。
        fn play(&self) {
            self.0.send(ui::MediaCommand::Play);
        }

        fn pause(&self) {
            self.0.send(ui::MediaCommand::Pause);
        }

        /// 没有「停止」这个动作 —— 队列还在,只是不出声。当暂停处理。
        fn stop(&self) {
            self.0.send(ui::MediaCommand::Pause);
        }

        fn next(&self) {
            self.0.send(ui::MediaCommand::Next);
        }

        fn previous(&self) {
            self.0.send(ui::MediaCommand::Previous);
        }

        /// 相对跳转,入参微秒可负。
        fn seek(&self, offset: i64) {
            self.0.send(ui::MediaCommand::SeekBy(
                offset / MICROS_PER_MILLI,
            ));
        }

        /// 绝对跳转。曲目对不上就忽略 —— 规范如此,而且那多半是 bar 上那张
        /// 卡片还停在上一首,照做就是跳错歌。
        fn set_position(
            &self,
            track: ObjectPath<'_>,
            at: i64,
        ) {
            if track.as_str()
                != track_path(&self.0.now().track_id)
            {
                return;
            }
            self.0.send(ui::MediaCommand::SeekTo(
                at / MICROS_PER_MILLI,
            ));
        }

        #[zbus(property)]
        fn playback_status(&self) -> String {
            status_name(self.0.now().status).to_owned()
        }

        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            metadata_of(&self.0.now())
        }

        /// 随机开着没有。可写 —— bar 上那颗随机键按下去走的是这条 setter。
        #[zbus(property)]
        fn shuffle(&self) -> bool {
            self.0.now().shuffle
        }

        #[zbus(property)]
        fn set_shuffle(&self, want: bool) {
            self.0.send(ui::MediaCommand::SetShuffle(want));
        }

        /// 位置每时每刻都在变,规范明说它**不**走 `PropertiesChanged` ——
        /// 对面要么来问,要么自己按倍率外推。
        #[zbus(property(emits_changed_signal = "false"))]
        fn position(&self) -> i64 {
            (self.0.hooks.position)().as_micros() as i64
        }

        // ponytail: 音量恒 1.0 且只读。bar 上那条音量条通常本来就是系统音量,
        // 真要接就给 `MediaHooks` 再加一对读写的线。只读比「写了但没生效」诚实。
        #[zbus(property)]
        fn volume(&self) -> f64 {
            1.0
        }

        // rodio 不变速,三个都恒为 1。
        #[zbus(property)]
        fn rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn minimum_rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn maximum_rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_play(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_pause(&self) -> bool {
            true
        }

        /// 跳转能不能成要到解码线程才知道(见 `docs/adr/0019`),这里只能说
        /// 「按钮该画出来」。跳不动时界面自己会说话。
        #[zbus(property)]
        fn can_seek(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_control(&self) -> bool {
            true
        }
    }

    fn status_name(
        status: ui::MediaStatus,
    ) -> &'static str {
        match status {
            ui::MediaStatus::Playing => "Playing",
            ui::MediaStatus::Paused => "Paused",
            ui::MediaStatus::Stopped => "Stopped",
        }
    }

    /// 曲目 id 变成一条合法的 object path。
    ///
    /// object path 的每一段只认 `[A-Za-z0-9_]`,而平台 id 里 `-`、`.` 都常见。
    /// 不换掉,D-Bus 会拒收**整条** Metadata —— 不是少一个字段,是 bar 上什么
    /// 都不显示。
    fn track_path(id: &str) -> String {
        if id.is_empty() {
            return NO_TRACK.to_owned();
        }

        let mut path = String::with_capacity(
            TRACK_PREFIX.len() + id.len(),
        );
        path.push_str(TRACK_PREFIX);
        for ch in id.chars() {
            path.push(if ch.is_ascii_alphanumeric() {
                ch
            } else {
                '_'
            });
        }
        path
    }

    /// 一首歌的 Metadata。
    fn metadata_of(
        now: &ui::NowPlaying,
    ) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();

        // 路径转不出来就整条不发:一条非法 object path 会让对面丢掉整个 Metadata,
        // 而 `track_path` 的产物按构造必然合法,走到这里说明前提被改坏了。
        let path = track_path(&now.track_id);
        match ObjectPath::try_from(path.clone()) {
            Ok(path) => {
                put(&mut map, "mpris:trackid", path.into())
            }
            Err(err) => {
                log::error!(
                    "曲目路径 {path} 不合法: {err}"
                );
                return map;
            }
        }

        if now.track_id.is_empty() {
            return map;
        }

        put(
            &mut map,
            "xesam:title",
            now.title.clone().into(),
        );
        // 列表原样给出:`xesam:artist` 的类型就是字符串数组,join 成一句
        // 会让外面拿到一个名叫「甲/乙」的人。
        put(
            &mut map,
            "xesam:artist",
            now.artists.clone().into(),
        );
        put(
            &mut map,
            "mpris:length",
            (now.duration_ms * MICROS_PER_MILLI).into(),
        );
        // 没有封面就不写这个键 —— 给空串的话对面会当成一条取不到的图去拉。
        if let Some(url) = &now.art_url {
            put(
                &mut map,
                "mpris:artUrl",
                url.clone().into(),
            );
        }

        map
    }

    /// 往 Metadata 里放一个键。转不成 `OwnedValue` 的就不放:宁可少一个键,
    /// 也不要让整条 Metadata 发不出去。
    fn put(
        map: &mut HashMap<String, OwnedValue>,
        key: &str,
        value: Value<'_>,
    ) {
        match OwnedValue::try_from(value) {
            Ok(value) => {
                map.insert(key.to_owned(), value);
            }
            Err(err) => {
                log::warn!(
                    "Metadata 的 {key} 放不进去: {err}"
                );
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc;

        // publish 是 trait 上的方法,不引进来这里调不动。
        use ui::MediaControls as _;

        use super::*;

        /// 一首用来测的歌。id 里那些字符是重点:平台 id 常带 `-` 与 `.`。
        fn now_playing() -> ui::NowPlaying {
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
            }
        }

        /// 一对能被记下来的 hooks:命令进 channel,位置恒定好断言。
        fn recording_hooks() -> (
            ui::MediaHooks,
            mpsc::Receiver<ui::MediaCommand>,
        ) {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            (
                ui::MediaHooks {
                    command: Arc::new(move |command| {
                        tx.lock()
                            .unwrap_or_else(
                                PoisonError::into_inner,
                            )
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

        /// 曲目 id 要变成一条合法的 object path。
        ///
        /// `mpris:trackid` 的类型是 object path,不是字符串 —— 平台 id 里的 `-`、`.`
        /// 甚至中文会让 D-Bus 拒收**整条** Metadata,于是 bar 上什么都不显示。
        #[test]
        fn a_track_id_becomes_a_valid_object_path() {
            let path = track_path("ne-1962165898.v2");

            assert_eq!(
                path,
                "/io/github/osmosis/track/ne_1962165898_v2"
            );
            // 真正的判据不是长相,是 D-Bus 收不收。
            assert!(ObjectPath::try_from(path).is_ok());
            assert!(
                ObjectPath::try_from(track_path(
                    "尼古喵喵"
                ))
                .is_ok()
            );
        }

        /// 时长以微秒报出。
        ///
        /// seam 那边统一是毫秒,`mpris:length` 是微秒。差这一个千倍,进度条会缩成
        /// 一条几乎不动的线,而且看起来「像是对的」。
        #[test]
        fn metadata_carries_length_in_microseconds() {
            let map = metadata_of(&now_playing());

            let length = i64::try_from(
                map["mpris:length"].try_clone().unwrap(),
            )
            .unwrap();
            assert_eq!(length, 240_000_000);
        }

        /// 艺术家保持列表形态。
        ///
        /// `xesam:artist` 的类型是字符串数组。join 成一句会让外面拿到一个
        /// 名叫「甲/乙」的人。
        #[test]
        fn artists_stay_a_list() {
            let map = metadata_of(&now_playing());

            let artists = Vec::<String>::try_from(
                map["xesam:artist"].try_clone().unwrap(),
            )
            .unwrap();
            assert_eq!(artists, ["一个狼人", "另一个"]);
        }

        /// 没有封面就不写这个键。
        ///
        /// `mpris:artUrl` 给空串比不给更糟:外面会当成一条取不到的图去拉,
        /// 拉失败之后未必回退到占位图。
        #[test]
        fn a_track_without_a_cover_omits_art_url() {
            let mut now = now_playing();
            now.art_url = None;

            let map = metadata_of(&now);

            assert!(!map.contains_key("mpris:artUrl"));
            assert!(map.contains_key("xesam:title"));
        }

        /// 什么都没放时报 NoTrack。
        ///
        /// 规范给了一条专用路径。空 Metadata 与「有一首歌但字段都是空的」在客户端
        /// 那头长得一样,而后者会让 bar 挂着一行空白。
        #[test]
        fn an_empty_now_playing_reports_the_no_track_path()
        {
            let map =
                metadata_of(&ui::NowPlaying::default());

            let id = ObjectPath::try_from(
                map["mpris:trackid"].try_clone().unwrap(),
            )
            .unwrap();
            assert_eq!(id.as_str(), NO_TRACK);
            // 没有歌就别摆歌名与时长的空壳。
            assert!(!map.contains_key("xesam:title"));
            assert!(!map.contains_key("mpris:length"));
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

            let dbus = zbus::blocking::fdo::DBusProxy::new(
                &client,
            )
            .unwrap();
            assert!(
                dbus.name_has_owner(
                    BUS_NAME.try_into().unwrap()
                )
                .unwrap(),
                "名字没占上,外壳扫不到"
            );

            let xml = zbus::blocking::fdo::IntrospectableProxy::builder(&client)
                .destination(BUS_NAME).unwrap()
                .path(OBJECT_PATH).unwrap()
                .build().unwrap()
                .introspect().unwrap();
            assert!(xml.contains(PLAYER_IFACE));
            assert!(
                xml.contains("\"org.mpris.MediaPlayer2\"")
            );

            let player = zbus::blocking::Proxy::new(
                &client,
                BUS_NAME,
                OBJECT_PATH,
                PLAYER_IFACE,
            )
            .unwrap();

            let status: String = player
                .get_property("PlaybackStatus")
                .unwrap();
            assert_eq!(status, "Playing");

            let meta: HashMap<String, OwnedValue> =
                player.get_property("Metadata").unwrap();
            assert_eq!(
                String::try_from(
                    meta["xesam:title"]
                        .try_clone()
                        .unwrap()
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
        }
    }
}
