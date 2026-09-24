//! 音乐页:搜歌、点一首出声、控制条(播放/暂停、上一首/下一首、随机)。
//!
//! 本模块是**组装点**的一部分:它把 `api` 的请求函数、`audio` 的播放器和
//! `app_core::Queue` 接到 `app_core::Playback` 上,几者互不相识。
//!
//! 显示层面的决定都落在这里,而不是服务端:歌手用什么符号拼、时长写成什么样,
//! 换个界面就该换个写法,不该固化进线上格式。

use std::cell::RefCell;
use std::rc::Rc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

use app_core::{
    Playback, PlaybackState, TrackDto, TracksDto,
};

use slint::{
    ComponentHandle, Model as _, ModelRc, VecModel,
};

use crate::{MainWindow, TrackRow};

#[cfg(not(target_arch = "wasm32"))]
use app_core::Queue;

#[cfg(not(target_arch = "wasm32"))]
mod download;
mod feed;
mod list;
mod notice;
mod queuepage;
mod report;
mod rules;
#[cfg(not(target_arch = "wasm32"))]
mod views;
// 放哪一首:控制条、传输层、自动续播。
mod playback;

pub(crate) use feed::{CoverFeed, LyricFeed};
pub use rules::{describe_playback, join_artists};

#[cfg(not(target_arch = "wasm32"))]
pub use download::{
    DownloadCommit, DownloadStore, install_download_store,
};

// 各子模块的条目都引进这一层,子模块的 `use super::*` 因此能互相看见 ——
// 拆分前它们本就在同一个作用域里,这几行是把那个作用域重新拼起来。
use crate::Player;
#[cfg(not(target_arch = "wasm32"))]
use download::bind_download;
use list::*;
use notice::*;
use playback::*;
use report::*;
use rules::*;
#[cfg(not(target_arch = "wasm32"))]
use views::*;

/// 洗牌与循环回卷重洗的种子。`app-core` 不引 `rand`(要编到 wasm),
/// 种子由这里造:`RandomState` 每次实例化都带进程级随机,当种子够用。
#[cfg(not(target_arch = "wasm32"))]
fn shuffle_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// 备好的下一首:它是哪一首,以及备好的那一份(解码器 + 那条流的健康句柄)。
///
/// 抽成别名是 clippy 的要求 —— 三层嵌套写在字段上确实读不出是什么。
#[cfg(not(target_arch = "wasm32"))]
type Prefetched = Rc<
    RefCell<
        Option<(
            String,
            (audio::Loaded, audio::StreamHealth),
        )>,
    >,
>;

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
    /// 输出设备与遥控状态。本机输出时它一概不插手,现有路径一个字节不变
    /// (见 `crate::sync::remote`)。
    remote: crate::sync::remote::Remote,
    /// 系统媒体控件的把手。后端由平台入口给,这里只管往里推(见 `crate::media`)。
    media: Rc<crate::media::Bridge>,
    lyrics: LyricFeed,
    cover: CoverFeed,
    /// 列表里那一批歌的权威副本。Slint 的 model 只存格式化后的字符串,
    /// 点击时要靠它把 id 换回完整的 `TrackDto`;重推行(标加载态)也从它来。
    tracks: Rc<RefCell<Vec<TrackDto>>>,
    /// 各个浏览视图自己那份曲目,以及此刻摆的是哪一个(见 `views`)。
    /// `tracks` 永远是其中当前那一份的投影。
    views: Views,
    /// 哪些歌在红心里。服务端给的曲目不带这个字段(那要让每个列表接口都多问
    /// 一次上游),所以取一次全量标识存成集合,推行时本地比对(见 crate::library::liked)。
    liked: crate::library::liked::LikedSet,
    /// 正在编辑哪个歌单,以及打开它之前列表里摆的那一批歌。
    /// 后者是「把刚才那批加进来」的唯一来源 —— 进歌单那一刻 `tracks`
    /// 就被换掉了(见 crate::library::playlist::Editing)。
    editing: crate::library::playlist::Editing,
    /// 歌单封面表。取一次、记住、下次直接给(见 crate::imagery::artwork)。
    artwork: crate::imagery::artwork::Artwork,
    /// 曲目行的缩略图。与 `artwork` 是两套:那边按歌单 id 存全量取,
    /// 这边按封面 URL 存、只取滑进可见区的那些(见 crate::imagery::thumbnail)。
    thumbnails: crate::imagery::thumbnail::Thumbnails,
    /// 上次拉当日推荐的日期。推荐是**当天**的,跨过零点就过期(见 [`daily_is_due`])。
    /// 只活在进程里 —— 重启重拉一次,不落盘。
    last_daily:
        Rc<std::cell::Cell<Option<chrono::NaiveDate>>>,
    /// 当前这一路流的死亡证明。源结束时问它:放弃过就是断流,没放弃就是放完了。
    /// 两者在播放器那头长得一模一样(见 `docs/adr/0013`)。
    stream: Rc<RefCell<Option<audio::StreamHealth>>>,
    /// 已经备好的下一首:(它是哪一首, 解码器, 健康句柄)。
    ///
    /// 带着 id 是必须的:用户中途点了别的歌、或洗了牌,备的就不是要放的那一首了。
    /// 认错了会放出一首根本没点过的歌([`take_prefetched`] 负责这道校验)。
    prefetched: Prefetched,
    /// 预取是不是正在路上。少了它,轮询每秒都会再起一条下载。
    prefetching: Rc<std::cell::Cell<bool>>,
    /// 当前这一路的跳转状态。跳转是异步的 —— `player.seek()` 只把请求送出去,
    /// 真正取字节在解码线程上,成没成要问这里(见 `audio::SeekState`)。
    /// 每首歌一个,换歌时跟着换。
    seeking: Rc<RefCell<Option<audio::SeekState>>>,
    /// 手上这份执行副本是服务端哪个队列的哪一版(见 `playback::execution`)。
    ///
    /// 与 `queue` 并排而不是塞进它:`app_core::Queue` 管的是「放哪一首、
    /// 下一首是谁」,它一个字节都不该知道服务端的存在 —— 断网时自动续播
    /// 照样要走(`docs/adr/0031` 四)。
    execution: Execution,
    /// 遥控时拉下来的那份**只读**队列缓存(见 `queuepage`)。
    ///
    /// 遥控器持有的不是第二份队列真相:它不拿这份决定下一首,只用来画
    /// 那一页(`docs/adr/0031` 三)。
    queue_mirror: queuepage::QueueMirror,
    /// 等下一帧画出来的用户动作(见 `crate::runtime::trace`)。渲染循环也拿着
    /// 同一份,画完一帧就给它们打 `drawn`。
    frames: crate::runtime::trace::Frames,
    /// 下一次起播不从头放:从哪、放不放。迁移过来的那一首用它,起播时取走
    /// (见 `playback::migrate`)。
    start_at: Rc<std::cell::Cell<Option<report::Start>>>,
    /// 本机作为迁移的一端:备好的那一份,与最近一次迁移步骤的回话
    /// (见 `playback::migrate`)。
    member: Member,
    /// 音量的节流存盘(见 `playback::dispatch::VolumeSave`)。
    volume_save: VolumeSave,
    /// 换过几次歌。封面在后台线程上排队解码时拿它判断「还是不是这一首」——
    /// `playback` 是 `Rc`,过不了线程(见 `imagery::cover::decode_off_thread`)。
    cover_turn: Arc<std::sync::atomic::AtomicU64>,
}

/// 把搜索与播放接到音乐页上。
///
/// 音频设备只开一次并常驻:每次播放都重开设备的话,alsa 上会听到明显的咔哒声,
/// 而且第二次开可能因设备被自己占着而失败。开不出来(无声卡)不是致命错误 ——
/// 界面照常能搜歌,点播放时才报错。
#[cfg(not(target_arch = "wasm32"))]
pub fn bind(
    ui: &MainWindow,
    media: impl FnOnce(
        crate::media::MediaHooks,
    )
        -> Box<dyn crate::media::MediaControls>,
) -> (
    crate::viz::Source,
    LyricFeed,
    CoverFeed,
    crate::runtime::trace::Frames,
) {
    let player = Arc::new(audio::Player::new());
    let media =
        Rc::new(crate::media::bind(ui, &player, media));

    let lyrics = LyricFeed {
        lines: Rc::new(RefCell::new(Vec::new())),
        generation: Rc::new(std::cell::Cell::new(0)),
        player: player.clone(),
    };
    let cover = CoverFeed::default();

    let remote = crate::sync::link::bind(ui);

    let deck = Deck {
        playback: Rc::new(
            RefCell::new(Playback::default()),
        ),
        queue: Rc::new(RefCell::new(Queue::default())),
        remote,
        media,
        player,
        lyrics: lyrics.clone(),
        cover: cover.clone(),
        tracks: Rc::new(RefCell::new(Vec::new())),
        views: Views::default(),
        liked: crate::library::liked::LikedSet::default(),
        editing: crate::library::playlist::Editing::default(
        ),
        artwork: crate::imagery::artwork::Artwork::default(
        ),
        thumbnails:
            crate::imagery::thumbnail::Thumbnails::default(),
        last_daily: Rc::new(std::cell::Cell::new(None)),
        stream: Rc::new(RefCell::new(None)),
        prefetched: Rc::new(RefCell::new(None)),
        prefetching: Rc::new(std::cell::Cell::new(false)),
        seeking: Rc::new(RefCell::new(None)),
        execution: Execution::default(),
        queue_mirror: queuepage::QueueMirror::default(),
        frames: crate::runtime::trace::Frames::default(),
        start_at: Rc::new(std::cell::Cell::new(None)),
        member: Member::default(),
        volume_save: Default::default(),
        cover_turn: Default::default(),
    };

    // 红心先接上再拉:拉回来那一刻会重标列表,而列表这时还是空的,
    // 真正生效的是之后每次 push_rows 里的那次重标。
    crate::library::liked::bind(ui, &deck.liked);
    crate::library::liked::refresh(&deck.liked, ui);

    // 本地歌单的写操作。改完要把当前歌单的曲目重取一遍,而那要用播放队列 ——
    // 队列归这里,所以重取那一步由这边交出去。
    let reloading = deck.clone();
    crate::library::playlist::bind_edit(
        ui,
        &deck.editing,
        &deck.artwork,
        move |ui| reload_open_playlist(ui, &reloading),
    );

    // 缩略图目录削一次。放在这里而不是写入路径上:几百个文件的 metadata()
    // 是毫秒级,而挂在每次写入后面会让滚一次列表 stat 整个目录几十遍。
    api::sweep_track_artwork();
    bind_needs_cover(ui, &deck);

    bind_volume(ui, &deck);
    bind_seek(ui, &deck);

    bind_search(ui, &deck);
    bind_list(ui, &deck);
    bind_play(ui, &deck);
    bind_controls(ui, &deck);
    bind_remote(ui, &deck);
    bind_session(ui, &deck);
    queuepage::bind(ui, &deck);
    bind_download(ui, &deck);
    start_auto_advance(ui, &deck);
    start_progress_tick(ui, &deck);
    startup_check(ui);

    ui.global::<Player>().set_playback_text(
        describe_playback(&PlaybackState::Idle).into(),
    );

    // 播放页可视化的数据源。无声卡时没有播放器,自然也没有频谱可看。
    let viz = deck
        .player
        .as_ref()
        .as_ref()
        .ok()
        .map(audio::Player::visualizer);
    (viz, lyrics, cover, deck.frames.clone())
}

/// wasm 上没有原生音频栈(见 `Cargo.toml` 的条件依赖)。界面照常在,
/// 只是这一页不接任何行为 —— 「余端 graceful 缺省」,不写平台判断到 `.slint` 里。
#[cfg(target_arch = "wasm32")]
pub fn bind(
    ui: &MainWindow,
    _media: impl FnOnce(
        crate::media::MediaHooks,
    )
        -> Box<dyn crate::media::MediaControls>,
) -> (
    crate::viz::Source,
    LyricFeed,
    CoverFeed,
    crate::runtime::trace::Frames,
) {
    // 状态行而不是提示:wasm 上这句永远为真,它就是这一端的播放状态。
    ui.global::<Player>()
        .set_playback_text("Web 端暂不支持播放".into());

    // 下载键在这一端点得动却什么都不会发生 —— 那是「界面上不放假东西」
    // (docs/design.md 硬规则 8)最典型的样子。接一句实话上去。
    let weak = ui.as_weak();
    ui.global::<crate::Library>().on_download_track(
        move |_id| {
            if let Some(ui) = weak.upgrade() {
                crate::notice::show(
                    &ui,
                    "Web 端暂不支持下载".to_owned(),
                );
            }
        },
    );

    (
        None,
        LyricFeed,
        CoverFeed::default(),
        crate::runtime::trace::Frames::default(),
    )
}

/// 「Web 端暂不支持播放」里的中文也得在子集字体里 —— 但它只在 wasm 上出现,
/// [`playback_copy_only_uses_subset_glyphs`] 那条守不到。这个常量把它摆到
/// 原生也能看见的地方,好让同一个测试覆盖。
#[cfg(test)]
const WASM_NOTICE: &str = "Web 端暂不支持播放";

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;
