//! 音乐页:搜歌、点一首出声、控制条(播放/暂停、上一首/下一首、随机)。
//!
//! 本模块是**组装点**的一部分:它把 `api` 的请求函数、`audio` 的播放器和
//! `app_core::Queue` 接到 `app_core::Playback` 上,几者互不相识。
//!
//! 显示层面的决定都落在这里,而不是服务端:歌手用什么符号拼、时长写成什么样,
//! 换个界面就该换个写法,不该固化进线上格式。
//!
//! 收听同播时本机是「听众」:在这台机器上做任何播放动作都视为退出收听
//! (`CONTEXT.md`「听众」)。自动续播在收听时也必须闭嘴 —— 它要是切了歌,
//! 会把对面推来的声音捣掉。

use std::cell::RefCell;
use std::rc::Rc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

use app_core::{Playback, PlaybackState, TrackDto};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{MainWindow, TrackRow};
#[cfg(not(target_arch = "wasm32"))]
use app_core::Queue;

/// 多歌手之间的分隔符。
const ARTIST_SEPARATOR: &str = " / ";

/// 一秒有多少毫秒。
const MILLIS_PER_SECOND: i64 = 1_000;

/// 一分钟有多少秒。
const SECONDS_PER_MINUTE: i64 = 60;

/// 队列放完时状态行上的话。常量而非字面量:子集字体测试要遍历到它。
const QUEUE_DONE: &str = "队列放完了";

/// 自动续播的轮询间隔。rodio 没有"放完了"的回调,只能定期看一眼。
#[cfg(not(target_arch = "wasm32"))]
const ADVANCE_POLL: core::time::Duration =
    core::time::Duration::from_secs(1);

/// 歌手列表拼成一行。
///
/// 服务端保持列表形态是对的 —— 用「/」还是「&」是显示问题,只有界面知道。
pub fn join_artists(artists: &[String]) -> String {
    artists.join(ARTIST_SEPARATOR)
}

/// 毫秒时长写成 `分:秒`。
///
/// 负数在真实数据里不会出现(上游是 protobuf 的 int64,平台给的是正数),
/// 但真出现了也不该显示成 `-1:-30` —— 一并压到 0。
pub fn format_duration(duration_ms: i64) -> String {
    let total_seconds =
        (duration_ms / MILLIS_PER_SECOND).max(0);
    let minutes = total_seconds / SECONDS_PER_MINUTE;
    let seconds = total_seconds % SECONDS_PER_MINUTE;
    format!("{minutes}:{seconds:02}")
}

/// 把播放状态翻译成一行人类可读的文案。
pub fn describe_playback(state: &PlaybackState) -> String {
    match state {
        PlaybackState::Idle => "点一首歌开始".to_owned(),
        PlaybackState::Loading(track) => {
            format!("加载中… {}", track.title)
        }
        PlaybackState::Playing(track) => {
            format!("正在播放 {}", track.title)
        }
        PlaybackState::Failed(message) => {
            format!("失败: {message}")
        }
    }
}

/// 开机静默自检的结论:健康就闭嘴,坏了才开口。
///
/// Server 页删掉之后,这是版本协商唯一的运行时入口 —— 客户端与服务端的
/// `PROTOCOL_VERSION` 对不上时,这里的一行话是用户能得到的全部解释。
pub fn describe_startup(
    result: &Result<(), api::ApiError>,
) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(error) => Some(format!("失败: {error}")),
    }
}

/// 自动续播的判据:**只有**「本机在放 && 声源放空了 && 不是听众」才推进队列。
///
/// 听众那一条是硬约束:听众放的 `ChannelSource` 在没数据时给静音而非结束,
/// 正常情况下 `drained` 不会为真;但万一将来有人改了那个行为,这里也不许
/// 在收听时切歌 —— 那会把对面推来的声音捣掉。
pub fn should_advance(
    state: &PlaybackState,
    drained: bool,
    listening: bool,
) -> bool {
    matches!(state, PlaybackState::Playing(_))
        && drained
        && !listening
}

/// 把一首歌翻成列表里的一行。所有格式化都在这里做完,`.slint` 只负责摆。
fn to_row(track: &TrackDto) -> TrackRow {
    TrackRow {
        id: track.id.clone().into(),
        title: track.title.clone().into(),
        artists: join_artists(&track.artists).into(),
        duration: format_duration(track.duration_ms).into(),
    }
}

/// 洗牌的种子。`app-core` 不引 `rand`(要编到 wasm),种子由这里造 ——
/// `RandomState` 每次实例化都带进程级随机,当种子够用。
#[cfg(not(target_arch = "wasm32"))]
fn shuffle_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// 播放侧所有回调共享的一组把手。
///
/// 五个绑定函数各 clone 各的这几样东西,签名会长到看不出谁在用什么 ——
/// 打包起来,clone 一次传一份。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct Deck {
    playback: Rc<RefCell<Playback>>,
    queue: Rc<RefCell<Queue>>,
    player: Arc<Result<audio::Player, audio::AudioError>>,
    sync: crate::syncplay::Sync,
}

/// 把搜索与播放接到音乐页上。
///
/// 音频设备只开一次并常驻:每次播放都重开设备的话,alsa 上会听到明显的咔哒声,
/// 而且第二次开可能因设备被自己占着而失败。开不出来(无声卡)不是致命错误 ——
/// 界面照常能搜歌,点播放时才报错。
///
/// 播放器是 `Arc` 而非 `Rc`:同播的事件在后台线程上到达,听众收到的声音要在
/// **那里**直接出声(见 `syncplay::handle`),绕回 UI 线程只会让起播多等一帧。
#[cfg(not(target_arch = "wasm32"))]
pub fn bind(ui: &MainWindow) -> crate::viz::Source {
    // 搜索结果的权威副本。Slint 的 model 只存格式化后的字符串,
    // 点击时要靠它把 id 换回完整的 TrackDto。
    let tracks: Rc<RefCell<Vec<TrackDto>>> =
        Rc::new(RefCell::new(Vec::new()));
    let player = Arc::new(audio::Player::new());

    let deck = Deck {
        playback: Rc::new(
            RefCell::new(Playback::default()),
        ),
        queue: Rc::new(RefCell::new(Queue::default())),
        sync: crate::syncplay::bind(ui, &player),
        player,
    };

    bind_search(ui, &tracks);
    bind_list(ui, &tracks);
    bind_play(ui, &tracks, &deck);
    bind_controls(ui, &deck);
    start_auto_advance(ui, &deck);
    startup_check(ui);

    ui.set_playback_text(
        describe_playback(&PlaybackState::Idle).into(),
    );

    // 播放页可视化的数据源。无声卡时没有播放器,自然也没有频谱可看。
    deck.player
        .as_ref()
        .as_ref()
        .ok()
        .map(audio::Player::visualizer)
}

/// wasm 上没有原生音频栈(见 `Cargo.toml` 的条件依赖)。界面照常在,
/// 只是这一页不接任何行为 —— 「余端 graceful 缺省」,不写平台判断到 `.slint` 里。
#[cfg(target_arch = "wasm32")]
pub fn bind(ui: &MainWindow) -> crate::viz::Source {
    ui.set_playback_text("Web 端暂不支持播放".into());
    None
}

/// 「Web 端暂不支持播放」里的中文也得在子集字体里 —— 但它只在 wasm 上出现,
/// [`playback_copy_only_uses_subset_glyphs`] 那条守不到。这个常量把它摆到
/// 原生也能看见的地方,好让同一个测试覆盖。
#[cfg(test)]
const WASM_NOTICE: &str = "Web 端暂不支持播放";

/// 搜索:关键词 → `GET /search` → 结果列表。
#[cfg(not(target_arch = "wasm32"))]
fn bind_search(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
) {
    let tracks = tracks.clone();
    let weak = ui.as_weak();

    ui.on_search(move |keyword| {
        let keyword = keyword.to_string();
        if keyword.trim().is_empty() {
            return;
        }

        let tracks = tracks.clone();
        let weak = weak.clone();
        slint::spawn_local(async move {
            let found = api::search(&keyword).await;
            let Some(ui) = weak.upgrade() else { return };
            match found {
                Ok(dto) => show(&ui, &tracks, dto.tracks),
                Err(error) => {
                    // 搜索失败复用播放状态那一行 —— 音乐页只有一处报错位,
                    // 再加一行"搜索状态"会让两行里总有一行是空的。
                    ui.set_playback_text(
                        format!("失败: {error}").into(),
                    );
                }
            }
        })
        .expect("event loop must be running");
    });
}

/// 今日推荐与我喜欢的音乐。两者只差调哪个请求函数,其余完全相同。
#[cfg(not(target_arch = "wasm32"))]
fn bind_list(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
) {
    let daily = tracks.clone();
    let weak = ui.as_weak();
    ui.on_daily(move || {
        fetch_into(&weak, &daily, async {
            api::daily().await.map(|dto| dto.tracks)
        });
    });

    let liked = tracks.clone();
    let weak = ui.as_weak();
    ui.on_liked(move || {
        fetch_into(&weak, &liked, async {
            api::liked().await.map(|dto| dto.tracks)
        });
    });
}

/// 跑一个返回曲目列表的请求,结果填进列表,失败填进状态行。
///
/// 收的是 `Vec<TrackDto>` 而非线上的信封类型:`ui` 按分层不直接依赖 `contract`,
/// 剥壳在调用处一句 `.map(|dto| dto.tracks)` 完成。
///
/// 三个入口(搜索/推荐/红心)填的是**同一个** `tracks` 列表 ——
/// 换一个来源就整批换掉,不合并:合并了就说不清列表里这首是哪来的。
#[cfg(not(target_arch = "wasm32"))]
fn fetch_into<Fut>(
    weak: &slint::Weak<MainWindow>,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
    request: Fut,
) where
    Fut: core::future::Future<
            Output = Result<Vec<TrackDto>, api::ApiError>,
        > + 'static,
{
    let tracks = tracks.clone();
    let weak = weak.clone();
    slint::spawn_local(async move {
        let found = request.await;
        let Some(ui) = weak.upgrade() else { return };
        match found {
            Ok(found) => show(&ui, &tracks, found),
            Err(error) => ui.set_playback_text(
                format!("失败: {error}").into(),
            ),
        }
    })
    .expect("event loop must be running");
}

/// 把一批曲目同时装进 Slint 的 model 和 Rust 侧的权威副本。
#[cfg(not(target_arch = "wasm32"))]
fn show(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
    found: Vec<TrackDto>,
) {
    let rows: Vec<TrackRow> =
        found.iter().map(to_row).collect();
    *tracks.borrow_mut() = found;
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
}

/// 点一首歌:这一批成为队列、从这首开始放(见 `CONTEXT.md`「队列」)。
#[cfg(not(target_arch = "wasm32"))]
fn bind_play(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
    deck: &Deck,
) {
    let tracks = tracks.clone();
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.on_play(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        // 点歌是播放动作:正在收听的话,先退出(CONTEXT.md「听众」)。
        if deck.sync.is_listening() {
            deck.sync.leave();
        }

        let id = id.to_string();
        let batch = tracks.borrow().clone();
        let Some(index) =
            batch.iter().position(|track| track.id == id)
        else {
            return;
        };

        deck.queue.borrow_mut().replace(batch, index);
        if ui.get_shuffle_on() {
            deck.queue.borrow_mut().shuffle(shuffle_seed());
        }
        play_current(&ui, &deck);
    });
}

/// 控制条:播放/暂停、上一首/下一首、随机开关。
///
/// 收听中的任何一键都先退出收听;⏯ 到此为止(退出即静音,再按才操作自己的
/// 队列),切歌键退出后紧接着作用于本机队列 —— 点了"下一首"的人想听的是
/// 自己的下一首,不是单纯安静下来。
#[cfg(not(target_arch = "wasm32"))]
fn bind_controls(ui: &MainWindow, deck: &Deck) {
    let toggle = deck.clone();
    let weak = ui.as_weak();
    ui.on_toggle_play(move || {
        let Some(ui) = weak.upgrade() else { return };
        if toggle.sync.is_listening() {
            toggle.sync.leave();
            if let Ok(player) = toggle.player.as_ref() {
                player.stop();
            }
            ui.set_is_playing(false);
            return;
        }

        let Ok(player) = toggle.player.as_ref() else {
            return;
        };
        if ui.get_is_playing() {
            player.pause();
            ui.set_is_playing(false);
        } else if !player.empty() {
            // 暂停中,接着放。
            player.resume();
            ui.set_is_playing(true);
        } else {
            // 放空了(队列结束后又按了播放):重放当前这首。
            play_current(&ui, &toggle);
        }
    });

    let next = deck.clone();
    let weak = ui.as_weak();
    ui.on_next_track(move || {
        let Some(ui) = weak.upgrade() else { return };
        if next.sync.is_listening() {
            next.sync.leave();
        }
        advance(&ui, &next);
    });

    let previous = deck.clone();
    let weak = ui.as_weak();
    ui.on_prev_track(move || {
        let Some(ui) = weak.upgrade() else { return };
        if previous.sync.is_listening() {
            previous.sync.leave();
        }
        if previous.queue.borrow_mut().previous().is_some()
        {
            play_current(&ui, &previous);
        }
    });

    let shuffle = deck.clone();
    let weak = ui.as_weak();
    ui.on_shuffle_toggled(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_shuffle_on() {
            shuffle
                .queue
                .borrow_mut()
                .shuffle(shuffle_seed());
        } else {
            shuffle.queue.borrow_mut().unshuffle();
        }
    });
}

/// 队列前进一首;到底了就停下并说明(放完即停,见 `CONTEXT.md`「队列」)。
#[cfg(not(target_arch = "wasm32"))]
fn advance(ui: &MainWindow, deck: &Deck) {
    let has_next = deck.queue.borrow_mut().next().is_some();
    if has_next {
        play_current(ui, deck);
    } else {
        // 状态机也要停:不停的话它仍是 Playing,自动续播每秒都会再撞进来。
        deck.playback.borrow_mut().stop();
        ui.set_playback_text(QUEUE_DONE.into());
        ui.set_is_playing(false);
    }
}

/// 放队列的当前曲目:取直链 → 开流 → 解码 → 出声,经 `app_core::play` 记账。
#[cfg(not(target_arch = "wasm32"))]
fn play_current(ui: &MainWindow, deck: &Deck) {
    let Some(track) =
        deck.queue.borrow().current().cloned()
    else {
        return;
    };

    // spawn_local 的 future 要到下一轮事件循环才跑,而 Loading 要立刻显示。
    ui.set_playback_text(
        describe_playback(&PlaybackState::Loading(
            track.clone(),
        ))
        .into(),
    );

    // 播放页的歌名与封面。旧封面立刻清掉 —— 新歌配旧图比空着更误导。
    ui.set_now_title(track.title.clone().into());
    ui.set_now_artists(join_artists(&track.artists).into());
    ui.set_cover_art(slint::Image::default());
    if let Some(url) = track.cover.clone() {
        let weak = ui.as_weak();
        slint::spawn_local(async move {
            // 拿不到或解不出就保持空图:封面 CDN 会过期,失败是常态(见 cover.rs)。
            if let Ok(bytes) = api::fetch_bytes(&url).await
                && let Some(img) =
                    crate::cover::decode(&bytes)
                && let Some(ui) = weak.upgrade()
            {
                ui.set_cover_art(img);
            }
        })
        .expect("event loop must be running");
    }

    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        app_core::play(&deck.playback, track, |track| {
            start(
                deck.player.clone(),
                deck.sync.clone(),
                track,
            )
        })
        .await;

        if let Some(ui) = weak.upgrade() {
            let state = deck.playback.borrow();
            ui.set_is_playing(matches!(
                state.state(),
                PlaybackState::Playing(_)
            ));
            ui.set_playback_text(
                describe_playback(state.state()).into(),
            );
        }
    })
    .expect("event loop must be running");
}

/// 自动续播:每秒看一眼,放空了就推进队列。
///
/// rodio 没有"放完了"的回调,轮询是唯一的办法;判据抽在 [`should_advance`],
/// 收听同播时它恒为假 —— 那时切歌会把对面推来的声音捣掉。
#[cfg(not(target_arch = "wasm32"))]
fn start_auto_advance(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();
    let timer = slint::Timer::default();

    timer.start(
        slint::TimerMode::Repeated,
        ADVANCE_POLL,
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let drained = match deck.player.as_ref() {
                Ok(player) => player.empty(),
                Err(_) => true,
            };
            if should_advance(
                deck.playback.borrow().state(),
                drained,
                deck.sync.is_listening(),
            ) {
                advance(&ui, &deck);
            }
        },
    );

    // ponytail: 定时器与进程同寿,leak 掉省一条把 Timer 递回平台入口的通道;
    // 真要按页开关时再把它挂到 Deck 上管理。
    Box::leak(Box::new(timer));
}

/// 开机静默自检:`GET /health` 一次,健康就一声不吭。
///
/// Server 页删掉之后,这是协议版本协商唯一的运行时入口(`api::health` 内部
/// 比对 `PROTOCOL_VERSION`)。坏消息写进音乐页状态行 —— 那是用户最先看的地方。
#[cfg(not(target_arch = "wasm32"))]
fn startup_check(ui: &MainWindow) {
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let result = api::health().await.map(|_dto| ());
        if let Some(message) = describe_startup(&result)
            && let Some(ui) = weak.upgrade()
        {
            ui.set_playback_text(message.into());
        }
    })
    .expect("event loop must be running");
}

/// 真正把一首歌变成声音:取直链 → 开流 → 解码 → 归一 → 分出一支给同播 → 出声。
///
/// 这就是注入给 `app_core::play` 的那个闭包体。`app-core` 只看到
/// "一个返回 Result 的 future",看不到 HTTP、alsa,也看不到 WebRTC。
///
/// 每首歌都分一支给同播,不管当下有没有人在听:支路满了会自己丢采样
/// (见 `audio::codec::Tee`),而等"确认有人听"再接的话,换歌时听众会掉音。
#[cfg(not(target_arch = "wasm32"))]
async fn start(
    player: Arc<Result<audio::Player, audio::AudioError>>,
    sync: crate::syncplay::Sync,
    track: TrackDto,
) -> Result<(), String> {
    use audio::codec::{BRANCH_CAPACITY, Tee, normalize};

    let source = api::play_source(&track.id)
        .await
        .map_err(|error| error.to_string())?;
    // 开流与解码都在 `audio` 自己的后台 runtime 上跑 —— 这里是 Slint 的 UI 线程,
    // 没有 tokio 反应堆,也不能被阻塞读占住。
    let decoded = audio::load(&source.url)
        .await
        .map_err(|error| error.to_string())?;

    match player.as_ref() {
        Ok(player) => {
            // 先归一再分支:本机听到的和推出去的因此是同一批采样,
            // 而 Opus 只在 48kHz 立体声上工作(见 `audio::codec::normalize`)。
            let (tee, branch) = Tee::new(
                normalize(decoded),
                BRANCH_CAPACITY,
            );
            // 先换歌再交支路。反过来的话,新泵在上一首还没被丢掉时就起来了,
            // 两条泵会同时往同一条轨上写,听众听到的是两首歌交错的几十毫秒。
            player.play(tee);
            sync.feed(branch);
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn track() -> TrackDto {
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

    /// 四个状态都要有人能读的文案。少一个,用户就会遇到一行空白 ——
    /// 那时候他分不清是没点中、还是放不了。
    #[test]
    fn describe_playback_covers_every_state() {
        for state in [
            PlaybackState::Idle,
            PlaybackState::Loading(track()),
            PlaybackState::Playing(track()),
            PlaybackState::Failed("直链已过期".to_owned()),
        ] {
            assert!(
                !describe_playback(&state).is_empty(),
                "{state:?} 没有文案"
            );
        }
    }

    /// 多个歌手用固定分隔符拼成一行。
    #[test]
    fn track_row_joins_artists_with_separator() {
        assert_eq!(
            join_artists(&[
                "LiSA".to_owned(),
                "梶浦由記".to_owned()
            ]),
            "LiSA / 梶浦由記"
        );
    }

    /// 234000ms 是 3 分 54 秒。
    #[test]
    fn track_row_formats_duration_as_minutes_seconds() {
        assert_eq!(format_duration(234_000), "3:54");
    }

    /// 秒数不足两位要补零 —— 不补的话 61 秒会写成 `1:1`,读起来像 1 分 10 秒。
    #[test]
    fn track_row_pads_seconds_below_ten() {
        assert_eq!(format_duration(61_000), "1:01");
    }

    /// 平台没给时长时上游填 0。要写成 `0:00`,不能是空白 ——
    /// 空白会让那一列忽有忽无,整个列表跟着抖。
    #[test]
    fn track_row_handles_zero_duration() {
        assert_eq!(format_duration(0), "0:00");
    }

    /// 自动续播只在「本机在放 && 放空了 && 不是听众」时才动手。
    ///
    /// 第二行是本条真正要守的:**收听同播时绝不切歌**,即使声源看起来空了 ——
    /// 切了就是把对面推来的声音捣掉。
    #[test]
    fn advances_only_when_playing_and_drained_and_not_listening()
     {
        let playing = PlaybackState::Playing(track());

        assert!(should_advance(&playing, true, false));
        assert!(
            !should_advance(&playing, true, true),
            "收听中不许自动切歌"
        );
        assert!(!should_advance(&playing, false, false));
        assert!(!should_advance(
            &PlaybackState::Idle,
            true,
            false
        ));
        assert!(!should_advance(
            &PlaybackState::Loading(track()),
            true,
            false
        ));
    }

    /// 开机自检只在坏消息时开口:健康 → None,失败 → 一行能显示的话。
    #[test]
    fn startup_check_speaks_only_on_failure() {
        assert_eq!(describe_startup(&Ok(())), None);

        let mismatch = describe_startup(&Err(
            api::ApiError::VersionMismatch {
                expected: 1,
                actual: 2,
            },
        ))
        .expect("版本不匹配必须开口");
        assert!(
            mismatch.contains("协议版本不匹配"),
            "文案要点名版本问题,实得 {mismatch}"
        );

        let transport = describe_startup(&Err(
            api::ApiError::Transport(
                "connection refused".to_owned(),
            ),
        ))
        .expect("连不上必须开口");
        assert!(
            transport.contains("网络错误"),
            "文案要点名网络问题,实得 {transport}"
        );
    }

    /// 子集字体必须覆盖本模块会吐出的每一个非 ASCII 字符。
    ///
    /// 新增中文而忘了重跑 `just font-subset`,这里就红。
    ///
    /// **歌名本身不在此列** —— 它是平台给的任意 CJK,不可能预裁,
    /// 界面上走系统字体(见 `app.slint` 音乐页的注释)。所以下面只喂 ASCII 标题,
    /// 检查的是文案里的固定部分。
    ///
    /// 只在原生上跑:失败文案取自 `audio::AudioError`,而 `audio` 是 wasm 下
    /// 不存在的条件依赖(见 `Cargo.toml`)。
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn playback_copy_only_uses_subset_glyphs() {
        use api::ApiError;
        use audio::AudioError;

        const CJK_SUBSET: &[u8] =
            include_bytes!("../fonts/cjk-subset.ttf");

        let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
            .expect("子集字体应能被解析");

        let ascii_track = TrackDto {
            title: "Gurenge".to_owned(),
            ..track()
        };

        // 失败原因不是手抄的:直接取两个错误类型的真实 Display 输出。
        // 它们各自带中文前缀,改了措辞而没重裁字体,这里会红。
        let failures: Vec<String> = vec![
            AudioError::Device("no device".to_owned())
                .to_string(),
            AudioError::Stream("refused".to_owned())
                .to_string(),
            AudioError::Decode("unrecognized".to_owned())
                .to_string(),
            ApiError::Transport("timed out".to_owned())
                .to_string(),
            ApiError::Decode("expected value".to_owned())
                .to_string(),
            ApiError::VersionMismatch {
                expected: 1,
                actual: 2,
            }
            .to_string(),
        ];

        let mut states = vec![
            PlaybackState::Idle,
            PlaybackState::Loading(ascii_track.clone()),
            PlaybackState::Playing(ascii_track),
        ];
        states.extend(
            failures.into_iter().map(PlaybackState::Failed),
        );

        let mut copy: Vec<String> =
            states.iter().map(describe_playback).collect();
        // 不经过 describe_playback 的固定文案,单独列上。
        copy.push(QUEUE_DONE.to_owned());
        copy.push(WASM_NOTICE.to_owned());
        copy.push("同播: 没有其他设备".to_owned());
        // 听众收听时的播放行(见 `syncplay.rs` 的 Listening 分支)。
        copy.push("收听中…".to_owned());
        // 开机自检的两种坏消息。
        copy.extend(
            [
                describe_startup(&Err(
                    api::ApiError::VersionMismatch {
                        expected: 1,
                        actual: 2,
                    },
                )),
                describe_startup(&Err(
                    api::ApiError::Transport(
                        "refused".to_owned(),
                    ),
                )),
            ]
            .into_iter()
            .flatten(),
        );

        for line in copy {
            for ch in line.chars() {
                assert!(
                    face.glyph_index(ch).is_some(),
                    "子集字体缺字 {ch:?}(文案 {line:?})—— 重跑 `just font-subset`"
                );
            }
        }
    }
}
