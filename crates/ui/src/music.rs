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

use app_core::{
    Playback, PlaybackState, TrackDto, TracksDto,
};
use slint::{
    ComponentHandle, Model as _, ModelRc, VecModel,
};

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

/// 当前这首放过多久之后才去备下一首。
///
/// 早了会和正在放的那首抢带宽,晚了等于没预取。十秒:那时起播的那阵下载高峰
/// 已经过去,而离用户可能按「下一首」还早(见 [`should_prefetch`])。
#[cfg(not(target_arch = "wasm32"))]
const PREFETCH_AFTER: core::time::Duration =
    core::time::Duration::from_secs(10);

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

/// 该不该起预取:本机在放、已经放过一阵、手里没有备着的、队列还有下一首。
///
/// **不能一起播就预取**:那样两条下载会抢同一条链路,而正在放的那首经不起抢
/// (取直链的 CDN 本来就爱停摆,见 `docs/adr/0013`)。等当前这首站稳了再备,
/// 反正备的是"还有一整首歌的时间"之后才用得上的东西。
///
/// 听众那一条与 [`should_advance`] 同理:收听时切歌的决定权不在本机,
/// 备了也用不上,白占一条下载。
#[cfg(not(target_arch = "wasm32"))]
pub fn should_prefetch(
    state: &PlaybackState,
    position: core::time::Duration,
    listening: bool,
    already_have: bool,
    has_next: bool,
) -> bool {
    matches!(state, PlaybackState::Playing(_))
        && position >= PREFETCH_AFTER
        && !listening
        && !already_have
        && has_next
}

/// 该不该报断流:本机在放、声源空了、而且这条流留下了放弃的证据。
///
/// 与 [`should_advance`] 是同一刻的两个出口,互斥:声源空下来时,要么是放完了
/// 该切下一首,要么是断了该停下说话。四个输入一模一样,只多问一句"放弃过没有" ——
/// 那正是两者唯一的分野(见 `docs/adr/0013`)。
///
/// 听众那一条与 [`should_advance`] 同理:收听时本机没有自己的流,`gave_up`
/// 反映的是上一次本机播放留下的旧证据,不能拿它去掐别人推来的声音。
pub fn should_report_loss(
    state: &PlaybackState,
    drained: bool,
    listening: bool,
    gave_up: bool,
) -> bool {
    matches!(state, PlaybackState::Playing(_))
        && drained
        && !listening
        && gave_up
}

/// 断流横幅该说哪句话。
///
/// `server_reachable` 是那次 `/health` 探测的结论:`None` = 还没回来。
/// 先弹粗文案再改精确文案,是为了不让沉默时长受探测连累(见 `docs/adr/0013`)。
///
/// 探得通只说明**我们自己的**服务端还在,上游 CDN 挂了也会落到这一支 ——
/// 所以那句话指向用户能做的动作,不去断言是谁的锅。
pub fn describe_stream_loss(
    server_reachable: Option<bool>,
) -> &'static str {
    match server_reachable {
        None => "播放中断了",
        Some(false) => "没网了,检查一下网络再试",
        Some(true) => "播放地址失效了,重新点一下这首歌",
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

/// 这一下点击是不是多余的。
///
/// 判据只认 `Loading`,而且只认**同一首**:网络慢时点了看不出反应,用户就会
/// 连点,每一下都发一次下载、每条回来都从头出声。已经在放的那一首再点是
/// 「从头听」,不是多余(见 `CONTEXT.md`「队列」)。
pub fn is_redundant_tap(
    state: &PlaybackState,
    id: &str,
) -> bool {
    matches!(state, PlaybackState::Loading(track) if track.id == id)
}

/// 当日推荐该不该拉。`last` 是上次拉取的日期,`today` 是今天。
///
/// 相等就不拉,于是搜完歌切出去再回来,搜索结果不会被推荐冲掉 —— 三个入口
/// (搜索/推荐/红心)填的是同一个列表,拉一次就整批换掉。
/// 不相等一律拉,包括 `last` 比 `today` 还晚的情况:时钟被拨过就重拉一次,
/// 比推理"是不是该信这个日期"便宜。
pub fn daily_is_due<D: PartialEq>(
    last: Option<&D>,
    today: &D,
) -> bool {
    last != Some(today)
}

/// 把一批歌翻成列表的行,顺带标出正在加载的那一首。
///
/// 所有格式化都在这里做完,`.slint` 只负责摆。`loading` 给的是那一首的 id ——
/// 它不在这批里(点完歌又搜了别的)就一行都不标。
#[cfg(not(target_arch = "wasm32"))]
fn to_rows(
    batch: &[TrackDto],
    loading: Option<&str>,
) -> Vec<TrackRow> {
    batch
        .iter()
        .map(|track| TrackRow {
            id: track.id.clone().into(),
            title: track.title.clone().into(),
            artists: join_artists(&track.artists).into(),
            duration: format_duration(track.duration_ms)
                .into(),
            loading: loading == Some(track.id.as_str()),
            // 红心状态由 push_rows 之后的 remark 填 —— 这里没有那个集合,
            // 而把它传进来会让这个纯格式化函数多认识一样东西。
            liked: false,
            // 平台没给封面就是空串,那一行永远画占位色(见 tracklist.slint)。
            cover_url: track
                .cover
                .clone()
                .unwrap_or_default()
                .into(),
            // 图由 thumbnail 在行滑进可见区之后回填,与红心同理。
            cover: slint::Image::default(),
        })
        .collect()
}

/// 正在加载的那一首的 id,没有就给 `None`。
#[cfg(not(target_arch = "wasm32"))]
fn loading_id(state: &PlaybackState) -> Option<&str> {
    match state {
        PlaybackState::Loading(track) => {
            Some(track.id.as_str())
        }
        _ => None,
    }
}

/// 当前这一首的 id —— 正在加载的和已经在放的都算,没有就给 `None`。
///
/// 与 [`loading_id`] 的差别正是"算不算已经放起来的那一首":那一个用来在列表上
/// 标加载态,这一个用来判断异步回来的东西**还是不是给当前这首的**。
#[cfg(not(target_arch = "wasm32"))]
fn current_id(state: &PlaybackState) -> Option<&str> {
    match state {
        PlaybackState::Loading(track)
        | PlaybackState::Playing(track) => {
            Some(track.id.as_str())
        }
        _ => None,
    }
}

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
    sync: crate::syncplay::Sync,
    /// 系统媒体控件的把手。后端由平台入口给,这里只管往里推(见 `crate::media`)。
    media: Rc<crate::media::Bridge>,
    lyrics: LyricFeed,
    cover: CoverFeed,
    /// 列表里那一批歌的权威副本。Slint 的 model 只存格式化后的字符串,
    /// 点击时要靠它把 id 换回完整的 `TrackDto`;重推行(标加载态)也从它来。
    tracks: Rc<RefCell<Vec<TrackDto>>>,
    /// 哪些歌在红心里。服务端给的曲目不带这个字段(那要让每个列表接口都多问
    /// 一次上游),所以取一次全量标识存成集合,推行时本地比对(见 crate::liked)。
    liked: crate::liked::LikedSet,
    /// 正在编辑哪个歌单,以及打开它之前列表里摆的那一批歌。
    /// 后者是「把刚才那批加进来」的唯一来源 —— 进歌单那一刻 `tracks`
    /// 就被换掉了(见 crate::playlist::Editing)。
    editing: crate::playlist::Editing,
    /// 歌单封面表。取一次、记住、下次直接给(见 crate::artwork)。
    artwork: crate::artwork::Artwork,
    /// 曲目行的缩略图。与 `artwork` 是两套:那边按歌单 id 存全量取,
    /// 这边按封面 URL 存、只取滑进可见区的那些(见 crate::thumbnail)。
    thumbnails: crate::thumbnail::Thumbnails,
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
}

/// 点云封面的取用口:播放页每帧问它「这一帧封面该怎么办」。
///
/// 只在换歌那一帧交出动作,取走即回到"没消息" —— 一张封面是兆级的字节,
/// 每帧搬一次过 seam 纯属白耗(见 `crates/render3d::cloud`)。
///
/// 三态而不是"有没有新图":换歌与拿到新图之间隔着几百毫秒的网络,而封面
/// 常常根本拿不到(CDN 会过期)。只有"有没有新图"的话,这两种情况长得一样,
/// 点云就会一直挂着上一首(见 `crate::viz::CoverUpdate`)。
#[derive(Clone, Default)]
pub(crate) struct CoverFeed {
    pending: Rc<RefCell<crate::viz::CoverUpdate>>,
}

impl CoverFeed {
    /// 取走这一帧的动作,取完回到 [`crate::viz::CoverUpdate::Unchanged`]。
    pub(crate) fn take(&self) -> crate::viz::CoverUpdate {
        core::mem::take(&mut *self.pending.borrow_mut())
    }

    /// 换歌了:先让点云退回渐变,别挂着上一首的图等新图。
    fn clear(&self) {
        *self.pending.borrow_mut() =
            crate::viz::CoverUpdate::Clear;
    }

    /// 新封面解出来了:排上队等下一帧取走。上一个动作还没被取走就直接顶掉 ——
    /// 点云只显示当前这一首,过期的封面排队也没人要。
    fn replace(
        &self,
        pixels: std::sync::Arc<crate::viz::CoverPixels>,
    ) {
        *self.pending.borrow_mut() =
            crate::viz::CoverUpdate::Show(pixels);
    }
}

/// 歌词的取用口:播放页每帧问它「现在该显示哪一行」。
///
/// 行表随换歌整批替换,`generation` 随之自增 —— 调用方靠 `(generation, 行号)`
/// 判断该不该推新值:每帧无脑推会把属性标脏,播放页的省电门就白设了。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub(crate) struct LyricFeed {
    lines: Rc<RefCell<Vec<app_core::LyricLineDto>>>,
    generation: Rc<std::cell::Cell<u64>>,
    player: Arc<Result<audio::Player, audio::AudioError>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl LyricFeed {
    /// 当前该显示的 (代际, 行号, 原文, 译文)。没歌词、还在前奏、或没播放器时给 `None`。
    pub(crate) fn current(
        &self,
    ) -> Option<(u64, usize, String, String)> {
        let player = self.player.as_ref().as_ref().ok()?;
        let position = player.position().as_millis() as i64;
        let lines = self.lines.borrow();
        let index =
            app_core::current_line(&lines, position)?;
        let line = lines.get(index)?;
        Some((
            self.generation.get(),
            index,
            line.text.clone(),
            line.translation.clone().unwrap_or_default(),
        ))
    }

    /// 换歌:先清空(旧歌词配新歌比空着更误导),取到再整批换上并递增代际。
    fn replace(&self, lines: Vec<app_core::LyricLineDto>) {
        *self.lines.borrow_mut() = lines;
        self.generation.set(self.generation.get() + 1);
    }
}

/// wasm 上没有播放器,也就没有位置可读。恒给 `None`,调用方无需平台判断。
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Default)]
pub(crate) struct LyricFeed;

#[cfg(target_arch = "wasm32")]
impl LyricFeed {
    pub(crate) fn current(
        &self,
    ) -> Option<(u64, usize, String, String)> {
        None
    }
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
pub fn bind(
    ui: &MainWindow,
    media: impl FnOnce(
        crate::media::MediaHooks,
    )
        -> Box<dyn crate::media::MediaControls>,
) -> (crate::viz::Source, LyricFeed, CoverFeed) {
    let player = Arc::new(audio::Player::new());
    let media =
        Rc::new(crate::media::bind(ui, &player, media));

    let lyrics = LyricFeed {
        lines: Rc::new(RefCell::new(Vec::new())),
        generation: Rc::new(std::cell::Cell::new(0)),
        player: player.clone(),
    };
    let cover = CoverFeed::default();

    let deck = Deck {
        playback: Rc::new(
            RefCell::new(Playback::default()),
        ),
        queue: Rc::new(RefCell::new(Queue::default())),
        sync: crate::syncplay::bind(ui, &player),
        media,
        player,
        lyrics: lyrics.clone(),
        cover: cover.clone(),
        tracks: Rc::new(RefCell::new(Vec::new())),
        liked: crate::liked::LikedSet::default(),
        editing: crate::playlist::Editing::default(),
        artwork: crate::artwork::Artwork::default(),
        thumbnails: crate::thumbnail::Thumbnails::default(),
        last_daily: Rc::new(std::cell::Cell::new(None)),
        stream: Rc::new(RefCell::new(None)),
        prefetched: Rc::new(RefCell::new(None)),
        prefetching: Rc::new(std::cell::Cell::new(false)),
        seeking: Rc::new(RefCell::new(None)),
    };

    // 红心先接上再拉:拉回来那一刻会重标列表,而列表这时还是空的,
    // 真正生效的是之后每次 push_rows 里的那次重标。
    crate::liked::bind(ui, &deck.liked);
    crate::liked::refresh(&deck.liked, ui);

    // 本地歌单的写操作。改完要把当前歌单的曲目重取一遍,而那要用播放队列 ——
    // 队列归这里,所以重取那一步由这边交出去。
    let reloading = deck.clone();
    crate::playlist::bind_edit(
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
    start_auto_advance(ui, &deck);
    startup_check(ui);

    ui.set_playback_text(
        describe_playback(&PlaybackState::Idle).into(),
    );

    // 播放页可视化的数据源。无声卡时没有播放器,自然也没有频谱可看。
    let viz = deck
        .player
        .as_ref()
        .as_ref()
        .ok()
        .map(audio::Player::visualizer);
    (viz, lyrics, cover)
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
) -> (crate::viz::Source, LyricFeed, CoverFeed) {
    // 状态行而不是提示:wasm 上这句永远为真,它就是这一端的播放状态。
    ui.set_playback_text("Web 端暂不支持播放".into());
    (None, LyricFeed, CoverFeed::default())
}

/// 「Web 端暂不支持播放」里的中文也得在子集字体里 —— 但它只在 wasm 上出现,
/// [`playback_copy_only_uses_subset_glyphs`] 那条守不到。这个常量把它摆到
/// 原生也能看见的地方,好让同一个测试覆盖。
#[cfg(test)]
const WASM_NOTICE: &str = "Web 端暂不支持播放";

/// 把当前打开的那个歌单的曲目重取一遍。
///
/// 加歌、删歌之后走这里:服务端已经变了,而界面上那一批还是改之前的。
/// 乐观更新在这里不划算 —— 加进来的那批要重新格式化、还要标红心,
/// 而这是一次本机往返。
#[cfg(not(target_arch = "wasm32"))]
fn reload_open_playlist(ui: &MainWindow, deck: &Deck) {
    let Some((source, id)) = deck.editing.current() else {
        return;
    };
    let weak = ui.as_weak();
    fetch_into(&weak, deck, async move {
        crate::playlist::tracks_of(source, &id).await
    });
}

/// 某个歌单叫什么。先找「我的歌单」,再找搜索结果。
///
/// 两张列表都要找:搜到的歌单点开走的是同一条路,只是它不在「我的歌单」里 ——
/// 只找一张的现象是从搜索结果点进去,标题写着「歌单」。
#[cfg(not(target_arch = "wasm32"))]
fn playlist_name(
    ui: &MainWindow,
    id: &slint::SharedString,
    source: i32,
) -> slint::SharedString {
    let matches = |row: &crate::PlaylistRow| {
        row.id == id && row.source == source
    };

    ui.get_playlists()
        .iter()
        .find(matches)
        .or_else(|| {
            ui.get_found_playlists().iter().find(matches)
        })
        .map_or_else(
            || slint::SharedString::from("歌单"),
            |row| row.name.clone(),
        )
}

/// 搜索:关键词 → 三条路由之一 → 对应的一列结果。
///
/// 页签与关键词的记账在 [`crate::search`],这里只交出「搜歌」那一路 ——
/// 它要往播放队列里塞东西,而队列归这个模块。
#[cfg(not(target_arch = "wasm32"))]
fn bind_search(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();

    crate::search::bind(ui, move |ui, keyword| {
        let deck = deck.clone();
        let weak = ui.as_weak();
        let keyword = keyword.to_owned();

        slint::spawn_local(async move {
            let found = api::search_tracks(&keyword).await;
            let Some(ui) = weak.upgrade() else { return };
            match found {
                // 搜索结果没有「平台给不出详情」这回事:它给什么就是什么
                Ok(dto) => show(
                    &ui,
                    &deck,
                    TracksDto {
                        tracks: dto.tracks,
                        unavailable: 0,
                    },
                ),
                Err(error) => {
                    crate::notice::show(
                        &ui,
                        format!("搜索失败: {error}"),
                    );
                }
            }
        })
        .expect("event loop must be running");
    });
}

/// 今日推荐与我喜欢的音乐。两者只差调哪个请求函数,其余完全相同。
///
/// 外加一个「进了 Music 页」的钩子:那时若当天还没拉过推荐,就替用户拉一次 ——
/// 空着一页只有一行「点一首歌开始」不是个好开局。
#[cfg(not(target_arch = "wasm32"))]
fn bind_list(ui: &MainWindow, deck: &Deck) {
    let daily = deck.clone();
    let weak = ui.as_weak();
    ui.on_daily(move || fetch_daily(&weak, &daily));

    let liked = deck.clone();
    let weak = ui.as_weak();
    ui.on_liked(move || {
        fetch_into(&weak, &liked, async {
            api::liked().await
        });
    });

    // 二级导航换了分区。四个分区各自对应一次取数,映射写在**一处** ——
    // 分散在四个回调里的话,加第五个分区时必然漏掉某一处。
    let sectioned = deck.clone();
    let weak = ui.as_weak();
    ui.on_select_section(move |section| {
        if let Some(ui) = weak.upgrade() {
            ui.set_music_section(section);
            // 换分区回到歌单**列表**那一层。不清的话,从别处回到「我的歌单」
            // 看到的是上次点开的那个歌单 —— 这一节的入口行为就不稳定了。
            ui.set_open_playlist_name(
                slint::SharedString::new(),
            );
        }
        load_section(&weak, &sectioned, section);
    });

    // 打开一个歌单:记下来源与 id,再按来源取它的曲目。
    let opened = deck.clone();
    let weak = ui.as_weak();
    ui.on_open_playlist(move |id, source| {
        let Some(ui) = weak.upgrade() else { return };
        // 顺手把红心集合重拉一次:在手机官方 App 里改过的红心,这边只有
        // 重启才跟得上 —— 那个集合原本整个进程只拉一次。接口很轻(一次
        // 全量 id),而每次进歌单都要用它决定每行的心画哪一态。
        ui.invoke_refresh_liked();
        // 标题从列表那一行取 —— 详情页要显示它,而 Rust 侧已经有这份数据了。
        // 两张列表都找:搜到的歌单点开走的是同一条路,只是它不在「我的歌单」里。
        let name = playlist_name(&ui, &id, source);
        ui.set_open_playlist_name(name);

        let source =
            crate::playlist::Source::from_index(source);
        let id = id.to_string();

        // 存下**现在**列表里那一批 —— 下一行就要把它换成这个歌单自己的歌了,
        // 而「把刚才那批加进来」要的正是它。
        let previous = opened.tracks.borrow().clone();
        let count = previous.len();
        opened.editing.opened(source, &id, previous);

        let editable = crate::playlist::is_editable(source);
        ui.set_open_playlist_local(editable);
        // 详情页那张封面按标识索引 —— 名字会重复,两个歌单可以同名
        ui.set_open_playlist_id(id.as_str().into());
        ui.set_open_playlist_cover(
            opened.artwork.get(&id).unwrap_or_default(),
        );
        ui.set_add_batch_text(
            if editable {
                crate::playlist::add_batch_text(count)
            } else {
                String::new()
            }
            .into(),
        );
        fetch_into(&weak, &opened, async move {
            crate::playlist::tracks_of(source, &id).await
        });
    });

    let closing = deck.clone();
    let weak = ui.as_weak();
    ui.on_close_playlist(move || {
        if let Some(ui) = weak.upgrade() {
            closing.editing.closed();
            ui.set_open_playlist_name(
                slint::SharedString::new(),
            );
            ui.set_open_playlist_local(false);
            ui.set_add_batch_text(
                slint::SharedString::new(),
            );
        }
    });

    // 打开一位歌手:摆他此刻的热门曲目。走的是与歌单详情完全相同的那一层 ——
    // 摊开之后两者都是「一批歌」,再造一套详情页只会让返回键有两种写法。
    let artist = deck.clone();
    let weak = ui.as_weak();
    ui.on_open_artist(move |id, name| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_open_playlist_name(name);

        let id = id.to_string();
        fetch_into(&weak, &artist, async move {
            api::artist_tracks(&id).await
        });
    });

    let shown = deck.clone();
    let weak = ui.as_weak();
    ui.on_music_shown(move || {
        // 当天拉过就什么都不做 —— 搜完歌切出去再回来,搜索结果因此保得住。
        if daily_is_due(
            shown.last_daily.get().as_ref(),
            &chrono::Local::now().date_naive(),
        ) {
            fetch_daily(&weak, &shown);
        }
    });
}

/// Music 页的四个分区。编号即 `musicnav.slint` 里 `MusicSections.items` 的下标。
///
/// 两处手工对齐:那边加一项,这里就要多一个分支。做成枚举而不是散在各处的
/// 魔数,是为了让「漏了一个分区」变成编译错误而不是运行时的一片空白。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Daily,
    Playlists,
    Search,
    Recent,
}

// 门要加在 impl 上,不能只加在方法上:方法没了,`impl Section` 这行还在,
// 而 wasm 上根本没有 Section 这个类型(见上面枚举的同一道门)。
#[cfg(not(target_arch = "wasm32"))]
impl Section {
    /// 由界面给的编号认出分区。认不出的编号当每日推荐 ——
    /// 那是开局那一页,总比留在原地什么都不发生强。
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Playlists,
            2 => Self::Search,
            3 => Self::Recent,
            _ => Self::Daily,
        }
    }
}

/// 换到某个分区时该取什么。
#[cfg(not(target_arch = "wasm32"))]
fn load_section(
    weak: &slint::Weak<MainWindow>,
    deck: &Deck,
    section: i32,
) {
    match Section::from_index(section) {
        Section::Daily => fetch_daily(weak, deck),
        Section::Recent => {
            fetch_into(weak, deck, async {
                api::recent().await
            });
        }
        // 歌单分区摆的是歌单列表,不是一批歌 —— 曲目要等用户点开某一个。
        Section::Playlists => {
            if let Some(ui) = weak.upgrade() {
                crate::playlist::refresh(
                    &ui,
                    &deck.artwork,
                );
            }
        }
        // 搜索不自动取:没有关键词,打一次空搜索只会得到一片空白。
        Section::Search => {}
    }
}

/// 拉当日推荐,并记下拉取的日期。
///
/// 日期在**发出**请求时就戳上,而不是等结果回来:失败了也算今天试过,
/// 否则请求一失败,此后每次进 Music 页都会再打一次。手动按 Daily 仍能重试。
#[cfg(not(target_arch = "wasm32"))]
fn fetch_daily(
    weak: &slint::Weak<MainWindow>,
    deck: &Deck,
) {
    deck.last_daily
        .set(Some(chrono::Local::now().date_naive()));
    fetch_into(weak, deck, async { api::daily().await });
}

/// 跑一个返回曲目列表的请求,结果填进列表,失败填进状态行。
///
/// 收的是 `Vec<TrackDto>` 而非线上的信封类型:`ui` 按分层不直接依赖 `contract`,
/// 剥壳在调用处一句 `.map(|dto| dto.tracks)` 完成。
///
/// 三个入口(搜索/推荐/红心)填的是**同一个**列表 ——
/// 换一个来源就整批换掉,不合并:合并了就说不清列表里这首是哪来的。
#[cfg(not(target_arch = "wasm32"))]
fn fetch_into<Fut>(
    weak: &slint::Weak<MainWindow>,
    deck: &Deck,
    request: Fut,
) where
    Fut: core::future::Future<
            Output = Result<TracksDto, api::ApiError>,
        > + 'static,
{
    let deck = deck.clone();
    let weak = weak.clone();
    slint::spawn_local(async move {
        let found = request.await;
        let Some(ui) = weak.upgrade() else { return };
        match found {
            Ok(found) => show(&ui, &deck, found),
            // 会话失效要把人送回登录页,而不是在音乐页上写一句"失败" ——
            // 那句话解释不了为什么什么都拉不出来。已经送回去了就不再报错。
            Err(error)
                if crate::account::handle_session_expiry(
                    &ui, &error,
                ) => {}
            Err(error) => crate::notice::show(
                &ui,
                format!("取曲目失败: {error}"),
            ),
        }
    })
    .expect("event loop must be running");
}

/// 把一批曲目同时装进 Slint 的 model 和 Rust 侧的权威副本。
#[cfg(not(target_arch = "wasm32"))]
fn show(ui: &MainWindow, deck: &Deck, found: TracksDto) {
    // 平台给不出详情的那些没能进这一批。说一声,否则歌单静默变短
    ui.set_unavailable_note(
        crate::playlist::unavailable_text(
            found.unavailable,
        )
        .into(),
    );
    *deck.tracks.borrow_mut() = found.tracks;
    let loading =
        loading_id(deck.playback.borrow().state())
            .map(str::to_owned);
    push_rows(ui, deck, loading.as_deref());
}

/// 重推一遍列表,把 `loading` 那一行标成加载中。
///
/// 加载中的 id 由调用方给,而不是就地读 `playback`:点下去的那一刻状态还没
/// 写进去(`app_core::play` 在 spawn 出去的协程里才 `begin`),读它会标错行。
#[cfg(not(target_arch = "wasm32"))]
fn push_rows(
    ui: &MainWindow,
    deck: &Deck,
    loading: Option<&str>,
) {
    let rows = to_rows(&deck.tracks.borrow(), loading);
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
    // 换了一批歌就重标一遍红心 —— 少了这一步,心的状态会停在上一批。
    crate::liked::remark(&deck.liked, ui);
    // 同理:模型是整个换掉的,新模型里每一行的图都是空的。手上已经有的
    // 那些立刻摆回去,不然标一次加载态就会让满屏封面闪一下。
    deck.thumbnails.apply(ui);
}

/// 接上「这一行要封面」。
///
/// 行滑进可见区时由 `.slint` 那边报过来 —— 列表虚拟化之后,「哪一行现在是哪一
/// 首」只有界面知道(见 tracklist.slint 里 `changed wanted` 那一段)。
#[cfg(not(target_arch = "wasm32"))]
fn bind_needs_cover(ui: &MainWindow, deck: &Deck) {
    let thumbnails = deck.thumbnails.clone();
    let weak = ui.as_weak();

    ui.on_needs_cover(move |url| {
        let Some(ui) = weak.upgrade() else { return };
        thumbnails.request(&ui, &url);
    });
}

/// 点一首歌:这一批成为队列、从这首开始放(见 `CONTEXT.md`「队列」)。
#[cfg(not(target_arch = "wasm32"))]
fn bind_play(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.on_play(move |id| {
        let Some(ui) = weak.upgrade() else { return };

        // 这一首已经在加载了:这一下是多余的,直接丢掉。不挡的话,连点五下
        // 就是五条在途下载,每条回来都往播放器里塞一次源,声音从头响五遍。
        let redundant = is_redundant_tap(
            deck.playback.borrow().state(),
            &id,
        );
        if redundant {
            return;
        }

        // 点歌是播放动作:正在收听的话,先退出(CONTEXT.md「听众」)。
        if deck.sync.is_listening() {
            deck.sync.leave();
        }

        let id = id.to_string();
        let batch = deck.tracks.borrow().clone();
        let Some(index) =
            batch.iter().position(|track| track.id == id)
        else {
            return;
        };

        // replace 把随机清掉(新批还没洗过),开着的话补洗一次把它立回去。
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
        toggle_play(&ui, &toggle);
        // 暂停图标不该慢一拍 —— 轮询要 1 秒之后才轮到。
        crate::media::push(
            &ui,
            &toggle.playback,
            &toggle.media,
        );
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
        let on = {
            let mut queue = shuffle.queue.borrow_mut();
            if queue.is_shuffled() {
                queue.unshuffle();
            } else {
                queue.shuffle(shuffle_seed());
            }
            queue.is_shuffled()
        };
        // 界面上那个开关是这一位的投影,拨完由这里写回去 —— 开关自己不置位。
        ui.set_shuffle_on(on);
        // 系统控件上的随机也该立刻跟着翻,轮询要 1 秒之后才轮到。
        crate::media::push(
            &ui,
            &shuffle.playback,
            &shuffle.media,
        );
    });

    let looper = deck.clone();
    let weak = ui.as_weak();
    ui.on_loop_cycled(move || {
        let Some(ui) = weak.upgrade() else { return };
        use app_core::LoopMode;
        // 关→列表→单曲→关:单键三态,读的是队列里的真相,不是界面属性。
        let next = match looper.queue.borrow().loop_mode()
        {
            LoopMode::Off => LoopMode::All,
            LoopMode::All => LoopMode::One,
            LoopMode::One => LoopMode::Off,
        };
        apply_loop(&ui, &looper, next);
    });

    let setter = deck.clone();
    let weak = ui.as_weak();
    ui.on_loop_mode_set(move |mode| {
        let Some(ui) = weak.upgrade() else { return };
        apply_loop(
            &ui,
            &setter,
            crate::media::loop_from_index(mode),
        );
    });
}

/// 循环模式落到队列,把投影写回界面,并立刻推给系统媒体控件 ——
/// 轮询要 1 秒之后才轮到,锁屏上的键不该慢一拍。
#[cfg(not(target_arch = "wasm32"))]
fn apply_loop(
    ui: &MainWindow,
    deck: &Deck,
    mode: app_core::LoopMode,
) {
    deck.queue.borrow_mut().set_loop_mode(mode);
    ui.set_loop_mode(crate::media::loop_index(mode));
    crate::media::push(ui, &deck.playback, &deck.media);
}

/// 取走备好的那一份 —— **只在它确实是这一首时**。
///
/// 不是这一首就地丢掉:用户中途点了列表里别的歌、或洗了牌,备的那一份再也用不上,
/// 留着只是占一个临时文件和一条还在跑的下载。认错了则更糟 —— 会放出一首
/// 根本没点过的歌。
///
/// 泛型是为了能单独测这道校验:备好的那一份是解码器,测试里造不出来,
/// 而这里唯一的逻辑是"id 对不对得上",与备的是什么东西无关。
fn take_prefetched<T>(
    slot: &RefCell<Option<(String, T)>>,
    wanted: &str,
) -> Option<T> {
    let (id, ready) = slot.borrow_mut().take()?;
    (id == wanted).then_some(ready)
}

/// 备下一首:与正常播放走**同一个** [`prepare`],只是备好了先搁着。
///
/// 失败不声张:预取只是提速,它没成的话照常走原路,用户什么都不会察觉。
#[cfg(not(target_arch = "wasm32"))]
fn start_prefetch(deck: &Deck) {
    let Some(track) =
        deck.queue.borrow().peek_next().cloned()
    else {
        return;
    };

    deck.prefetching.set(true);
    let deck = deck.clone();
    slint::spawn_local(async move {
        let ready =
            prepare(deck.player.clone(), track.clone())
                .await;
        deck.prefetching.set(false);
        match ready {
            Ok((decoded, health)) => {
                log::debug!("预取就绪: {}", track.title);
                *deck.prefetched.borrow_mut() =
                    Some((track.id, (decoded, health)));
            }
            Err(error) => {
                log::debug!("预取没成,照常走: {error}");
            }
        }
    })
    .expect("event loop must be running");
}

/// 手动「下一首」:队列前进一首;到底了就停下并说明
/// (循环语义见 `CONTEXT.md`「队列」)。
#[cfg(not(target_arch = "wasm32"))]
fn advance(ui: &MainWindow, deck: &Deck) {
    let has_next = deck
        .queue
        .borrow_mut()
        .next(shuffle_seed())
        .is_some();
    after_advance(ui, deck, has_next);
}

/// 播完一首的自动推进:与手动只差队列入口 —— 单曲循环时留在本曲重放。
#[cfg(not(target_arch = "wasm32"))]
fn advance_auto(ui: &MainWindow, deck: &Deck) {
    let has_next = deck
        .queue
        .borrow_mut()
        .advance_auto(shuffle_seed())
        .is_some();
    after_advance(ui, deck, has_next);
}

/// 推进之后的收尾:有歌就放,没有就停。
#[cfg(not(target_arch = "wasm32"))]
fn after_advance(
    ui: &MainWindow,
    deck: &Deck,
    has_next: bool,
) {
    if has_next {
        play_current(ui, deck);
    } else {
        // 状态机也要停:不停的话它仍是 Playing,自动续播每秒都会再撞进来。
        deck.playback.borrow_mut().stop();
        ui.set_playback_text(QUEUE_DONE.into());
        ui.set_is_playing(false);
    }
}

/// ⏯ 这一下:退出收听 / 暂停 / 继续 / 重放。
///
/// 从回调里抽出来,是因为系统媒体控件按的也是这一下 —— 那边不该有第二套说法
/// (见 [`dispatch_media`])。
#[cfg(not(target_arch = "wasm32"))]
fn toggle_play(ui: &MainWindow, deck: &Deck) {
    if deck.sync.is_listening() {
        deck.sync.leave();
        if let Ok(player) = deck.player.as_ref() {
            player.stop();
        }
        ui.set_is_playing(false);
        return;
    }

    let Ok(player) = deck.player.as_ref() else {
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
        play_current(ui, deck);
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

    // 备好的那一份先取走 —— 取不到就是走原路。这一步要在停旧歌之前:
    // 备着的话下面那段等待根本不存在,声音接上就换。
    let ready =
        take_prefetched(&deck.prefetched, &track.id);
    let instant = ready.is_some();

    // **旧歌立刻停。** 界面下面几行就要换成新歌了,让耳朵继续听上一首是自相矛盾
    // ——「封面换了但还在放上一首」正是这么来的。备好了的话这一停是零长度的。
    if let Ok(player) = deck.player.as_ref() {
        player.stop();
    }
    ui.set_is_playing(false);
    ui.set_now_loading(!instant);

    // spawn_local 的 future 要到下一轮事件循环才跑,而 Loading 要立刻显示。
    //
    // 状态先写进 `playback`,再让状态行与列表都从它读:列表那一行的加载态
    // 是用户手指底下唯一看得见的反馈,晚一帧就等于没有。
    ui.set_playback_text(
        describe_playback(&PlaybackState::Loading(
            track.clone(),
        ))
        .into(),
    );
    push_rows(ui, deck, Some(&track.id));

    // 播放页的歌名与封面。旧封面立刻清掉 —— 新歌配旧图比空着更误导。
    ui.set_now_title(track.title.clone().into());
    ui.set_now_artists(join_artists(&track.artists).into());
    ui.set_cover_art(slint::Image::default());

    // 歌词也随歌换:先清空(旧歌词配新歌比空着更误导),取到再整批换上。
    // 取不到不影响播放 —— 没歌词是正常状态,不是故障(见 crates/contract)。
    deck.lyrics.replace(Vec::new());
    ui.set_lyric_line(slint::SharedString::new());
    ui.set_lyric_translation(slint::SharedString::new());
    {
        let lyrics = deck.lyrics.clone();
        let id = track.id.clone();
        slint::spawn_local(async move {
            if let Ok(dto) = api::lyric(&id).await {
                lyrics.replace(dto.lines);
            }
        })
        .expect("event loop must be running");
    }

    // 点云也跟着清:与上面那两样同一条原则。少了这一步,取封面的那几百毫秒里
    // 点云仍是上一首;而封面取不到时(CDN 会过期、有的歌根本没有封面)它会
    // **一直**是上一首(见 `docs/adr/0014` 与 `CONTEXT.md`「封面点云」)。
    deck.cover.clear();
    // 极光的封面色同理:旧色配新歌比主题绿更误导(aurora.rs)。
    crate::aurora::reset(ui);
    // 媒体控件那份同理:锁屏上挂着上一首的封面,比空着更误导。
    deck.media.clear_art();

    if let Some(url) = track.cover.clone() {
        let weak = ui.as_weak();
        let cover = deck.cover.clone();
        let media = deck.media.clone();
        let playback = deck.playback.clone();
        let id = track.id.clone();
        slint::spawn_local(async move {
            // 拿不到或解不出就保持空图:封面 CDN 会过期,失败是常态(见 cover.rs)。
            // 同一次解码喂两处:界面的封面卡,以及点云的采样纹理。
            if let Ok(bytes) = api::fetch_bytes(&url).await
                && let Some((img, pixels)) =
                    crate::cover::decode(&bytes)
                && let Some(ui) = weak.upgrade()
            {
                // 连按下一首时,先发的请求可能后回来。到这时它已经不是当前这首,
                // 换上去就是「A 的封面配 B 的歌」—— 与 `app_core::play` 的代际
                // 校验同一个道理,只是这里对得上 id 就够了。
                if current_id(playback.borrow().state())
                    != Some(id.as_str())
                {
                    return;
                }
                ui.set_cover_art(img);
                // 一张图四个去处:封面卡、点云、媒体控件,以及极光的三团光斑。
                // `Arc` 免掉后面几个各拷一份兆级字节。
                let pixels = Arc::new(pixels);
                crate::aurora::feed(&ui, &pixels);
                cover.replace(pixels.clone());
                media.set_art(pixels);
                crate::media::push(&ui, &playback, &media);
            }
        })
        .expect("event loop must be running");
    }

    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let commit = deck.clone();
        let player = deck.player.clone();
        app_core::play(
            &deck.playback,
            track,
            move |track| async move {
                // 备好了就直接交出去 —— 与现取的那份走同一个类型、同一段提交路径,
                // 差别只有"等不等"。
                match ready {
                    Some(ready) => Ok(ready),
                    None => prepare(player, track).await,
                }
            },
            move |(decoded, health)| {
                emit(
                    &commit.player,
                    &commit.sync,
                    &commit.stream,
                    &commit.seeking,
                    decoded,
                    health,
                );
            },
        )
        .await;

        if let Some(ui) = weak.upgrade() {
            let (playing, text) = {
                let state = deck.playback.borrow();
                (
                    matches!(
                        state.state(),
                        PlaybackState::Playing(_)
                    ),
                    describe_playback(state.state()),
                )
            };
            ui.set_is_playing(playing);
            ui.set_playback_text(text.into());
            ui.set_now_loading(false);
            // 放起来了就把断流横幅收掉:声音回来了,那句话已经过期。
            if playing {
                ui.set_banner_text(
                    slint::SharedString::new(),
                );
            }
            // 这一首要么放起来了、要么失败了,行上的加载态该收了。
            // 被顶掉的那次连这里都到不了 —— `app_core::play` 提前返回。
            push_rows(&ui, &deck, None);
            // 换歌立刻报出去。等下一次轮询是 1 秒之后,锁屏上会慢半拍。
            crate::media::push(
                &ui,
                &deck.playback,
                &deck.media,
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
            let (drained, position) =
                match deck.player.as_ref() {
                    Ok(player) => {
                        (player.empty(), player.position())
                    }
                    Err(_) => {
                        (true, core::time::Duration::ZERO)
                    }
                };
            // 卡住时这一行是唯一的判据:位置冻住而没放空 = 声卡回调被网络读堵住
            // (见 `audio` 的 PREFETCH_BYTES);位置反复归零 = 这一首被重放了。
            // 两种症状听起来一模一样,数出来才分得开。
            log::debug!(
                "自动续播轮询: 位置 {position:?}, 放空 {drained}"
            );

            let gave_up = deck
                .stream
                .borrow()
                .as_ref()
                .is_some_and(audio::StreamHealth::gave_up);
            let state = deck.playback.borrow().state().clone();
            let listening = deck.sync.is_listening();

            // 进度搭这趟车,不另起一个定时器:位置已经在上面取过了,
            // 而两个定时器意味着两套"现在放到哪"的说法。
            push_progress(&ui, &state, position);
            push_seek_state(&ui, &deck);
            // 媒体控件搭同一趟车。它自己去重,平帧推出去的是零个字节。
            crate::media::push(&ui, &deck.playback, &deck.media);

            // 断流先判:两个出口在同一刻都可能成立,而断了就不该切歌 ——
            // 网没了下一首同样放不出来,一分钟能把整个队列烧光。
            if should_report_loss(
                &state, drained, listening, gave_up,
            ) {
                report_stream_loss(&ui, &deck);
            } else if should_advance(&state, drained, listening)
            {
                advance_auto(&ui, &deck);
            }

            // 备下一首。判据抽在 `should_prefetch`,这里只负责把当下的事实凑齐。
            let already_have = deck.prefetching.get()
                || deck.prefetched.borrow().is_some();
            let has_next =
                deck.queue.borrow().peek_next().is_some();
            if should_prefetch(
                &state,
                position,
                listening,
                already_have,
                has_next,
            ) {
                start_prefetch(&deck);
            }
        },
    );

    // ponytail: 定时器与进程同寿,leak 掉省一条把 Timer 递回平台入口的通道;
    // 真要按页开关时再把它挂到 Deck 上管理。
    Box::leak(Box::new(timer));
}

/// 把当前进度推给界面。
///
/// 手上没歌时清成「没有」而不是留着上一首的数字 —— 停下之后那条进度条
/// 还停在 3:41,读起来像是还在放。
#[cfg(not(target_arch = "wasm32"))]
fn push_progress(
    ui: &MainWindow,
    state: &PlaybackState,
    position: core::time::Duration,
) {
    let track = match state {
        PlaybackState::Playing(track)
        | PlaybackState::Loading(track) => track,
        _ => {
            ui.set_has_track(false);
            return;
        }
    };

    let secs = position.as_secs_f64();
    ui.set_has_track(true);
    ui.set_progress_ratio(crate::progress::ratio(
        secs,
        track.duration_ms,
    ));
    ui.set_progress_text(
        crate::progress::progress_text(
            secs,
            track.duration_ms,
        )
        .into(),
    );
}

/// 接上音量:开局从本地设置恢复,拖动时既改播放器也存回去。
///
/// 音量跟着设备走,不跟着账号 —— 笔记本外放与一副耳机不该共用一个数值
/// (见 api::settings)。
#[cfg(not(target_arch = "wasm32"))]
fn bind_volume(ui: &MainWindow, deck: &Deck) {
    let saved = api::settings::load().volume;
    ui.set_volume(saved);
    if let Ok(player) = deck.player.as_ref() {
        player.set_volume(saved);
    }

    let deck = deck.clone();
    let weak = ui.as_weak();
    ui.on_volume_changed(move |volume| {
        let volume = audio::clamped_volume(volume);
        if let Some(ui) = weak.upgrade() {
            ui.set_volume(volume);
        }
        if let Ok(player) = deck.player.as_ref() {
            player.set_volume(volume);
        }

        // 每动一下就存:调音量是个连续动作,而"什么时候算调完了"没有信号。
        // 写的是本地一个几十字节的文件,存不下也只是下次回到默认值。
        //
        // **先读再改**:整份重造的话,这个文件里别的设置(明暗)会被这次
        // 调音量顺手冲回默认值。
        api::settings::save(&api::settings::Settings {
            volume,
            ..api::settings::load()
        });
    });
}

/// 接上进度条的拖动。
///
/// 跳转有**两种下场,两条报告路径**(见 `audio::ChannelSource::try_seek`):
///
/// - 当场就知道跳不动(格式不支持、这条流只进不退):`seek` 直接返回 `Err`,
///   这里当场说。那一刻 rodio 的位置计数器根本没动过,进度条与声音仍然一致。
/// - 真在取字节:`seek` 乐观返回 `Ok`,这里挂上「缓冲中」,结论由每秒那趟
///   轮询从 `audio::SeekState` 上取(`push_seek_state`)。
#[cfg(not(target_arch = "wasm32"))]
fn bind_seek(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.on_seek(move |at| {
        let Some(ui) = weak.upgrade() else { return };
        let state = deck.playback.borrow().state().clone();
        let (PlaybackState::Playing(track)
        | PlaybackState::Loading(track)) = state
        else {
            return;
        };
        let Some(target) = crate::progress::seek_target(
            at,
            track.duration_ms,
        ) else {
            return;
        };

        // 立刻挂上,不等轮询:那要慢一秒,而一秒的沉默正好是"点了没反应"
        ui.set_buffering(true);

        if let Ok(player) = deck.player.as_ref()
            && let Err(err) = player.seek(target)
        {
            ui.set_buffering(false);
            crate::notice::show(
                &ui,
                format!("这首跳不了: {err}"),
            );
        }
    });
}

/// 把跳转的下场推给界面。
///
/// 两个出口而不是一个:还在取字节 -> 挂着「缓冲中」;试过了不行 -> 摘掉缓冲
/// 并说一句为什么。少了后一条,跳不了的歌会永远停在「缓冲中」上,
/// 而那比一开始就说"跳不了"更难查。
#[cfg(not(target_arch = "wasm32"))]
fn push_seek_state(ui: &MainWindow, deck: &Deck) {
    let borrowed = deck.seeking.borrow();
    let Some(state) = borrowed.as_ref() else {
        return;
    };

    if let Some(why) = state.take_failure() {
        ui.set_buffering(false);
        crate::notice::show(
            ui,
            format!("这首跳不了: {why}"),
        );
        return;
    }

    ui.set_buffering(state.is_seeking());
}

/// 声音放到一半没了:停下,弹横幅,再去问清是哪一种没了。
///
/// 先弹粗文案,不等探测 —— 等的话最坏要让用户对着没声音的界面干等二十多秒,
/// 那个区间里他已经在想"是不是卡死了"(见 `docs/adr/0013`)。
#[cfg(not(target_arch = "wasm32"))]
fn report_stream_loss(ui: &MainWindow, deck: &Deck) {
    // 证据取走即清空。这个条件会一直成立到下次换歌,不清的话横幅每秒重弹一次。
    deck.stream.borrow_mut().take();

    let opening = describe_stream_loss(None);
    deck.playback.borrow_mut().fail(opening.to_owned());
    ui.set_is_playing(false);
    ui.set_playback_text(
        describe_playback(deck.playback.borrow().state())
            .into(),
    );
    ui.set_banner_text(opening.into());

    // 探测结果回来了再把话说准。探不通=本机没网,探得通=这条播放地址不行了。
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let reachable = api::health().await.is_ok();
        if let Some(ui) = weak.upgrade() {
            // 期间用户可能已经把横幅关了,或者又放起了别的歌 —— 那就不打扰他。
            if !ui.get_banner_text().is_empty() {
                ui.set_banner_text(
                    describe_stream_loss(Some(reachable))
                        .into(),
                );
            }
        }
    })
    .expect("event loop must be running");
}

/// 开机静默自检:`GET /health` 一次,健康就一声不吭。
///
/// Server 页删掉之后,这是协议版本协商唯一的运行时入口(`api::health` 内部
/// 比对 `PROTOCOL_VERSION`)。
///
/// 坏消息走横幅:这是**开机那一刻**的一次探测,不是一个会自己更新的状态。
/// 写进播放状态行的话,上游恢复之后没有任何东西会重算它,那句「失败: 上游超时」
/// 就一直挂在歌单顶上(见 `crate::notice`)。
#[cfg(not(target_arch = "wasm32"))]
fn startup_check(ui: &MainWindow) {
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let result = api::health().await.map(|_dto| ());
        if let Some(message) = describe_startup(&result)
            && let Some(ui) = weak.upgrade()
        {
            crate::notice::show(&ui, message);
        }
    })
    .expect("event loop must be running");
}

/// 把一首歌准备到「随时能出声」为止:取直链 → 开流 → 解码。**慢**的那一半。
///
/// 这是注入给 `app_core::play` 的 `prepare`。`app-core` 只看到"一个返回 Result
/// 的 future",看不到 HTTP、alsa,也看不到 WebRTC。
///
/// 停在解码,不往下走:再往下就是把源塞进播放器,那一步不可撤销。中间隔着
/// 一次代际校验 —— 准备期间被顶掉的这一份就地丢掉(见 `app_core::play`)。
#[cfg(not(target_arch = "wasm32"))]
async fn prepare(
    player: Arc<Result<audio::Player, audio::AudioError>>,
    track: TrackDto,
) -> Result<(audio::Loaded, audio::StreamHealth), String> {
    // 没声卡就在这里认输,别等下载完才发现放不了。
    if let Err(error) = player.as_ref() {
        return Err(error.to_string());
    }

    let source = api::play_source(&track.id)
        .await
        .map_err(|error| error.to_string())?;
    // 开流与解码都在 `audio` 自己的后台 runtime 上跑 —— 这里是 Slint 的 UI 线程,
    // 没有 tokio 反应堆,也不能被阻塞读占住。
    audio::load(&source.url)
        .await
        .map_err(|error| error.to_string())
}

/// 把备好的源交给播放器与同播。**不可撤销**的那一半,同步、立刻生效。
///
/// 每首歌都分一支给同播,不管当下有没有人在听:支路满了会自己丢采样
/// (见 `audio::codec::Tee`),而等"确认有人听"再接的话,换歌时听众会掉音。
///
/// 无声卡时这里什么都不做 —— 那种情况 [`prepare`] 已经先报了错,走不到这里。
#[cfg(not(target_arch = "wasm32"))]
fn emit(
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    sync: &crate::syncplay::Sync,
    stream: &Rc<RefCell<Option<audio::StreamHealth>>>,
    seeking: &Rc<RefCell<Option<audio::SeekState>>>,
    decoded: audio::Loaded,
    health: audio::StreamHealth,
) {
    use audio::buffered;
    use audio::codec::{BRANCH_CAPACITY, Tee, normalize};

    let Ok(player) = player.as_ref() else { return };
    // 换歌即换证据。上一首的死亡证明留着的话,新歌一放空就会被误报成断流。
    stream.borrow_mut().replace(health);
    // 先归一再缓冲再分支,三步的顺序都是硬的:
    //
    // - 归一在最前:`buffered` 交出的源对外声称 48kHz 立体声,格式得先对上;
    // - 缓冲在中间:它把解码挪到自己的线程,声卡回调从此不碰网络(见
    //   `audio::buffered`)。少了这一层,网络抖一下就是设备欠载;
    // - 分支在最后:本机听到的和推给听众的因此仍是同一批采样。
    let source = buffered(normalize(decoded));
    // 跳转状态得在源被交出去之前取走:此后它归 rodio,外面再也够不着。
    seeking.borrow_mut().replace(source.seek_state());
    let (tee, branch) = Tee::new(source, BRANCH_CAPACITY);
    // 先换歌再交支路。反过来的话,新泵在上一首还没被丢掉时就起来了,
    // 两条泵会同时往同一条轨上写,听众听到的是两首歌交错的几十毫秒。
    player.play(tee);
    sync.feed(branch);
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

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
        assert!(matches!(
            feed.take(),
            CoverUpdate::Unchanged
        ));
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
        assert!(matches!(
            feed.take(),
            CoverUpdate::Unchanged
        ));
    }

    /// 边长 `side` 的纯色封面像素,只用来分辨是哪一张。
    fn pixels(side: u32) -> crate::viz::CoverPixels {
        crate::viz::CoverPixels {
            width: side,
            height: side,
            rgba: vec![0; (side * side * 4) as usize],
        }
    }

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

    /// 指定 id 的一首歌,用来分辨列表里的行。
    fn track_with_id(id: &str) -> TrackDto {
        TrackDto {
            id: id.to_owned(),
            title: format!("歌 {id}"),
            ..track()
        }
    }

    /// 正在加载的就是这一首:再点它是多余的。
    ///
    /// 用户会连点,正是因为网络慢时点了看不出反应 —— 每一下都发一次
    /// 下载、每条回来都从头出声,这是 bug 不是"手快"。
    #[test]
    fn tapping_the_track_that_is_already_loading_is_redundant()
     {
        assert!(is_redundant_tap(
            &PlaybackState::Loading(track_with_id("1")),
            "1"
        ));
    }

    /// 正在加载别的歌:这一下该换歌,不算多余。
    #[test]
    fn tapping_a_different_track_while_loading_is_not_redundant()
     {
        assert!(!is_redundant_tap(
            &PlaybackState::Loading(track_with_id("1")),
            "2"
        ));
    }

    /// 已经在放这一首:再点是「从头听」,照旧生效。
    #[test]
    fn tapping_the_playing_track_is_not_redundant() {
        assert!(!is_redundant_tap(
            &PlaybackState::Playing(track_with_id("1")),
            "1"
        ));
    }

    /// 空闲与失败态下的点击一律照常 —— 失败之后重试是常见动作。
    #[test]
    fn tapping_while_idle_or_failed_is_not_redundant() {
        assert!(!is_redundant_tap(
            &PlaybackState::Idle,
            "1"
        ));
        assert!(!is_redundant_tap(
            &PlaybackState::Failed("直链已过期".to_owned()),
            "1"
        ));
    }

    /// 从没拉过:该拉。
    #[test]
    fn daily_has_never_been_fetched_so_it_is_due() {
        assert!(daily_is_due(None, &20_260_730));
    }

    /// 今天拉过了:不动 —— 搜索结果因此保得住。
    #[test]
    fn daily_fetched_today_is_not_due() {
        assert!(!daily_is_due(
            Some(&20_260_730),
            &20_260_730
        ));
    }

    /// 拉的是昨天的:跨天失效,该重拉。
    #[test]
    fn daily_fetched_on_an_earlier_day_is_due() {
        assert!(daily_is_due(
            Some(&20_260_729),
            &20_260_730
        ));
    }

    /// 只标出加载中的那一行,其余行不受影响。
    #[test]
    fn only_the_loading_track_row_is_marked() {
        let batch = [
            track_with_id("1"),
            track_with_id("2"),
            track_with_id("3"),
        ];
        let rows = to_rows(&batch, Some("2"));
        assert_eq!(
            rows.iter()
                .map(|row| row.loading)
                .collect::<Vec<_>>(),
            vec![false, true, false]
        );
    }

    /// 没有歌在加载时,一行都不标。
    #[test]
    fn no_row_is_marked_when_nothing_is_loading() {
        let batch =
            [track_with_id("1"), track_with_id("2")];
        let rows = to_rows(&batch, None);
        assert!(rows.iter().all(|row| !row.loading));
    }

    /// 加载中的歌不在当前列表里(点完歌又搜了别的):一行不标,也不出错。
    #[test]
    fn a_loading_track_outside_the_list_marks_nothing() {
        let batch =
            [track_with_id("1"), track_with_id("2")];
        let rows = to_rows(&batch, Some("99"));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| !row.loading));
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

    /// **断流与放完了要走向相反的出口。**
    ///
    /// 两者在播放器那头是同一个现象(声源空了),区别只有那条流留没留下放弃的
    /// 证据。分不清的话,断网时会一首接一首地切下去,每首再熬一轮超时,
    /// 一分钟就把整个队列烧光,而用户得到的解释是零(见 `docs/adr/0013`)。
    #[test]
    fn a_drained_source_reports_a_loss_only_when_it_gave_up()
     {
        let playing = PlaybackState::Playing(track());

        assert!(
            should_report_loss(&playing, true, false, true),
            "放空了且流放弃过,这就是断流"
        );
        assert!(
            !should_report_loss(
                &playing, true, false, false
            ),
            "放空了但流没放弃,那是正常放完,该切下一首"
        );
        // 两个出口必须互斥,否则同一刻既报错又切歌。
        assert!(
            !should_advance(&playing, true, false)
                || !should_report_loss(
                    &playing, true, false, false
                )
        );
        assert!(
            !should_report_loss(
                &playing, false, false, true
            ),
            "还没放空就报断流,声音还在放呢"
        );
        assert!(
            !should_report_loss(&playing, true, true, true),
            "收听同播时本机没有自己的流,那是上一次留下的旧证据"
        );
        assert!(
            !should_report_loss(
                &PlaybackState::Loading(track()),
                true,
                false,
                true
            ),
            "正在加载下一首时不许被上一首的证据打断"
        );
    }

    /// **预取要等当前这首站稳了再起。**
    ///
    /// 一起播就备的话,两条下载抢同一条链路,而正在放的那首经不起抢 ——
    /// 这个 CDN 本来就爱停摆(真机日志里连续四次失联,见 `docs/adr/0013`)。
    #[test]
    fn prefetch_starts_once_the_current_track_is_under_way()
    {
        let playing = PlaybackState::Playing(track());
        let under_way = PREFETCH_AFTER;
        let just_started = core::time::Duration::ZERO;

        assert!(should_prefetch(
            &playing, under_way, false, false, true
        ));
        assert!(
            !should_prefetch(
                &playing,
                just_started,
                false,
                false,
                true
            ),
            "刚起播就备下一首会和它自己抢带宽"
        );
        assert!(
            !should_prefetch(
                &playing, under_way, false, true, true
            ),
            "手里已经有备好的了,别再起一条"
        );
        assert!(
            !should_prefetch(
                &playing, under_way, false, false, false
            ),
            "队尾之后没有下一首可备"
        );
        assert!(
            !should_prefetch(
                &PlaybackState::Loading(track()),
                under_way,
                false,
                false,
                true
            ),
            "这一首自己还没放起来,轮不到备下一首"
        );
    }

    /// 收听同播时不预取:切歌的决定权不在本机,备了也用不上,白占一条下载。
    #[test]
    fn a_listener_never_prefetches() {
        assert!(!should_prefetch(
            &PlaybackState::Playing(track()),
            PREFETCH_AFTER,
            true,
            false,
            true
        ));
    }

    /// **备好的那一份只认它自己那一首。**
    ///
    /// 备下一首的时候用户可能改主意:点了列表里别的歌,或者洗了牌。认错了会放出
    /// 一首根本没点过的歌 —— 而且界面显示的还是对的那一首,查起来极其别扭。
    #[test]
    fn a_prefetched_track_is_only_used_for_its_own_track() {
        let slot = RefCell::new(Some((
            "2".to_owned(),
            "备好的源",
        )));

        assert!(
            take_prefetched(&slot, "7").is_none(),
            "id 对不上,不许拿来用"
        );
        assert!(
            slot.borrow().is_none(),
            "对不上的那一份要就地丢掉,不能留着占一条下载"
        );

        let slot = RefCell::new(Some((
            "2".to_owned(),
            "备好的源",
        )));
        assert_eq!(
            take_prefetched(&slot, "2"),
            Some("备好的源")
        );
        assert!(
            take_prefetched(&slot, "2").is_none(),
            "同一份不该被交出两次"
        );
    }

    /// 横幅先说发生了什么,探明之后再说是哪一种。
    ///
    /// 三句话必须互不相同:探测没回来时说"中断了"是诚实的,而一旦探明,
    /// 用户该做的事完全不同 —— 一个是等网络,一个是重新点这首歌。
    #[test]
    fn the_banner_says_what_the_user_can_do_about_it() {
        let pending = describe_stream_loss(None);
        let offline = describe_stream_loss(Some(false));
        let stale = describe_stream_loss(Some(true));

        assert!(!pending.is_empty());
        assert_ne!(pending, offline);
        assert_ne!(offline, stale);
        assert_ne!(pending, stale);
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

    /// 四个编号各自认出自己的分区,认不出的落回每日推荐 ——
    /// 那是开局那一页,总比留在原地什么都不发生强。
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn each_section_knows_what_to_load() {
        use super::Section;

        assert_eq!(Section::from_index(0), Section::Daily);
        assert_eq!(
            Section::from_index(1),
            Section::Playlists
        );
        assert_eq!(Section::from_index(2), Section::Search);
        assert_eq!(Section::from_index(3), Section::Recent);
        assert_eq!(Section::from_index(99), Section::Daily);
        assert_eq!(Section::from_index(-1), Section::Daily);
    }
}
