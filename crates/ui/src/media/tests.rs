use similar_asserts::assert_eq;

use helpers::*;

/// 测试里用的一首歌。只有 id 变,别的字段跟着 id 走。
mod helpers {
    use std::rc::Rc;
    use std::sync::Arc;

    use app_core::TrackDto;

    use crate::media::NowPlaying;
    use crate::viz::CoverPixels;

    pub fn track(id: &str) -> TrackDto {
        TrackDto {
            platform: "netease".into(),
            id: id.into(),
            title: format!("歌 {id}"),
            alias: None,
            artists: vec!["甲".into(), "乙".into()],
            cover: Some(format!("https://cdn/{id}.jpg")),
            duration_ms: 240_000,
        }
    }

    /// 一张 1×1 的封面。内容无关紧要,有没有才是被测的东西。
    pub fn art() -> Arc<CoverPixels> {
        Arc::new(CoverPixels {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        })
    }

    /// 数一数后端被推了几次,以及最后一次推的是什么。
    #[derive(Default)]
    pub struct Spy {
        pub pushes: std::cell::RefCell<Vec<NowPlaying>>,
    }

    impl crate::media::MediaControls for Rc<Spy> {
        fn publish(&self, now: &NowPlaying) {
            self.pushes.borrow_mut().push(now.clone());
        }
    }
}

use std::rc::Rc;
use std::sync::Arc;

use app_core::{LoopMode, PlaybackState};
use slint::ComponentHandle as _;

use crate::Player;
use crate::media::{
    Bridge, MediaCommand, MediaStatus, NowPlaying, toggles,
};

/// 随机开没开要一并报出去,不然外面那个开关永远是灭的。
#[test]
fn now_playing_carries_the_shuffle_flag() {
    let state = PlaybackState::Playing(track("a"));

    let on = NowPlaying::render(
        &state,
        true,
        true,
        LoopMode::Off,
        None,
    );
    let stopped = NowPlaying::render(
        &PlaybackState::Idle,
        false,
        true,
        LoopMode::Off,
        Some(art()),
    );

    assert!(on.shuffle);
    // 停下来抹掉的是曲目,不是播放器的开关:MPRIS 的 `Shuffle` 挂在
    // Player 接口上,与这一刻装没装着歌无关。
    assert!(stopped.shuffle, "停下来不该把随机也一起抹掉");
    assert_eq!(
        stopped.track_id, "",
        "曲目那半照旧要清干净"
    );
}

/// **只拨了随机也要重新推一次。**
///
/// 指纹不认随机的话,去重会把这次变更整个吃掉 —— 歌没换、放没放也没变,
/// 于是系统控件上那个开关一直停在旧样子。
#[test]
fn toggling_shuffle_pushes_again() {
    let spy = Rc::new(Spy::default());
    let bridge =
        Bridge::new(Box::new(spy.clone()), Arc::default());
    let state = PlaybackState::Playing(track("a"));

    bridge.publish(NowPlaying::render(
        &state,
        true,
        false,
        LoopMode::Off,
        None,
    ));
    bridge.publish(NowPlaying::render(
        &state,
        true,
        true,
        LoopMode::Off,
        None,
    ));

    let pushes = spy.pushes.borrow();
    assert_eq!(
        pushes.len(),
        2,
        "歌没换、放没放也没变,但随机换了 —— 去重不该吃掉它"
    );
    assert!(!pushes[0].shuffle);
    assert!(pushes[1].shuffle);
}

/// 外面给的是绝对值,界面只有一个切换回调 —— 一样就别去动它。
///
/// 与 [`toggles`] 同一道翻译,理由也同一个:让后端自己去猜「现在是不是
/// 随机」,两个后端迟早有一个猜错。
#[test]
fn set_shuffle_only_flips_when_it_differs() {
    use crate::media::flips_shuffle;

    assert!(flips_shuffle(
        MediaCommand::SetShuffle(true),
        false
    ));
    assert!(flips_shuffle(
        MediaCommand::SetShuffle(false),
        true
    ));
    assert!(!flips_shuffle(
        MediaCommand::SetShuffle(true),
        true
    ));
    assert!(!flips_shuffle(
        MediaCommand::SetShuffle(false),
        false
    ));
    // 别的键跟随机无关,一个都不许翻
    assert!(!flips_shuffle(MediaCommand::Next, false));
    assert!(!flips_shuffle(MediaCommand::Toggle, true));
}

/// **只拨了循环也要重新推一次。**
///
/// 与随机同一条:指纹不认它,去重会把这次变更吃掉,锁屏上的循环键
/// 就一直停在旧样子。
#[test]
fn changing_the_loop_mode_pushes_again() {
    let spy = Rc::new(Spy::default());
    let bridge =
        Bridge::new(Box::new(spy.clone()), Arc::default());
    let state = PlaybackState::Playing(track("a"));

    bridge.publish(NowPlaying::render(
        &state,
        true,
        false,
        LoopMode::Off,
        None,
    ));
    bridge.publish(NowPlaying::render(
        &state,
        true,
        false,
        LoopMode::All,
        None,
    ));

    let pushes = spy.pushes.borrow();
    assert_eq!(
        pushes.len(),
        2,
        "歌没换、放没放也没变,但循环换了 —— 去重不该吃掉它"
    );
    assert_eq!(pushes[1].loop_mode, LoopMode::All);
}

/// 外面给的循环是绝对值,与现值一样就不折腾 —— 与随机同一道翻译。
#[test]
fn set_loop_only_changes_when_it_differs() {
    use crate::media::wants_loop;

    assert_eq!(
        wants_loop(
            MediaCommand::SetLoop(LoopMode::All),
            LoopMode::Off
        ),
        Some(LoopMode::All)
    );
    assert_eq!(
        wants_loop(
            MediaCommand::SetLoop(LoopMode::Off),
            LoopMode::Off
        ),
        None,
        "值本来就一样,按下它的人什么都没要求"
    );
    // 别的键跟循环无关。
    assert_eq!(
        wants_loop(MediaCommand::Next, LoopMode::Off),
        None
    );
}

/// 装着一首歌但没在走 = 暂停。
///
/// `PlaybackState` 里没有「暂停」这个态,它记的是装载了哪一首;传输走没走
/// 是另一件事。少了这一问,暂停之后系统控件上仍是播放中。
#[test]
fn a_loaded_but_paused_track_reports_paused() {
    let state = PlaybackState::Playing(track("a"));

    let running = NowPlaying::render(
        &state,
        true,
        false,
        LoopMode::Off,
        None,
    );
    let paused = NowPlaying::render(
        &state,
        false,
        false,
        LoopMode::Off,
        None,
    );

    assert_eq!(running.status, MediaStatus::Playing);
    assert_eq!(paused.status, MediaStatus::Paused);
    // 暂停的仍是那一首,曲目信息不该跟着状态一起没了。
    assert_eq!(paused.track_id, "a");
}

/// 空闲与失败都报 Stopped。
///
/// 失败不是一种播放状态 —— 报成 Paused 会让外面显示一个「按一下就能继续」
/// 的假象,而那首歌根本没装起来。
#[test]
fn an_idle_or_failed_deck_reports_stopped() {
    let idle = NowPlaying::render(
        &PlaybackState::Idle,
        false,
        false,
        LoopMode::Off,
        None,
    );
    let failed = NowPlaying::render(
        &PlaybackState::Failed("上游超时".into()),
        // 失败那一刻界面可能还没来得及把 is-playing 抹掉。
        true,
        false,
        LoopMode::Off,
        None,
    );

    assert_eq!(idle.status, MediaStatus::Stopped);
    assert_eq!(failed.status, MediaStatus::Stopped);
}

/// 停下来时不留着上一首。
///
/// 队列放完了,控件上却还挂着最后那首歌的名字和封面 —— 这是「投影过期」
/// 的老毛病换了个地方犯(见 `crate::notice` 的模块注释)。
#[test]
fn a_stopped_deck_carries_no_track() {
    let stopped = NowPlaying::render(
        &PlaybackState::Idle,
        false,
        false,
        LoopMode::Off, // 上一首的封面还攥在手里,也不该被带出去。
        Some(art()),
    );

    assert_eq!(stopped.track_id, "");
    assert_eq!(stopped.title, "");
    assert!(stopped.artists.is_empty());
    assert_eq!(stopped.duration_ms, 0);
    assert!(stopped.art_url.is_none());
    assert!(stopped.art.is_none());
}

/// 同一份快照不推第二次。
///
/// 推送搭的是 1Hz 的续播轮询。不去重的话,一首四分钟的歌会往外发两百多次
/// 状态变更,而内容一个字都没变。
#[test]
fn the_same_snapshot_is_not_published_twice() {
    let spy = Rc::new(Spy::default());
    let bridge =
        Bridge::new(Box::new(spy.clone()), Arc::default());
    let state = PlaybackState::Playing(track("a"));

    for _ in 0..5 {
        bridge.publish(NowPlaying::render(
            &state,
            true,
            false,
            LoopMode::Off,
            None,
        ));
    }

    assert_eq!(spy.pushes.borrow().len(), 1);
}

/// 封面晚到了要再推一次。
///
/// 换歌那一刻封面还在路上(`play_current` 里那个 `spawn_local`),推出去的第一
/// 份必然没有图。若指纹只认歌的 id,图到了也推不出去,控件上就永远是空封面。
#[test]
fn a_late_cover_is_published_again() {
    let spy = Rc::new(Spy::default());
    let bridge =
        Bridge::new(Box::new(spy.clone()), Arc::default());
    let state = PlaybackState::Playing(track("a"));

    bridge.publish(NowPlaying::render(
        &state,
        true,
        false,
        LoopMode::Off,
        None,
    ));
    bridge.publish(NowPlaying::render(
        &state,
        true,
        false,
        LoopMode::Off,
        Some(art()),
    ));

    let pushes = spy.pushes.borrow();
    assert_eq!(pushes.len(), 2);
    assert!(pushes[0].art.is_none());
    assert!(pushes[1].art.is_some());
}

/// 已经在放时再按播放,什么都不该发生。
///
/// 界面只有一个「切换」回调,而外面给的是 `Play`/`Pause`/`PlayPause` 三个键。
/// 把 `Play` 一律当成切换,正在放的歌会被按停 —— 锁屏上最容易误触的就是它。
#[test]
fn play_while_already_playing_changes_nothing() {
    assert!(!toggles(MediaCommand::Play, true));
    assert!(toggles(MediaCommand::Play, false));

    assert!(toggles(MediaCommand::Pause, true));
    assert!(!toggles(MediaCommand::Pause, false));

    // 切换键名副其实,两种情形下都翻。
    assert!(toggles(MediaCommand::Toggle, true));
    assert!(toggles(MediaCommand::Toggle, false));
}

// ── 路由表:哪个键落到 `.slint` 的哪个回调(`super::dispatch`)──
//
// 判定本身(翻不翻、跳到哪)在 `rules.rs` 已经各有断言,`dispatch` 剩下的
// 全部职责就是这张表。接错一根线不会报错,只会让锁屏上的「下一首」去暂停。

use routing::*;

mod routing {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;

    use slint::ComponentHandle as _;

    use crate::{MainWindow, Player};

    /// 一台开不出声的播放器。
    ///
    /// `dispatch` 只在跳转那一支问它「现在放到哪」,而 `Err` 那条路取默认值 0 ——
    /// 于是相对跳转的起点是确定的,不必真开设备。
    pub fn deaf_player()
    -> Arc<Result<audio::Player, audio::AudioError>> {
        Arc::new(Err(audio::AudioError::Device(
            "测试里没有声卡".to_owned(),
        )))
    }

    /// 一台无头主窗口,外加数一数那几个回调各被喊了几次、带了什么参数。
    pub struct Routed {
        pub ui: MainWindow,
        pub next: Rc<Cell<u32>>,
        pub prev: Rc<Cell<u32>>,
        pub toggle: Rc<Cell<u32>>,
        pub shuffle: Rc<Cell<u32>>,
        pub loop_set: Rc<RefCell<Vec<i32>>>,
        pub seek: Rc<RefCell<Vec<f32>>>,
    }

    impl Routed {
        pub fn new() -> Self {
            i_slint_backend_testing::init_no_event_loop();
            let ui =
                MainWindow::new().expect("建不出主窗口");

            let next = Rc::new(Cell::new(0));
            let prev = Rc::new(Cell::new(0));
            let toggle = Rc::new(Cell::new(0));
            let shuffle = Rc::new(Cell::new(0));
            let loop_set =
                Rc::new(RefCell::new(Vec::new()));
            let seek = Rc::new(RefCell::new(Vec::new()));

            let player = ui.global::<Player>();
            player.on_next_track({
                let hits = next.clone();
                move || hits.set(hits.get() + 1)
            });
            player.on_prev_track({
                let hits = prev.clone();
                move || hits.set(hits.get() + 1)
            });
            player.on_toggle_play({
                let hits = toggle.clone();
                move || hits.set(hits.get() + 1)
            });
            player.on_shuffle_toggled({
                let hits = shuffle.clone();
                move || hits.set(hits.get() + 1)
            });
            player.on_loop_mode_set({
                let asked = loop_set.clone();
                move |index| asked.borrow_mut().push(index)
            });
            player.on_seek({
                let asked = seek.clone();
                move |ratio| asked.borrow_mut().push(ratio)
            });

            Self {
                ui,
                next,
                prev,
                toggle,
                shuffle,
                loop_set,
                seek,
            }
        }

        /// 按下一个键。时长单独给:它平时由 `Bridge::publish` 写进那个原子。
        pub fn press(
            &self,
            command: crate::media::MediaCommand,
            duration_ms: i64,
        ) {
            super::super::dispatch(
                &self.ui,
                &deaf_player(),
                &std::sync::atomic::AtomicI64::new(
                    duration_ms,
                ),
                command,
            );
        }
    }
}

/// 上一首与下一首各走各的回调。
///
/// 两个键长得最像,接反了在界面上毫无痕迹 —— 锁屏按「下一首」听到的是上一首。
#[test]
fn next_and_previous_reach_their_own_callbacks() {
    let routed = Routed::new();

    routed.press(MediaCommand::Next, 0);
    assert_eq!(routed.next.get(), 1);
    assert_eq!(
        routed.prev.get(),
        0,
        "按下一首不该同时惊动上一首"
    );

    routed.press(MediaCommand::Previous, 0);
    assert_eq!(routed.prev.get(), 1);
    assert_eq!(routed.next.get(), 1);
}

/// 正在放时按 `Play` 一声不吭,按 `Pause` 才落到切换回调上。
///
/// 界面只有一个切换回调,而 MPRIS 给的是三个键。`Play` 一律当切换的话,
/// 锁屏上误触播放会把正在放的歌按停。
#[test]
fn play_and_pause_only_reach_the_toggle_when_they_change_something()
 {
    let routed = Routed::new();
    routed.ui.global::<Player>().set_is_playing(true);

    routed.press(MediaCommand::Play, 0);
    assert_eq!(
        routed.toggle.get(),
        0,
        "已经在放了,`Play` 不该按停它"
    );

    routed.press(MediaCommand::Pause, 0);
    assert_eq!(routed.toggle.get(), 1);

    // 切换键名副其实,状态是什么都翻。
    routed.press(MediaCommand::Toggle, 0);
    assert_eq!(routed.toggle.get(), 2);
}

/// 随机拨到它已经在的那个值 = 什么都没要求,不该喊那个切换回调。
///
/// 外面给的是绝对值,而界面只有一个切换回调 —— 不比一下就调,开关会翻到反面。
#[test]
fn set_shuffle_only_reaches_the_toggle_when_it_differs() {
    let routed = Routed::new();
    routed.ui.global::<Player>().set_shuffle_on(false);

    routed.press(MediaCommand::SetShuffle(false), 0);
    assert_eq!(
        routed.shuffle.get(),
        0,
        "本来就是关的,再拨一次「关」不该把它打开"
    );

    routed.press(MediaCommand::SetShuffle(true), 0);
    assert_eq!(routed.shuffle.get(), 1);
}

/// 循环拨到别的态时,喊出去的是 `.slint` 那侧的**编号**,不是枚举。
///
/// 编号是 seam 的形状(0 关 / 1 列表 / 2 单曲)。这里翻错一位不会报错,
/// 只会让锁屏上按「列表循环」变成单曲循环。
#[test]
fn set_loop_hands_over_the_index_the_slint_side_speaks() {
    let routed = Routed::new();
    routed.ui.global::<Player>().set_loop_mode(0);

    routed.press(MediaCommand::SetLoop(LoopMode::Off), 0);
    assert!(
        routed.loop_set.borrow().is_empty(),
        "本来就是关的,不该再拨一次"
    );

    routed.press(MediaCommand::SetLoop(LoopMode::One), 0);
    assert_eq!(
        routed.loop_set.borrow().as_slice(),
        [2],
        "单曲循环在 slint 那侧是 2"
    );
}

/// 跳转:绝对位置换成比例才喊得出去,而时长为 0 时这一跳整个丢掉。
///
/// `.slint` 的 `seek` 收的是 0..=1 的比例。时长还没到手就除,得到的是
/// 无穷大或 NaN —— 进度条会跳到条外,而没有任何东西会报错。
#[test]
fn a_seek_without_a_duration_is_dropped() {
    let routed = Routed::new();

    routed.press(MediaCommand::SeekTo(60_000), 0);
    assert!(
        routed.seek.borrow().is_empty(),
        "时长未知时这一跳该被丢掉,而不是除零"
    );

    routed.press(MediaCommand::SeekTo(60_000), 240_000);
    assert_eq!(routed.seek.borrow().as_slice(), [0.25]);
}

/// 相对跳转从「现在放到哪」起算;没有播放器时那个起点是 0。
///
/// 无声卡是常态(见 `music::bind`),那时按 MPRIS 的 `Seek` 不该 panic,
/// 也不该把一个来路不明的位置当起点。
#[test]
fn a_relative_seek_starts_from_the_players_position() {
    let routed = Routed::new();

    routed.press(MediaCommand::SeekBy(60_000), 240_000);
    assert_eq!(routed.seek.borrow().as_slice(), [0.25]);

    // 往回跳过了头落到开头,而不是负比例。
    routed.press(MediaCommand::SeekBy(-60_000), 240_000);
    assert_eq!(
        routed.seek.borrow().as_slice(),
        [0.25, 0.0],
        "往回跳过头该停在开头"
    );
}
