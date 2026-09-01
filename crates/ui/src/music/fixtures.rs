//! 测试共用的曲目夹具。

use app_core::TrackDto;

pub(super) fn track() -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: "1".to_owned(),
        title: "紅蓮華".to_owned(),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 234_000,
    }
}

/// 指定 id 的一首歌,用来分辨列表里的行。
pub(super) fn track_with_id(id: &str) -> TrackDto {
    TrackDto {
        id: id.to_owned(),
        title: format!("歌 {id}"),
        ..track()
    }
}

/// 一台**收得下 `spawn_local` 的**无头后端。
///
/// 播放侧到处是 `slint::spawn_local(..).expect("event loop must be running")`,
/// 而 `init_no_event_loop` 那一套刻意不给事件循环代理 —— 于是那一行在测试里
/// 直接炸掉。这里在同一套测试后端外面只补上那个代理:
///
/// * `pump` 为假:代理收到的闭包**原地丢掉**。future 排上了队却一次都没被
///   poll,恰好等于真机上「这一轮事件循环还没轮到它」的那一刻 —— 被测函数
///   同步那一段照跑,异步那一段一步都不进来。
/// * `pump` 为真:闭包**就地跑掉**,于是 spawn 出去的协程当场推进到第一个
///   真正的 await。无声卡时整条播放链在那之前就认输了(见 `report::prepare`),
///   所以它会一口气跑完 —— 收尾那一段因此测得到。
///
/// 两处细节:
/// * 别的线程送来的闭包一律丢掉。future 的 waker 会断言自己回到了原线程,
///   在后台线程上就地跑会当场炸。
/// * `new_event_loop_proxy` 第一次被问时装作没有。`set_platform` 正是靠这一问
///   去占那把**进程级**的锁(一个进程只占得了一次),让它扑空,同一个进程里的
///   多条测试才能各装一份自己的后端 —— 否则第二条测试起就装不上了。
#[cfg(not(target_arch = "wasm32"))]
fn init_spawnable_backend(pump: bool) {
    use std::cell::Cell;
    use std::rc::Rc;

    use slint::platform::{
        EventLoopProxy, Platform, WindowAdapter,
    };

    struct Proxy {
        pump: bool,
        home: std::thread::ThreadId,
    }

    impl EventLoopProxy for Proxy {
        fn quit_event_loop(
            &self,
        ) -> Result<(), slint::EventLoopError> {
            Ok(())
        }

        fn invoke_from_event_loop(
            &self,
            event: Box<dyn FnOnce() + Send>,
        ) -> Result<(), slint::EventLoopError> {
            if self.pump
                && std::thread::current().id() == self.home
            {
                event();
            }
            Ok(())
        }
    }

    struct Spawnable {
        inner: i_slint_backend_testing::TestingBackend,
        pump: bool,
        asked: Cell<bool>,
    }

    impl Platform for Spawnable {
        fn create_window_adapter(
            &self,
        ) -> Result<
            Rc<dyn WindowAdapter>,
            slint::PlatformError,
        > {
            self.inner.create_window_adapter()
        }

        fn duration_since_start(
            &self,
        ) -> core::time::Duration {
            self.inner.duration_since_start()
        }

        fn new_event_loop_proxy(
            &self,
        ) -> Option<Box<dyn EventLoopProxy>> {
            // 头一问是 `set_platform` 在探那把进程级的锁,让它扑空。
            if !self.asked.replace(true) {
                return None;
            }
            Some(Box::new(Proxy {
                pump: self.pump,
                home: std::thread::current().id(),
            }))
        }
    }

    // `..Default::default()` 眼下确实补不出东西 —— 但那份选项里还有一个按
    // 构建特性开关的字段(headless 渲染器),特性一开就得靠它补上。
    #[allow(clippy::needless_update)]
    let options =
        i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            threading: false,
            ..Default::default()
        };

    slint::platform::set_platform(Box::new(Spawnable {
        inner: i_slint_backend_testing::TestingBackend::new(
            options,
        ),
        pump,
        asked: Cell::new(false),
    }))
    .expect("这条测试线程上还不该有别的后端");
}

/// 一台无头主窗口,外加接在它上面的一副空 [`Deck`]。
///
/// spawn 出去的活一步都不跑,看到的是被测函数**同步**做完的那一段。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn deck_window()
-> (super::MainWindow, super::Deck) {
    deck_window_with(false)
}

/// 同上,但 spawn 出去的活当场推进 —— 起播失败那条路因此走得到收尾。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn deck_window_pumped()
-> (super::MainWindow, super::Deck) {
    deck_window_with(true)
}

/// 播放器给的是 `Err`:开不出设备本来就是要支持的常态(见 `music::bind`),
/// 而这样就不必在测试机上真占一张声卡。同播那半边照旧要接 —— `Deck` 攥着
/// 它的把手,而那个把手只有 `syncplay::bind` 造得出来;信令连的是
/// `127.0.0.1`,连不上就在后台按自己的节奏重试,不影响这里任何一条断言。
#[cfg(not(target_arch = "wasm32"))]
fn deck_window_with(
    pump: bool,
) -> (super::MainWindow, super::Deck) {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;

    use slint::ComponentHandle as _;

    use super::Deck;

    init_spawnable_backend(pump);
    let ui =
        super::MainWindow::new().expect("建不出主窗口");
    ui.global::<crate::Session>().set_logged_in(true);

    let player = Arc::new(Err(audio::AudioError::Device(
        "测试里没有声卡".to_owned(),
    )));
    let lyrics = super::LyricFeed {
        lines: Rc::new(RefCell::new(Vec::new())),
        generation: Rc::new(Cell::new(0)),
        player: player.clone(),
    };
    let media = Rc::new(crate::media::Bridge::new(
        Box::new(crate::media::NoControls),
        Arc::default(),
    ));

    let deck = Deck {
        playback: Rc::new(RefCell::new(
            app_core::Playback::default(),
        )),
        queue: Rc::new(RefCell::new(
            app_core::Queue::default(),
        )),
        sync: crate::syncplay::detached(&ui),
        media,
        player,
        lyrics,
        cover: super::CoverFeed::default(),
        tracks: Rc::new(RefCell::new(Vec::new())),
        liked: crate::liked::LikedSet::default(),
        editing: crate::playlist::Editing::default(),
        artwork: crate::artwork::Artwork::default(),
        thumbnails: crate::thumbnail::Thumbnails::default(),
        last_daily: Rc::new(Cell::new(None)),
        stream: Rc::new(RefCell::new(None)),
        prefetched: Rc::new(RefCell::new(None)),
        prefetching: Rc::new(Cell::new(false)),
        seeking: Rc::new(RefCell::new(None)),
    };

    (ui, deck)
}
