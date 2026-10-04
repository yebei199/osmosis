use similar_asserts::assert_eq;
use std::cell::Cell;

mod dispatch;
#[cfg(not(target_arch = "wasm32"))]
mod downloads;
mod group;
mod views;

use super::fixtures::*;
use super::*;
use crate::viz::CoverUpdate;
use crate::{Shell, Viz};

/// 封面像素只交出一次:换歌那一帧给一个动作,之后一直是"没消息"。
/// 一张封面是兆级的字节,每帧搬一次过 seam 纯属白耗。
#[test]
fn cover_feed_hands_pixels_over_once_per_track() {
    let ui = cover_window();
    let feed = CoverFeed::default();
    assert!(
        matches!(feed.take(), CoverUpdate::Unchanged),
        "没换歌不该有动作"
    );

    feed.replace(&ui, Arc::new(pixels(2)));
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
    let ui = cover_window();
    let feed = CoverFeed::default();
    feed.replace(&ui, Arc::new(pixels(2)));
    feed.replace(&ui, Arc::new(pixels(4)));
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
    let ui = cover_window();
    let feed = CoverFeed::default();
    feed.replace(&ui, Arc::new(pixels(2)));
    // 上一首的图还排在队里没人取,这时候用户按了下一首。
    feed.clear(&ui);

    assert!(
        matches!(feed.take(), CoverUpdate::Clear),
        "换歌那一帧该是清空,而不是把上一首的图交出去"
    );
    assert!(matches!(feed.take(), CoverUpdate::Unchanged));
}

/// 封面一排上队就叫醒渲染循环(#153):循环可能正定格着,没人叫的话换歌
/// 之后点云要等到下一次触摸才换图。清空与新图两条路都要叫。
#[test]
fn a_cover_arrival_wakes_the_render_loop() {
    let ui = cover_window();
    let settles = Rc::new(Cell::new(0));
    {
        let settles = settles.clone();
        ui.global::<Shell>().on_settle(move || {
            settles.set(settles.get() + 1)
        });
    }
    let feed = CoverFeed::default();

    feed.clear(&ui);
    assert_eq!(settles.get(), 1, "换歌清空时没叫醒");
    assert!(feed.pending());

    feed.replace(&ui, Arc::new(pixels(2)));
    assert_eq!(settles.get(), 2, "新封面到了没叫醒");

    let _ = feed.take();
    assert!(!feed.pending(), "取走之后不该还挂着");
}

/// 封面测试用的无头窗口:叫醒要经它的 `Shell` 回调。
fn cover_window() -> MainWindow {
    i_slint_backend_testing::init_no_event_loop();
    MainWindow::new().expect("建不出主窗口")
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
    // 经 show 摆上去:列表的行由分组状态投影(#160),只写 deck.tracks 不上屏
    show(
        &ui,
        &deck,
        TracksDto {
            tracks: batch.clone(),
            unavailable: 0,
            hidden: 0,
        },
    );
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
    // 经 show 摆上去:列表的行由分组状态投影(#160),只写 deck.tracks 不上屏
    show(
        &ui,
        &deck,
        TracksDto {
            tracks: batch.clone(),
            unavailable: 0,
            hidden: 0,
        },
    );
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
    // 经 show 摆上去:列表的行由分组状态投影(#160),只写 deck.tracks 不上屏
    show(
        &ui,
        &deck,
        TracksDto {
            tracks: batch.clone(),
            unavailable: 0,
            hidden: 0,
        },
    );
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

/// 起播时标加载态只改那一行,不整表重建(#137 ⑥)。
///
/// 整表重建一次要把近千行重新格式化、重标红心、重摆封面,而起播一首要建两次
/// (标上、收掉)。模型换掉还会让行元素整批重建 —— 手指底下那一行正被按着时,
/// 那一下点击就没了。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn marking_the_loading_row_keeps_the_list_model() {
    let (ui, deck) = deck_window();
    show(&ui, &deck, batch(&["a", "b", "c"]));
    let model = ui.global::<Player>().get_tracks();
    let queued = deck.tracks.borrow().clone();
    deck.queue.borrow_mut().replace(queued, 1);

    play_current(&ui, &deck);

    let now = ui.global::<Player>().get_tracks();
    assert!(now == model, "标加载态把整张列表的模型换掉了");
    assert!(now.row_data(1).expect("第二行该在").loading);
    assert!(!now.row_data(0).expect("第一行该在").loading);
}

/// 收加载态同样只改那一行。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn clearing_the_loading_row_keeps_the_list_model() {
    let (ui, deck) = deck_window_pumped();
    show(&ui, &deck, batch(&["a", "b", "c"]));
    let model = ui.global::<Player>().get_tracks();
    let queued = deck.tracks.borrow().clone();
    deck.queue.borrow_mut().replace(queued, 1);

    // 测试里没有声卡,起播当场失败,标上与收掉在这一步里都走完
    play_current(&ui, &deck);

    let now = ui.global::<Player>().get_tracks();
    assert!(now == model, "收加载态把整张列表的模型换掉了");
    assert!(!now.row_data(1).expect("第二行该在").loading);
}

// ── 先画缓存里上次那份,新的回来再换(#123)──
//
// 协程在 pumped 窗口里当场跑完,网络那个 future 被 poll 的那一刻,就是
// 「缓存已经摆上、网络还没回来」的那一刻 —— 探针就插在那里。

#[cfg(not(target_arch = "wasm32"))]
fn batch(ids: &[&str]) -> TracksDto {
    TracksDto {
        tracks: ids
            .iter()
            .map(|id| track_with_id(id))
            .collect(),
        unavailable: 0,
        hidden: 0,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn shown_ids(ui: &MainWindow) -> Vec<String> {
    ui.global::<Player>()
        .get_tracks()
        .iter()
        .map(|row| row.id.to_string())
        .collect()
}

/// 网络还没回来时列表里已经是上次那份,回来之后换成新的。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_cached_list_is_shown_before_the_network_answers() {
    let (ui, deck) = deck_window_pumped();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let probe = seen.clone();
    let weak = ui.as_weak();

    fetch_cached_into(
        &ui.as_weak(),
        &deck,
        crate::runtime::trace::Action::begin("test-cached"),
        ViewSource::Daily,
        async { Some(batch(&["a"])) },
        async move {
            if let Some(ui) = weak.upgrade() {
                *probe.borrow_mut() = shown_ids(&ui);
            }
            Ok(batch(&["a", "b"]))
        },
    );

    assert_eq!(
        *seen.borrow(),
        vec!["a".to_owned()],
        "等网络的那段时间里,该摆着缓存里上次那份"
    );
    assert_eq!(
        shown_ids(&ui),
        vec!["a".to_owned(), "b".to_owned()],
        "新的回来之后换成新的"
    );
}

/// 新的与上次那份一样就不重建模型:整表重建会把封面与加载态刷一遍。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_unchanged_answer_keeps_the_shown_model() {
    let (ui, deck) = deck_window_pumped();
    let seen = Rc::new(RefCell::new(None));
    let probe = seen.clone();
    let weak = ui.as_weak();

    fetch_cached_into(
        &ui.as_weak(),
        &deck,
        crate::runtime::trace::Action::begin(
            "test-unchanged",
        ),
        ViewSource::Daily,
        async { Some(batch(&["a"])) },
        async move {
            if let Some(ui) = weak.upgrade() {
                *probe.borrow_mut() = Some(
                    ui.global::<Player>().get_tracks(),
                );
            }
            Ok(batch(&["a"]))
        },
    );

    let cached_model =
        seen.borrow().clone().expect("网络那一步没被走到");
    assert!(
        ui.global::<Player>().get_tracks() == cached_model,
        "内容没变,摆着的还该是缓存那一份模型"
    );
}

/// 断网:刷新失败,缓存里那份留在列表里。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_failed_refresh_keeps_the_cached_list() {
    let (ui, deck) = deck_window_pumped();

    fetch_cached_into(
        &ui.as_weak(),
        &deck,
        crate::runtime::trace::Action::begin(
            "test-offline",
        ),
        ViewSource::Daily,
        async { Some(batch(&["a"])) },
        async {
            Err(api::ApiError::Transport(
                "断网了".to_owned(),
            ))
        },
    );

    assert_eq!(
        shown_ids(&ui),
        vec!["a".to_owned()],
        "取不到新的,上次那份得还在"
    );
}

// ── 分组条与筛选(#160)──

/// 一批带歌手与标签的歌:a 甲+乙、b 甲、c 丙(标签「夜」)。
#[cfg(not(target_arch = "wasm32"))]
fn faceted_batch() -> TracksDto {
    let mut a = track_with_id("a");
    a.artists = vec!["甲".into(), "乙".into()];
    let mut b = track_with_id("b");
    b.artists = vec!["甲".into()];
    let mut c = track_with_id("c");
    c.artists = vec!["丙".into()];
    c.facets.tags = vec!["夜".into()];
    TracksDto {
        tracks: vec![a, b, c],
        unavailable: 0,
        hidden: 0,
    }
}

/// 摆进一个视图,并接上分组条。
#[cfg(not(target_arch = "wasm32"))]
fn faceted_window(
    source: ViewSource,
) -> (MainWindow, Deck) {
    let (ui, deck) = deck_window();
    super::facets::bind(&ui, &deck);
    let (_ticket, shown) = deck.views.begin(source);
    project(&ui, &deck, shown);
    show(&ui, &deck, faceted_batch());
    (ui, deck)
}

#[cfg(not(target_arch = "wasm32"))]
fn headers(ui: &MainWindow) -> Vec<String> {
    ui.global::<Player>()
        .get_tracks()
        .iter()
        .filter(|row| row.header)
        .map(|row| {
            format!("{} {}", row.title, row.duration)
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn queue_ids(deck: &Deck) -> Vec<String> {
    deck.tracks
        .borrow()
        .iter()
        .map(|track| track.id.clone())
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn chip_index(ui: &MainWindow, text: &str) -> i32 {
    ui.global::<Player>()
        .get_chips()
        .iter()
        .position(|chip| chip.text == text)
        .map(|at| at as i32)
        .unwrap_or_else(|| {
            panic!("没有「{text}」这个 chip")
        })
}

/// 按歌手分:堆数等于歌手去重数,多歌手的歌进每一堆,队列里只排一次。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn grouping_by_artist_makes_one_pile_per_artist() {
    let (ui, deck) = faceted_window(ViewSource::Daily);
    assert!(ui.global::<Player>().get_facets_enabled());

    ui.global::<Player>().invoke_set_grouping(
        app_core::facets::Facet::Artist.index(),
    );

    assert_eq!(
        headers(&ui),
        vec!["甲 2 首", "丙 1 首", "乙 1 首"],
        "三位歌手就是三堆,按堆大小排"
    );
    assert_eq!(ui.global::<Player>().get_pile_count(), 3);
    assert_eq!(queue_ids(&deck), vec!["a", "b", "c"]);
}

/// 选了分组就把卡墙切回列表(用户 2026-09-27 定)。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn choosing_a_grouping_leaves_the_wall() {
    let (ui, _deck) = faceted_window(ViewSource::Daily);
    let asked = Rc::new(RefCell::new(Vec::new()));
    let probe = asked.clone();
    ui.global::<Shell>().on_set_view_wall(move |to| {
        probe.borrow_mut().push(to)
    });

    ui.global::<Player>().invoke_set_grouping(0);
    assert!(asked.borrow().is_empty(), "不分组不碰卡墙");

    ui.global::<Player>().invoke_set_grouping(
        app_core::facets::Facet::Tag.index(),
    );
    assert_eq!(*asked.borrow(), vec![false]);
}

/// chip 筛掉的歌既不在列表上、也不进队列;换了视图,选中的 chip 清掉。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_chip_filters_the_rows_and_the_queue() {
    let (ui, deck) = faceted_window(ViewSource::Daily);

    ui.global::<Player>()
        .invoke_toggle_chip(chip_index(&ui, "夜 1"));

    assert_eq!(shown_ids(&ui), vec!["c"]);
    assert_eq!(queue_ids(&deck), vec!["c"]);
    assert_eq!(ui.global::<Player>().get_chosen_count(), 1);

    let (_ticket, shown) =
        deck.views.begin(ViewSource::Recent);
    project(&ui, &deck, shown);
    show(&ui, &deck, faceted_batch());
    assert_eq!(ui.global::<Player>().get_chosen_count(), 0);
    assert_eq!(shown_ids(&ui), vec!["a", "b", "c"]);
}

/// 折起来的堆只剩堆头;再点一次展开。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_collapsed_pile_keeps_only_its_header() {
    let (ui, _deck) = faceted_window(ViewSource::Daily);
    ui.global::<Player>().invoke_set_grouping(
        app_core::facets::Facet::Artist.index(),
    );

    ui.global::<Player>().invoke_toggle_pile("甲".into());
    assert_eq!(shown_ids(&ui), vec!["", "", "c", "", "a"]);

    ui.global::<Player>().invoke_toggle_pile("甲".into());
    assert_eq!(
        shown_ids(&ui),
        vec!["", "a", "b", "", "c", "", "a"]
    );
}

/// 搜索结果不是歌单:不挂分组条,选着的分组也不作用在它上面。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn search_results_are_not_grouped() {
    let (ui, deck) = faceted_window(ViewSource::Daily);
    ui.global::<Player>().invoke_set_grouping(
        app_core::facets::Facet::Artist.index(),
    );

    let (_ticket, shown) =
        deck.views.begin(ViewSource::Search("x".into()));
    project(&ui, &deck, shown);
    show(&ui, &deck, faceted_batch());

    assert!(!ui.global::<Player>().get_facets_enabled());
    assert_eq!(shown_ids(&ui), vec!["a", "b", "c"]);
}

/// 电台在放,摆着 `source` 这个视图,选「夜」再点其中一首。返回电台还在不在放、带什么筛选。
#[cfg(not(target_arch = "wasm32"))]
fn pick_filtered_on(
    source: ViewSource,
) -> Option<Vec<app_core::FacetPickDto>> {
    let (ui, deck) = faceted_window(source);
    bind_play(&ui, &deck);
    super::radio::begin(
        &ui,
        &deck,
        api::RadioMode::Fm,
        faceted_batch().tracks,
    );
    assert!(super::radio::taste(&deck).is_some());

    ui.global::<Player>()
        .invoke_toggle_chip(chip_index(&ui, "夜 1"));
    ui.global::<Player>().invoke_play("c".into());
    super::radio::taste(&deck)
}

/// 电台区里选了 chip 再点歌(#166):换出来的那一批仍是电台的,选着的筛选记下来续歌用。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn picking_on_the_radio_list_keeps_the_radio_with_its_filter()
 {
    assert_eq!(
        pick_filtered_on(ViewSource::Radio),
        Some(vec![app_core::FacetPickDto {
            facet: app_core::FacetDto::Tag,
            label: "夜".to_owned(),
        }])
    );
}

/// 点的正是在放的那首(连点去重挡掉,没换批):电台仍在放这一批,筛选照样记下。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_deduplicated_pick_on_the_radio_list_still_takes_the_filter()
 {
    let (ui, deck) = faceted_window(ViewSource::Radio);
    super::radio::begin(
        &ui,
        &deck,
        api::RadioMode::Fm,
        faceted_batch().tracks,
    );
    ui.global::<Player>()
        .invoke_toggle_chip(chip_index(&ui, "夜 1"));

    let batch = deck.queue.borrow().batch();
    super::radio::adopt(&deck, batch);

    assert_eq!(
        super::radio::taste(&deck),
        Some(vec![app_core::FacetPickDto {
            facet: app_core::FacetDto::Tag,
            label: "夜".to_owned(),
        }])
    );
}

/// 在别的视图点歌,电台照旧让位。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn picking_elsewhere_still_stops_the_radio() {
    assert_eq!(pick_filtered_on(ViewSource::Daily), None);
}

/// 电台区也是歌单视图(主路由确认):挂分组条,分组作用在它上面。
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_radio_list_is_grouped_too() {
    let (ui, _deck) = faceted_window(ViewSource::Radio);

    assert!(ui.global::<Player>().get_facets_enabled());
    ui.global::<Player>().invoke_set_grouping(
        app_core::facets::Facet::Artist.index(),
    );
    assert_eq!(ui.global::<Player>().get_pile_count(), 3);
}
