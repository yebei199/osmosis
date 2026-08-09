//! 歌单列表与详情的绑定。
//!
//! 三种来源的歌单在界面上是同一张列表(见 `docs/adr/0016`),但**取曲目的路
//! 不同**:「我喜欢的」是平台的红心列表,本地歌单在自家库里,平台歌单要问上游。
//! 走错的现象是点开一个歌单看到另一个歌单的歌 —— 而两边都不报错。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{PlaylistDto, PlaylistSource, TrackDto};
use slint::ComponentHandle;

use crate::{MainWindow, PlaylistRow};

/// 歌单的来源。数值即 `PlaylistSource` 的顺序,也是 `.slint` 里
/// `PlaylistRow.source` 的取值 —— 三处手工对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Liked,
    Platform,
    Local,
}

impl Source {
    /// 由契约里的来源认出它。
    pub fn from_dto(source: PlaylistSource) -> Self {
        match source {
            PlaylistSource::Liked => Self::Liked,
            PlaylistSource::Platform => Self::Platform,
            PlaylistSource::Local => Self::Local,
        }
    }

    /// 界面用的编号。
    pub fn to_index(self) -> i32 {
        match self {
            Self::Liked => 0,
            Self::Platform => 1,
            Self::Local => 2,
        }
    }

    /// 由界面给的编号认回来。认不出的当平台歌单 ——
    /// 那是三者里唯一只读的,猜错了最多是取不到歌,不会误删本地数据。
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Liked,
            2 => Self::Local,
            _ => Self::Platform,
        }
    }
}

/// 一个歌单的副标题:它有多少首歌。
///
/// 在 Rust 侧格式化好再推给界面 —— `.slint` 里不做计算(见 types.slint)。
/// 空歌单说「暂无曲目」而不是「0 首」:后者读起来像个统计数字,
/// 而这里要说的是"点进去也没东西"。
pub fn track_count_text(count: i32) -> String {
    if count <= 0 {
        "暂无曲目".to_owned()
    } else {
        format!("{count} 首")
    }
}

/// 从 [`track_count_text`] 写出来的那句话里读回条数。
///
/// 读不出来就是 `None`。调用方据此**不动**那个数字 —— 凭空猜一个写上去,
/// 比留着一个旧的更难发现。
///
/// 「暂无曲目」也读不出来,这是**有意**的:它既对应 0,也对应上游给了个负数
/// (见 [`track_count_text`]),两者不该被读成同一个可加减的起点。
pub fn track_count_of(text: &str) -> Option<i32> {
    text.strip_suffix(" 首")?.parse().ok()
}

/// 「把刚才那批加进来」那行的文案。
///
/// 带上条数,因为进歌单那一刻列表已经换掉了 —— 不说清是哪一批,用户点下去
/// 才知道加了什么。空批返回空串,那一行整个不出现。
pub fn add_batch_text(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        format!("+ 把刚才那 {count} 首加进来")
    }
}

/// 这个来源的歌单能不能改。
///
/// 判据是**来源**不是名字:用户完全可以把一个本地歌单起名叫「我喜欢的」,
/// 而平台歌单与红心的真相不在这边,改名删歌都改不动。
pub fn is_editable(source: Source) -> bool {
    matches!(source, Source::Local)
}

/// 取某个歌单的曲目。三种来源各走各的路。
pub async fn tracks_of(
    source: Source,
    id: &str,
) -> Result<Vec<TrackDto>, api::ApiError> {
    match source {
        // 「我喜欢的」没有自己的 id —— 它是账号的属性,不是一个歌单实体
        Source::Liked => {
            api::liked().await.map(|dto| dto.tracks)
        }
        Source::Local => api::playlist_tracks(id)
            .await
            .map(|dto| dto.tracks),
        Source::Platform => {
            api::platform_playlist_tracks(id)
                .await
                .map(|dto| dto.tracks)
        }
    }
}

/// 把契约里的歌单翻成界面要的一行。
///
/// 封面留空:它要一趟网络,而这个函数是同步的。图到了之后由
/// `crate::artwork::apply` 回填(见那个模块)。
pub fn to_row(list: &PlaylistDto) -> PlaylistRow {
    PlaylistRow {
        id: list.id.clone().into(),
        name: list.name.clone().into(),
        subtitle: track_count_text(list.track_count).into(),
        source: Source::from_dto(list.source).to_index(),
        cover: slint::Image::default(),
    }
}

/// 给这一批歌单挨个把封面取上。
///
/// 已经在内存或磁盘里的那些在这一帧就摆上,剩下的各发一次请求。
pub fn fetch_covers(
    ui: &MainWindow,
    art: &crate::artwork::Artwork,
    lists: &[PlaylistDto],
) {
    for list in lists {
        let Some(url) = list.cover.as_deref() else {
            continue;
        };
        crate::artwork::ensure(ui, art, &list.id, url);
    }
    crate::artwork::apply(ui, art);
}

/// 正在编辑什么。
///
/// 两样东西:当前打开的是哪个歌单,以及**打开之前**列表里摆的那一批歌。
/// 后者是「把刚才那批加进来」的全部来源 —— 进歌单那一刻 `tracks` 就被换成
/// 这个歌单自己的歌了,不先存一份就再也找不回来。
#[derive(Clone, Default)]
pub struct Editing {
    open: Rc<RefCell<Option<(Source, String)>>>,
    stash: Rc<RefCell<Vec<TrackDto>>>,
}

impl Editing {
    /// 记下打开了哪个歌单,并把打开之前那一批歌存起来。
    pub fn opened(
        &self,
        source: Source,
        id: &str,
        previous: Vec<TrackDto>,
    ) {
        *self.open.borrow_mut() =
            Some((source, id.to_owned()));
        *self.stash.borrow_mut() = previous;
    }

    /// 退出详情。存下的那一批一起丢掉 —— 它只在详情里有意义。
    pub fn closed(&self) {
        *self.open.borrow_mut() = None;
        self.stash.borrow_mut().clear();
    }

    /// 当前打开的那个歌单。
    pub fn current(&self) -> Option<(Source, String)> {
        self.open.borrow().clone()
    }

    /// 当前打开的**本地**歌单。写操作一律先过这里 ——
    /// 平台歌单与红心改不动,拿不到 id 就发不出请求。
    pub fn current_local(&self) -> Option<String> {
        match self.current() {
            Some((source, id)) if is_editable(source) => {
                Some(id)
            }
            _ => None,
        }
    }

    /// 存下的那一批。
    pub fn stashed(&self) -> Vec<TrackDto> {
        self.stash.borrow().clone()
    }

    /// 丢掉存下的那一批。收进歌单之后调用 —— 那一行的活干完了。
    pub fn clear_stash(&self) {
        self.stash.borrow_mut().clear();
    }
}

/// 写请求里的平台名。
///
/// 曲目的身份是 (平台, 平台内 id),而界面那一行只带 id —— 补上这一半。
//
// ponytail: 单平台写死。接第二个平台时 TrackRow 要多一个 platform 字段,
// 这个常量随之作废;在那之前多一个字段只是每行多存一份同样的字符串。
const ONLY_PLATFORM: &str = "netease";

/// 曲目在写请求里的形态:(平台, 平台内 id)。
///
/// 身份是这两个合起来,缺一不可 —— 只传 id 的话,接第二个平台时两边的 id
/// 会静默撞车(见 bang-dream 的 `docs/adr/0003`)。
fn refs_of(tracks: &[TrackDto]) -> Vec<(String, String)> {
    tracks
        .iter()
        .map(|track| {
            (track.platform.clone(), track.id.clone())
        })
        .collect()
}

/// 接上本地歌单的写操作。
///
/// `reload` 由 `music` 传进来:改完之后要把当前歌单的曲目重取一遍,而那要用到
/// 播放队列,队列归那边。
pub fn bind_edit<R>(
    ui: &MainWindow,
    editing: &Editing,
    art: &crate::artwork::Artwork,
    reload: R,
) where
    R: Fn(&MainWindow) + Clone + 'static,
{
    bind_create(ui, art);
    bind_rename(ui, editing, art);
    bind_delete(ui, editing, art);
    bind_add_batch(ui, editing, reload.clone());
    bind_remove(ui, editing, reload);
}

/// 新建歌单。名字由回调带出来 —— 输入框在 `if` 里,Rust 引用不到它。
fn bind_create(
    ui: &MainWindow,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let weak = ui.as_weak();

    ui.on_create_playlist(move |name| {
        let name = name.trim().to_owned();
        let Some(ui) = weak.upgrade() else { return };
        // 空名字服务端也会拒,但那要等一趟往返才说 ——
        // 而「没打字就按了新建」这件事这边就看得见。
        if name.is_empty() {
            crate::notice::show(
                &ui,
                "歌单要有名字".to_owned(),
            );
            return;
        }

        let art = art.clone();
        let weak = ui.as_weak();
        let _ = slint::spawn_local(async move {
            let done = api::create_playlist(&name).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(_) => refresh(&ui, &art),
                Err(err) => report(&ui, &err, "建歌单失败"),
            }
        });
    });
}

/// 改名。
fn bind_rename(
    ui: &MainWindow,
    editing: &Editing,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.on_rename_playlist(move |name| {
        let name = name.trim().to_owned();
        let Some(ui) = weak.upgrade() else { return };
        let Some(id) = editing.current_local() else {
            return;
        };
        if name.is_empty() {
            crate::notice::show(
                &ui,
                "歌单要有名字".to_owned(),
            );
            return;
        }

        let art = art.clone();
        let weak = ui.as_weak();
        let _ = slint::spawn_local(async move {
            let done =
                api::rename_playlist(&id, &name).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => {
                    // 标题就地改掉,不等列表刷新 —— 详情页正显示着它
                    ui.set_open_playlist_name(
                        name.as_str().into(),
                    );
                    refresh(&ui, &art);
                }
                Err(err) => report(&ui, &err, "改名失败"),
            }
        });
    });
}

/// 删除。二次确认由界面那一层管(见 app.slint),到这里已经是确定要删了。
fn bind_delete(
    ui: &MainWindow,
    editing: &Editing,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.on_delete_playlist(move || {
        let Some(id) = editing.current_local() else {
            return;
        };
        let editing = editing.clone();
        let art = art.clone();
        let weak = weak.clone();

        let _ = slint::spawn_local(async move {
            let done = api::delete_playlist(&id).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => {
                    // 删掉的歌单不能再停在它的详情里
                    editing.closed();
                    ui.set_open_playlist_name(
                        slint::SharedString::new(),
                    );
                    ui.set_open_playlist_local(false);
                    ui.set_add_batch_text(
                        slint::SharedString::new(),
                    );
                    refresh(&ui, &art);
                }
                Err(err) => report(&ui, &err, "删歌单失败"),
            }
        });
    });
}

/// 把打开之前那一批歌收进当前歌单。
fn bind_add_batch<R>(
    ui: &MainWindow,
    editing: &Editing,
    reload: R,
) where
    R: Fn(&MainWindow) + Clone + 'static,
{
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.on_add_batch(move || {
        let Some(id) = editing.current_local() else {
            return;
        };
        let refs = refs_of(&editing.stashed());
        if refs.is_empty() {
            return;
        }

        let weak = weak.clone();
        let editing = editing.clone();
        let reload = reload.clone();
        let _ = slint::spawn_local(async move {
            let done =
                api::add_playlist_tracks(&id, &refs).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => {
                    // 收完就没有「刚才那批」了:那一行的活干完了。
                    // 留着的话再点一次是把同一批又加一遍(服务端幂等,
                    // 但界面上看着像什么都没发生)。
                    editing.clear_stash();
                    ui.set_add_batch_text(
                        slint::SharedString::new(),
                    );
                    reload(&ui);
                }
                Err(err) => report(&ui, &err, "加歌失败"),
            }
        });
    });
}

/// 把某一首移出当前歌单。
fn bind_remove<R>(
    ui: &MainWindow,
    editing: &Editing,
    reload: R,
) where
    R: Fn(&MainWindow) + Clone + 'static,
{
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.on_remove_track(move |track_id| {
        let Some(id) = editing.current_local() else {
            return;
        };
        let refs = vec![(
            ONLY_PLATFORM.to_owned(),
            track_id.to_string(),
        )];

        let weak = weak.clone();
        let reload = reload.clone();
        let _ = slint::spawn_local(async move {
            let done =
                api::remove_playlist_tracks(&id, &refs)
                    .await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => reload(&ui),
                Err(err) => report(&ui, &err, "移出失败"),
            }
        });
    });
}

/// 拉一次歌单列表,填进界面。
pub fn refresh(
    ui: &MainWindow,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let weak = ui.as_weak();

    let _ = slint::spawn_local(async move {
        let found = api::playlists().await;
        let Some(ui) = weak.upgrade() else { return };

        match found {
            Ok(dto) => {
                let rows: Vec<PlaylistRow> =
                    dto.playlists.iter().map(to_row).collect();
                ui.set_playlists(
                    slint::ModelRc::new(
                        slint::VecModel::from(rows),
                    ),
                );
                // 行先摆上,封面随后回填 —— 等图到齐再摆的话,
                // 网络慢时整张列表都是空的。
                fetch_covers(&ui, &art, &dto.playlists);
            }
            Err(err)
                if crate::account::handle_session_expiry(
                    &ui, &err,
                ) => {}
            Err(err) => report(&ui, &err, "取歌单失败"),
        }
    });
}

/// 报一次失败。走横幅,不走播放状态行(见 `crate::notice`)。
fn report(
    ui: &MainWindow,
    err: &api::ApiError,
    what: &str,
) {
    if crate::account::handle_session_expiry(ui, err) {
        return;
    }
    crate::notice::show(ui, format!("{what}: {err}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三种来源的编号来回转都对得上。
    ///
    /// 转错的现象是点开一个歌单看到另一个歌单的歌 —— 而两边都不报错。
    #[test]
    fn each_source_has_its_own_way_in() {
        for source in
            [Source::Liked, Source::Platform, Source::Local]
        {
            assert_eq!(
                Source::from_index(source.to_index()),
                source,
                "{source:?} 的编号转不回来"
            );
        }

        // 三个编号互不相同,否则上面那条也会过
        assert_eq!(Source::Liked.to_index(), 0);
        assert_eq!(Source::Platform.to_index(), 1);
        assert_eq!(Source::Local.to_index(), 2);

        // 认不出的编号落到平台歌单:三者里唯一只读的那个
        assert_eq!(
            Source::from_index(99),
            Source::Platform
        );
    }

    /// 「把刚才那批加进来」的文案带上条数;没有可加的就是空串,那一行不出现。
    ///
    /// 进歌单那一刻列表已经换掉了 —— 不说清是哪一批,用户点下去才知道加了什么。
    #[test]
    fn add_batch_label_says_how_many() {
        assert_eq!(
            add_batch_text(30),
            "+ 把刚才那 30 首加进来"
        );
        assert_eq!(
            add_batch_text(1),
            "+ 把刚才那 1 首加进来"
        );
        assert_eq!(
            add_batch_text(0),
            "",
            "没有可加的就不该有这一行"
        );
    }

    /// 能改的只有本地歌单。
    ///
    /// 判据是来源不是名字:用户完全可以把一个本地歌单起名叫「我喜欢的」,
    /// 而按名字判的话,那个歌单会突然变得不能改。
    #[test]
    fn only_local_playlists_are_editable() {
        assert!(is_editable(Source::Local));
        assert!(!is_editable(Source::Liked));
        assert!(!is_editable(Source::Platform));
    }

    /// 副标题说的是有多少首歌;空歌单说「暂无曲目」而不是「0 首」——
    /// 后者读起来像个统计数字,而这里要说的是"点进去也没东西"。
    #[test]
    fn the_subtitle_says_how_many_tracks() {
        assert_eq!(track_count_text(120), "120 首");
        assert_eq!(track_count_text(1), "1 首");
        assert_eq!(track_count_text(0), "暂无曲目");
        // 上游给了个负数也不该露出来
        assert_eq!(track_count_text(-1), "暂无曲目");
    }

    /// 写出去的条数读得回来 —— 点红心之后要就地把这个数字加一,
    /// 而界面上只剩这句话,没有别处存着那个数。
    #[test]
    fn a_track_count_survives_a_round_trip_through_its_text()
     {
        for count in [1, 7, 120, 976] {
            assert_eq!(
                track_count_of(&track_count_text(count)),
                Some(count),
                "{count} 首该读得回来"
            );
        }
    }

    /// 边界:读不出数字时返回 `None`,调用方据此不动它。
    ///
    /// 「暂无曲目」是其中一种 —— 它对应 0,但也对应「上游给了个负数」,
    /// 两者不该被读成同一个可加减的起点。
    #[test]
    fn an_unreadable_subtitle_yields_no_count() {
        // 「暂无曲目」既对应 0,也对应上游给的负数 —— 不是可加减的起点
        assert_eq!(
            track_count_of(&track_count_text(0)),
            None
        );
        assert_eq!(
            track_count_of(&track_count_text(-1)),
            None
        );
        // 别处写来的文案不能被误读成条数
        assert_eq!(track_count_of("12 张专辑"), None);
        assert_eq!(track_count_of(""), None);
        assert_eq!(track_count_of("首"), None);
        assert_eq!(track_count_of("很多 首"), None);
    }

    /// 「另有 N 首平台不再提供」要说出具体几首。
    ///
    /// 服务端把拿不到详情的曲目剔出成员关系(见 server 的 `cached_tracks`),
    /// 不说一声的话用户看到的只是数目对不上,而分不清「我少点了一个红心」
    /// 和「平台不给这首歌的详情」。
    #[test]
    #[ignore = "骨架待评审"]
    fn the_note_says_how_many_are_unavailable() {}

    /// 边界:一首都没少时返回空串,那一行整个不出现 ——
    /// 与 `add_batch_text` 同一条规矩。常态就是这一条,一个恒显示的
    /// 「另有 0 首」只会变成噪声。
    #[test]
    #[ignore = "骨架待评审"]
    fn no_note_when_nothing_is_unavailable() {}

    /// 契约里的来源原样翻成界面编号,不在中间丢掉。
    #[test]
    fn the_contract_source_survives_the_trip() {
        let row = to_row(&PlaylistDto {
            source: PlaylistSource::Local,
            id: "3".to_owned(),
            name: "睡前".to_owned(),
            cover: None,
            track_count: 12,
        });

        assert_eq!(row.id, "3");
        assert_eq!(row.name, "睡前");
        assert_eq!(row.subtitle, "12 首");
        assert_eq!(row.source, Source::Local.to_index());
    }
}
