use similar_asserts::assert_eq;

use super::fixtures::*;
use super::*;
use crate::viz::CoverUpdate;

/// 封面像素只交出一次:换歌那一帧给一个动作,之后一直是"没消息"。
/// 一张封面是兆级的字节,每帧搬一次过 seam 纯属白耗。
#[test]
fn cover_feed_hands_pixels_over_once_per_track() {
    let feed = CoverFeed::default();
    assert!(
        matches!(feed.take(), CoverUpdate::Unchanged),
        "没换歌不该有动作"
    );

    feed.replace(Arc::new(pixels(2)));
    assert!(
        matches!(feed.take(), CoverUpdate::Show(p) if p.width == 2)
    );
    assert!(
        matches!(feed.take(), CoverUpdate::Unchanged),
        "同一张被交出了两次"
    );
}

/// 上一张还没被取走就又换歌:取到的是新的那张。点云只显示当前这一首,
/// 过期的封面排队也没人要 —— 播放页收起时门是关的,没人来取,连着换几首
/// 就会攒下一串。
#[test]
fn cover_feed_replaces_a_pending_cover() {
    let feed = CoverFeed::default();
    feed.replace(Arc::new(pixels(2)));
    feed.replace(Arc::new(pixels(4)));
    assert!(
        matches!(feed.take(), CoverUpdate::Show(p) if p.width == 4)
    );
    assert!(matches!(feed.take(), CoverUpdate::Unchanged));
}

/// **换歌当场就要清,不等新封面。**
///
/// 这是那个 bug 的回归测试:取封面要几百毫秒,而且常常根本取不到
/// (CDN 会过期、有的歌压根没有封面)。只在成功时换图的话,点云会挂着
/// 上一首的封面 —— 少则几百毫秒,多则一直到下次换歌
/// (见 `CONTEXT.md`「封面点云」)。
#[test]
fn cover_feed_clears_before_the_new_art_arrives() {
    let feed = CoverFeed::default();
    feed.replace(Arc::new(pixels(2)));
    // 上一首的图还排在队里没人取,这时候用户按了下一首。
    feed.clear();

    assert!(
        matches!(feed.take(), CoverUpdate::Clear),
        "换歌那一帧该是清空,而不是把上一首的图交出去"
    );
    assert!(matches!(feed.take(), CoverUpdate::Unchanged));
}

/// 边长 `side` 的纯色封面像素,只用来分辨是哪一张。
fn pixels(side: u32) -> crate::viz::CoverPixels {
    crate::viz::CoverPixels {
        width: side,
        height: side,
        rgba: vec![0; (side * side * 4) as usize],
    }
}

/// 出声那一刻报一次,之后放着的每一秒都不再报。
///
/// 轮询每秒经过这里一次,不去重的话一首三分钟的歌会报出一百八十次播放。
#[test]
fn a_start_is_reported_once_and_not_every_tick() {
    let mut last = None;
    assert_eq!(
        play_to_report(
            &PlaybackState::Loading(track_with_id("1")),
            &mut last
        ),
        None,
        "还在取流,不算一次播放"
    );

    let started = play_to_report(
        &PlaybackState::Playing(track_with_id("1")),
        &mut last,
    );
    assert_eq!(
        started,
        Some(("netease".to_owned(), "1".to_owned())),
        "出声了就报,身份是 (平台, 平台内 id)"
    );

    assert_eq!(
        play_to_report(
            &PlaybackState::Playing(track_with_id("1")),
            &mut last
        ),
        None,
        "同一首还在放,不重复报"
    );
}

/// 换一首就再报一次。
#[test]
fn each_track_is_reported_on_its_own() {
    let mut last = None;
    play_to_report(
        &PlaybackState::Playing(track_with_id("1")),
        &mut last,
    );
    assert_eq!(
        play_to_report(
            &PlaybackState::Playing(track_with_id("2")),
            &mut last
        ),
        Some(("netease".to_owned(), "2".to_owned()))
    );
}

/// 重放同一首要能再报一次:重新点会先经过 Loading,记忆在那时清掉。
///
/// 不清的话「单曲循环」整晚只记一次播放,而它确实放了一整晚。
#[test]
fn replaying_the_same_track_is_a_second_play() {
    let mut last = None;
    play_to_report(
        &PlaybackState::Playing(track_with_id("1")),
        &mut last,
    );
    play_to_report(
        &PlaybackState::Loading(track_with_id("1")),
        &mut last,
    );
    assert_eq!(
        play_to_report(
            &PlaybackState::Playing(track_with_id("1")),
            &mut last
        ),
        Some(("netease".to_owned(), "1".to_owned()))
    );
}

/// 没放成的不算播放:取流失败停在 Failed,报出去就是假数字。
#[test]
fn a_failed_start_is_not_a_play() {
    let mut last = None;
    assert_eq!(
        play_to_report(
            &PlaybackState::Failed("取流失败".into()),
            &mut last
        ),
        None
    );
    assert_eq!(
        play_to_report(&PlaybackState::Idle, &mut last),
        None
    );
}

/// **备好的那一份只认它自己那一首。**
///
/// 备下一首的时候用户可能改主意:点了列表里别的歌,或者洗了牌。认错了会放出
/// 一首根本没点过的歌 —— 而且界面显示的还是对的那一首,查起来极其别扭。
#[test]
fn a_prefetched_track_is_only_used_for_its_own_track() {
    let slot =
        RefCell::new(Some(("2".to_owned(), "备好的源")));

    assert!(
        take_prefetched(&slot, "7").is_none(),
        "id 对不上,不许拿来用"
    );
    assert!(
        slot.borrow().is_none(),
        "对不上的那一份要就地丢掉,不能留着占一条下载"
    );

    let slot =
        RefCell::new(Some(("2".to_owned(), "备好的源")));
    assert_eq!(
        take_prefetched(&slot, "2"),
        Some("备好的源")
    );
    assert!(
        take_prefetched(&slot, "2").is_none(),
        "同一份不该被交出两次"
    );
}

/// 四个编号各自认出自己的分区,认不出的落回每日推荐 ——
/// 那是开局那一页,总比留在原地什么都不发生强。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn each_section_knows_what_to_load() {
    use super::Section;

    assert_eq!(Section::from_index(0), Section::Daily);
    assert_eq!(Section::from_index(1), Section::Playlists);
    assert_eq!(Section::from_index(2), Section::Search);
    assert_eq!(Section::from_index(3), Section::Recent);
    assert_eq!(Section::from_index(99), Section::Daily);
    assert_eq!(Section::from_index(-1), Section::Daily);
}
