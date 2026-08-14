//! 歌单、分区与搜索结果的装配:哪一批歌进列表,以及列表怎么刷新。

use super::*;
use crate::Library;
use crate::Player;
use crate::Shell;

/// 把当前打开的那个歌单的曲目重取一遍。
///
/// 加歌、删歌之后走这里:服务端已经变了,而界面上那一批还是改之前的。
/// 乐观更新在这里不划算 —— 加进来的那批要重新格式化、还要标红心,
/// 而这是一次本机往返。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn reload_open_playlist(
    ui: &MainWindow,
    deck: &Deck,
) {
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
pub(super) fn playlist_name(
    ui: &MainWindow,
    id: &slint::SharedString,
    source: i32,
) -> slint::SharedString {
    let matches = |row: &crate::PlaylistRow| {
        row.id == id && row.source == source
    };

    ui.global::<Library>()
        .get_playlists()
        .iter()
        .find(matches)
        .or_else(|| {
            ui.global::<Library>()
                .get_found_playlists()
                .iter()
                .find(matches)
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
pub(super) fn bind_search(ui: &MainWindow, deck: &Deck) {
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
pub(super) fn bind_list(ui: &MainWindow, deck: &Deck) {
    let daily = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>()
        .on_daily(move || fetch_daily(&weak, &daily));

    let liked = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_liked(move || {
        fetch_into(&weak, &liked, async {
            api::liked().await
        });
    });

    // 二级导航换了分区。四个分区各自对应一次取数,映射写在**一处** ——
    // 分散在四个回调里的话,加第五个分区时必然漏掉某一处。
    let sectioned = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_select_section(
        move |section| {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Shell>()
                    .set_music_section(section);
                // 换分区回到歌单**列表**那一层。不清的话,从别处回到「我的歌单」
                // 看到的是上次点开的那个歌单 —— 这一节的入口行为就不稳定了。
                ui.global::<Library>()
                    .set_open_playlist_name(
                        slint::SharedString::new(),
                    );
            }
            load_section(&weak, &sectioned, section);
        },
    );

    // 打开一个歌单:记下来源与 id,再按来源取它的曲目。
    let opened = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Library>().on_open_playlist(
        move |id, source| {
            let Some(ui) = weak.upgrade() else { return };
            // 顺手把红心集合重拉一次:在手机官方 App 里改过的红心,这边只有
            // 重启才跟得上 —— 那个集合原本整个进程只拉一次。接口很轻(一次
            // 全量 id),而每次进歌单都要用它决定每行的心画哪一态。
            ui.global::<Library>().invoke_refresh_liked();
            // 标题从列表那一行取 —— 详情页要显示它,而 Rust 侧已经有这份数据了。
            // 两张列表都找:搜到的歌单点开走的是同一条路,只是它不在「我的歌单」里。
            let name = playlist_name(&ui, &id, source);
            ui.global::<Library>()
                .set_open_playlist_name(name);

            let source =
                crate::playlist::Source::from_index(source);
            let id = id.to_string();

            // 存下**现在**列表里那一批 —— 下一行就要把它换成这个歌单自己的歌了,
            // 而「把刚才那批加进来」要的正是它。
            let previous = opened.tracks.borrow().clone();
            let count = previous.len();
            opened.editing.opened(source, &id, previous);

            let editable =
                crate::playlist::is_editable(source);
            ui.global::<Library>()
                .set_open_playlist_local(editable);
            // 详情页那张封面按标识索引 —— 名字会重复,两个歌单可以同名
            ui.global::<Library>()
                .set_open_playlist_id(id.as_str().into());
            ui.global::<Library>().set_open_playlist_cover(
                opened.artwork.get(&id).unwrap_or_default(),
            );
            ui.global::<Library>().set_add_batch_text(
                if editable {
                    crate::playlist::add_batch_text(count)
                } else {
                    String::new()
                }
                .into(),
            );
            fetch_into(&weak, &opened, async move {
                crate::playlist::tracks_of(source, &id)
                    .await
            });
        },
    );

    let closing = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Library>().on_close_playlist(move || {
        if let Some(ui) = weak.upgrade() {
            closing.editing.closed();
            ui.global::<Library>().set_open_playlist_name(
                slint::SharedString::new(),
            );
            ui.global::<Library>()
                .set_open_playlist_local(false);
            ui.global::<Library>().set_add_batch_text(
                slint::SharedString::new(),
            );
        }
    });

    // 打开一位歌手:摆他此刻的热门曲目。走的是与歌单详情完全相同的那一层 ——
    // 摊开之后两者都是「一批歌」,再造一套详情页只会让返回键有两种写法。
    let artist = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Library>().on_open_artist(
        move |id, name| {
            let Some(ui) = weak.upgrade() else { return };
            ui.global::<Library>()
                .set_open_playlist_name(name);

            let id = id.to_string();
            fetch_into(&weak, &artist, async move {
                api::artist_tracks(&id).await
            });
        },
    );

    let shown = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_music_shown(move || {
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
pub(super) fn load_section(
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
pub(super) fn fetch_daily(
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
pub(super) fn fetch_into<Fut>(
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
pub(super) fn show(
    ui: &MainWindow,
    deck: &Deck,
    found: TracksDto,
) {
    // 平台给不出详情的那些没能进这一批。说一声,否则歌单静默变短
    ui.global::<Library>().set_unavailable_note(
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
pub(super) fn push_rows(
    ui: &MainWindow,
    deck: &Deck,
    loading: Option<&str>,
) {
    let rows = to_rows(&deck.tracks.borrow(), loading);
    ui.global::<Player>()
        .set_tracks(ModelRc::new(VecModel::from(rows)));
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
pub(super) fn bind_needs_cover(
    ui: &MainWindow,
    deck: &Deck,
) {
    let thumbnails = deck.thumbnails.clone();
    let weak = ui.as_weak();

    ui.global::<Player>().on_needs_cover(move |url| {
        let Some(ui) = weak.upgrade() else { return };
        thumbnails.request(&ui, &url);
    });
}
