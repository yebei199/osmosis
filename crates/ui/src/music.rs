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
    lyrics: LyricFeed,
    cover: CoverFeed,
    /// 列表里那一批歌的权威副本。Slint 的 model 只存格式化后的字符串,
    /// 点击时要靠它把 id 换回完整的 `TrackDto`;重推行(标加载态)也从它来。
    tracks: Rc<RefCell<Vec<TrackDto>>>,
    /// 上次拉当日推荐的日期。推荐是**当天**的,跨过零点就过期(见 [`daily_is_due`])。
    /// 只活在进程里 —— 重启重拉一次,不落盘。
    last_daily: Rc<std::cell::Cell<Option<chrono::NaiveDate>>>,
}

/// 封面像素的取用口:播放页每帧问它「有没有新封面要送进点云」。
///
/// 只在换歌解出新封面的那一帧交出像素,取走即清空 —— 一张封面是兆级的字节,
/// 每帧搬一次过 seam 纯属白耗(见 `crates/render3d::cloud`)。
#[derive(Clone, Default)]
pub(crate) struct CoverFeed {
    pending: Rc<RefCell<Option<crate::viz::CoverPixels>>>,
}

impl CoverFeed {
    /// 取走待送的封面像素,没有新的就给 `None`。
    pub(crate) fn take(
        &self,
    ) -> Option<crate::viz::CoverPixels> {
        self.pending.borrow_mut().take()
    }

    /// 换歌解出了新封面:排上队等下一帧取走。上一张还没被取走就直接顶掉 ——
    /// 点云只显示当前这一首,过期的封面排队也没人要。
    fn replace(&self, pixels: crate::viz::CoverPixels) {
        *self.pending.borrow_mut() = Some(pixels);
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
) -> (crate::viz::Source, LyricFeed, CoverFeed) {
    let player = Arc::new(audio::Player::new());

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
        player,
        lyrics: lyrics.clone(),
        cover: cover.clone(),
        tracks: Rc::new(RefCell::new(Vec::new())),
        last_daily: Rc::new(std::cell::Cell::new(None)),
    };

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
) -> (crate::viz::Source, LyricFeed, CoverFeed) {
    ui.set_playback_text("Web 端暂不支持播放".into());
    (None, LyricFeed, CoverFeed::default())
}

/// 「Web 端暂不支持播放」里的中文也得在子集字体里 —— 但它只在 wasm 上出现,
/// [`playback_copy_only_uses_subset_glyphs`] 那条守不到。这个常量把它摆到
/// 原生也能看见的地方,好让同一个测试覆盖。
#[cfg(test)]
const WASM_NOTICE: &str = "Web 端暂不支持播放";

/// 搜索:关键词 → `GET /search` → 结果列表。
#[cfg(not(target_arch = "wasm32"))]
fn bind_search(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.on_search(move |keyword| {
        let keyword = keyword.to_string();
        if keyword.trim().is_empty() {
            return;
        }

        let deck = deck.clone();
        let weak = weak.clone();
        slint::spawn_local(async move {
            let found = api::search(&keyword).await;
            let Some(ui) = weak.upgrade() else { return };
            match found {
                Ok(dto) => show(&ui, &deck, dto.tracks),
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
            api::liked().await.map(|dto| dto.tracks)
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

/// 拉当日推荐,并记下拉取的日期。
///
/// 日期在**发出**请求时就戳上,而不是等结果回来:失败了也算今天试过,
/// 否则请求一失败,此后每次进 Music 页都会再打一次。手动按 Daily 仍能重试。
#[cfg(not(target_arch = "wasm32"))]
fn fetch_daily(weak: &slint::Weak<MainWindow>, deck: &Deck) {
    deck.last_daily
        .set(Some(chrono::Local::now().date_naive()));
    fetch_into(weak, deck, async {
        api::daily().await.map(|dto| dto.tracks)
    });
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
            Output = Result<Vec<TrackDto>, api::ApiError>,
        > + 'static,
{
    let deck = deck.clone();
    let weak = weak.clone();
    slint::spawn_local(async move {
        let found = request.await;
        let Some(ui) = weak.upgrade() else { return };
        match found {
            Ok(found) => show(&ui, &deck, found),
            Err(error) => ui.set_playback_text(
                format!("失败: {error}").into(),
            ),
        }
    })
    .expect("event loop must be running");
}

/// 把一批曲目同时装进 Slint 的 model 和 Rust 侧的权威副本。
#[cfg(not(target_arch = "wasm32"))]
fn show(ui: &MainWindow, deck: &Deck, found: Vec<TrackDto>) {
    *deck.tracks.borrow_mut() = found;
    let loading =
        loading_id(deck.playback.borrow().state()).map(str::to_owned);
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

    if let Some(url) = track.cover.clone() {
        let weak = ui.as_weak();
        let cover = deck.cover.clone();
        slint::spawn_local(async move {
            // 拿不到或解不出就保持空图:封面 CDN 会过期,失败是常态(见 cover.rs)。
            // 同一次解码喂两处:界面的封面卡,以及点云的采样纹理。
            if let Ok(bytes) = api::fetch_bytes(&url).await
                && let Some((img, pixels)) =
                    crate::cover::decode(&bytes)
                && let Some(ui) = weak.upgrade()
            {
                ui.set_cover_art(img);
                cover.replace(pixels);
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
            move |track| prepare(player, track),
            move |decoded| {
                emit(&commit.player, &commit.sync, decoded);
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
            // 这一首要么放起来了、要么失败了,行上的加载态该收了。
            // 被顶掉的那次连这里都到不了 —— `app_core::play` 提前返回。
            push_rows(&ui, &deck, None);
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
) -> Result<audio::Loaded, String> {
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
    decoded: audio::Loaded,
) {
    use audio::codec::{BRANCH_CAPACITY, Tee, normalize};

    let Ok(player) = player.as_ref() else { return };
    // 先归一再分支:本机听到的和推出去的因此是同一批采样,
    // 而 Opus 只在 48kHz 立体声上工作(见 `audio::codec::normalize`)。
    let (tee, branch) =
        Tee::new(normalize(decoded), BRANCH_CAPACITY);
    // 先换歌再交支路。反过来的话,新泵在上一首还没被丢掉时就起来了,
    // 两条泵会同时往同一条轨上写,听众听到的是两首歌交错的几十毫秒。
    player.play(tee);
    sync.feed(branch);
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 封面像素只交出一次:换歌那一帧给 `Some`,之后一直给 `None`。
    /// 一张封面是兆级的字节,每帧搬一次过 seam 纯属白耗。
    #[test]
    fn cover_feed_hands_pixels_over_once_per_track() {
        let feed = CoverFeed::default();
        assert!(feed.take().is_none(), "没换歌不该有封面");

        feed.replace(pixels(2));
        assert_eq!(feed.take().map(|p| p.width), Some(2));
        assert!(
            feed.take().is_none(),
            "同一张被交出了两次"
        );
    }

    /// 上一张还没被取走就又换歌:取到的是新的那张。点云只显示当前这一首,
    /// 过期的封面排队也没人要 —— 播放页收起时门是关的,没人来取,连着换几首
    /// 就会攒下一串。
    #[test]
    fn cover_feed_replaces_a_pending_cover() {
        let feed = CoverFeed::default();
        feed.replace(pixels(2));
        feed.replace(pixels(4));
        assert_eq!(feed.take().map(|p| p.width), Some(4));
        assert!(feed.take().is_none());
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
        assert!(!is_redundant_tap(&PlaybackState::Idle, "1"));
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
        assert!(daily_is_due(Some(&20_260_729), &20_260_730));
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
        let batch = [track_with_id("1"), track_with_id("2")];
        let rows = to_rows(&batch, None);
        assert!(rows.iter().all(|row| !row.loading));
    }

    /// 加载中的歌不在当前列表里(点完歌又搜了别的):一行不标,也不出错。
    #[test]
    fn a_loading_track_outside_the_list_marks_nothing() {
        let batch = [track_with_id("1"), track_with_id("2")];
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
