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

use crate::media::{
    Bridge, MediaCommand, MediaStatus, NowPlaying,
    seek_ratio, seek_target, toggles,
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
