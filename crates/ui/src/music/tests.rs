use similar_asserts::assert_eq;

use super::fixtures::*;
use super::*;
use crate::viz::CoverUpdate;
use crate::{Shell, Viz};

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

/// 只有推荐分区会戳上「今天拉过了」。
///
/// 这个日期是「进 Music 页要不要替用户拉一次」的唯一判据(见 `daily_is_due`)。
/// 别的分区顺手把它戳上的话,当天的推荐就再也拉不起来了 —— 页面一直空着,
/// 而没有任何东西会报错。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn only_the_daily_section_stamps_the_day() {
    let (ui, deck) = deck_window();
    let weak = ui.as_weak();

    for section in [1, 2, 3] {
        load_section(&weak, &deck, section);
        assert!(
            deck.last_daily.get().is_none(),
            "第 {section} 个分区不该戳「今天拉过推荐了」"
        );
    }

    load_section(&weak, &deck, 0);
    assert_eq!(
        deck.last_daily.get(),
        Some(chrono::Local::now().date_naive()),
        "推荐分区要戳上今天,否则一失败就会每次进页面都重打一次"
    );
}

/// 认不出的编号落回推荐,而不是留在原地什么都不发生。
///
/// 编号是 `musicnav.slint` 里那份列表的下标,两处手工对齐 —— 那边加一项、
/// 这里漏了一个分支时,用户点下去看到的是一片空白。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_unknown_section_falls_back_to_the_daily_one() {
    let (ui, deck) = deck_window();

    load_section(&ui.as_weak(), &deck, 99);

    assert_eq!(
        deck.last_daily.get(),
        Some(chrono::Local::now().date_naive()),
        "认不出的编号该当推荐处理"
    );
}

// ── 起播那一刻的界面([`play_current`])──
//
// 取直链、解码、出声全在 spawn 出去的协程里,测试里那一段不进来。这里钉的是
// 它**同步**做完的那一段:界面在等待的那几百毫秒里长什么样。上一首的残留
// (封面、歌词、点云、极光)若没在这一刻清掉,新歌会顶着旧图放完整首。

/// 带封面的一首歌 —— 起播时那条取封面的支线要靠它才走得到。
#[cfg(not(target_arch = "wasm32"))]
fn track_with_cover(id: &str) -> TrackDto {
    TrackDto {
        cover: Some(format!("https://cdn/{id}.jpg")),
        ..track_with_id(id)
    }
}

/// 点下去那一刻就得看见「加载中」,而不是等网络回来才动。
///
/// `spawn_local` 的 future 要到下一轮事件循环才跑,而列表那一行的加载态是
/// 用户手指底下唯一看得见的反馈 —— 晚一帧就等于没有。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn starting_a_track_shows_it_as_loading_right_away() {
    let (ui, deck) = deck_window();
    let batch =
        vec![track_with_id("a"), track_with_id("b")];
    *deck.tracks.borrow_mut() = batch.clone();
    deck.queue.borrow_mut().replace(batch, 0);
    ui.global::<Player>().set_is_playing(true);

    play_current(&ui, &deck);

    let player = ui.global::<Player>();
    assert!(player.get_now_loading(), "该立刻显示加载中");
    assert!(
        !player.get_is_playing(),
        "旧歌已经停了,这一刻没有任何声音在走"
    );
    assert_eq!(player.get_now_id(), "a");
    assert_eq!(
        player.get_playback_text(),
        describe_playback(&PlaybackState::Loading(
            track_with_id("a")
        ))
        .as_str()
    );

    let rows = player.get_tracks();
    assert!(
        rows.row_data(0).expect("第一行该在").loading,
        "点的那一行该标上加载态"
    );
    assert!(
        !rows.row_data(1).expect("第二行该在").loading,
        "别的行不该跟着一起转圈"
    );
}

/// 换歌那一刻把上一首的残留全部清掉。
///
/// 封面要过一趟网络,常常根本取不到(CDN 会过期)。留着旧的那份,新歌就会
/// 顶着上一首的封面、歌词、点云与极光色放完整首 —— 而没有任何东西会去收它。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn starting_a_track_wipes_what_the_previous_one_left_behind()
 {
    let (ui, deck) = deck_window();
    let batch = vec![track_with_cover("a")];
    *deck.tracks.borrow_mut() = batch.clone();
    deck.queue.borrow_mut().replace(batch, 0);

    // 上一首留下的那一摊。
    deck.media.set_art(Arc::new(pixels(2)));
    deck.lyrics.lines.replace(vec![
        app_core::LyricLineDto {
            start_ms: 0,
            end_ms: 1_000,
            text: "上一首的词".to_owned(),
            translation: None,
        },
    ]);
    ui.global::<Viz>().set_lyric_line("上一首的词".into());
    ui.global::<Viz>().set_lyric_translation("旧译".into());
    ui.global::<Shell>().set_aurora_cover_active(true);

    play_current(&ui, &deck);

    assert!(
        deck.media.art().is_none(),
        "锁屏上挂着上一首的封面,比空着更误导"
    );
    assert!(
        deck.lyrics.lines.borrow().is_empty(),
        "旧歌词配新歌,比没有歌词更误导"
    );
    assert_eq!(ui.global::<Viz>().get_lyric_line(), "");
    assert_eq!(
        ui.global::<Viz>().get_lyric_translation(),
        ""
    );
    assert_eq!(
        ui.global::<Viz>().get_cover_art().size().width,
        0,
        "封面卡该先空着,等新图到了再摆"
    );
    assert!(
        !ui.global::<Shell>().get_aurora_cover_active(),
        "极光该退回主题绿,旧色配新歌一样是误导"
    );
    assert!(
        matches!(
            deck.cover.take(),
            crate::viz::CoverUpdate::Clear
        ),
        "点云该先退回渐变,而不是挂着上一首"
    );
}

/// 队列是空的就什么都不做。
///
/// 队列放完之后再按播放会走到这里(见 `toggle_play`)。不早退的话,界面会
/// 停在一个永远不会结束的「加载中」上,而根本没有歌在装。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_empty_queue_starts_nothing() {
    let (ui, deck) = deck_window();
    ui.global::<Player>().set_now_loading(false);

    play_current(&ui, &deck);

    assert!(
        !ui.global::<Player>().get_now_loading(),
        "没歌可放,不该摆出一个永远转下去的加载态"
    );
    assert_eq!(ui.global::<Player>().get_now_id(), "");
}

/// 起不来的那一首,加载态要收掉,状态行要说出是为什么。
///
/// 无声卡时 `prepare` 当场认输,整条协程一口气跑到收尾。少了那一段,行上的
/// 转圈会一直转下去 —— 而根本没有东西在装,人只能靠重启才发现。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_track_that_fails_to_start_clears_the_loading_state() {
    let (ui, deck) = deck_window_pumped();
    let batch = vec![track_with_id("a")];
    *deck.tracks.borrow_mut() = batch.clone();
    deck.queue.borrow_mut().replace(batch, 0);

    play_current(&ui, &deck);

    let player = ui.global::<Player>();
    assert!(
        !player.get_now_loading(),
        "这一首已经失败了,加载态该收掉"
    );
    assert!(!player.get_is_playing());
    assert_eq!(
        player.get_playback_text(),
        describe_playback(&PlaybackState::Failed(
            "音频设备错误: 测试里没有声卡".to_owned()
        ))
        .as_str(),
        "状态行得说出是为什么起不来"
    );
    assert!(
        !player
            .get_tracks()
            .row_data(0)
            .expect("第一行该在")
            .loading,
        "行上的转圈也该跟着收"
    );
}
