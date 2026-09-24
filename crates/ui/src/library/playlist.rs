//! 歌单列表与详情的绑定。
//!
//! 三种来源的歌单在界面上是同一张列表(见 `docs/adr/0016`),但**取曲目的路
//! 不同**:「我喜欢的」是平台的红心列表,本地歌单在自家库里,平台歌单要问上游。
//! 走错的现象是点开一个歌单看到另一个歌单的歌 —— 而两边都不报错。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{PlaylistDto, TrackDto};
use slint::ComponentHandle;

use crate::Library;
use crate::{MainWindow, PlaylistRow};

mod edit;
mod rules;

pub(crate) use edit::bind_edit;
pub(crate) use rules::*;

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

/// 给这一批歌单挨个把封面取上。
///
/// 已经在内存里的那些在这一帧就摆上,剩下的去后台问磁盘、再不行上网。
pub fn fetch_covers(
    ui: &MainWindow,
    art: &crate::imagery::artwork::Artwork,
    lists: &[PlaylistDto],
) {
    for list in lists {
        let Some(url) = list.cover.as_deref() else {
            continue;
        };
        crate::imagery::artwork::ensure(
            ui, art, &list.id, url,
        );
    }
    crate::imagery::artwork::apply(ui, art);
}

/// 拉一次歌单列表,填进界面。
///
/// 先摆本地缓存里上次那份,新的回来且不同再换(#123)。
pub fn refresh(
    ui: &MainWindow,
    art: &crate::imagery::artwork::Artwork,
) {
    let art = art.clone();
    let weak = ui.as_weak();

    let _ = slint::spawn_local(async move {
        let stale = api::cached_playlists().await;
        if let Some(stale) = &stale
            && let Some(ui) = weak.upgrade()
        {
            fill(&ui, &art, &stale.playlists);
        }

        let found = api::playlists().await;
        let Some(ui) = weak.upgrade() else { return };

        match found {
            Ok(dto) if stale.as_ref() == Some(&dto) => {}
            Ok(dto) => fill(&ui, &art, &dto.playlists),
            Err(err)
                if crate::pages::account::handle_session_expiry(
                    &ui, &err,
                ) => {}
            Err(err) => report(&ui, &err, "取歌单失败"),
        }
    });
}

/// 把一份歌单列表摆进界面。
fn fill(
    ui: &MainWindow,
    art: &crate::imagery::artwork::Artwork,
    lists: &[PlaylistDto],
) {
    let rows: Vec<PlaylistRow> =
        lists.iter().map(to_row).collect();
    let model = ui.global::<Library>().get_playlists();
    match model
        .as_any()
        .downcast_ref::<slint::VecModel<PlaylistRow>>()
    {
        Some(shown) => update_in_place(shown, rows),
        None => ui.global::<Library>().set_playlists(
            slint::ModelRc::new(slint::VecModel::from(rows)),
        ),
    }
    // 行先摆上,封面随后回填 —— 等图到齐再摆的话,
    // 网络慢时整张列表都是空的。
    fetch_covers(ui, art, lists);
}

/// 原地改成 `rows` 那一份:只动有变化的行,多了截掉、少了补上(#137 ⑥)。
///
/// 整表换模型会让行元素整批重建,正落在旧行上的那一下点击就被吞了 —— ④ 真机上
/// 登录刚回来时点「我喜欢的」就这样丢过一次。同一个歌单已经取到的封面留着:
/// 新行里的封面是空的,拿它去比就每行都「变了」。
fn update_in_place(
    shown: &slint::VecModel<PlaylistRow>,
    rows: Vec<PlaylistRow>,
) {
    use slint::Model as _;

    for (index, mut row) in rows.iter().cloned().enumerate() {
        let Some(old) = shown.row_data(index) else {
            shown.push(row);
            continue;
        };
        if old.id == row.id && old.source == row.source {
            row.cover = old.cover.clone();
        }
        if old != row {
            shown.set_row_data(index, row);
        }
    }
    while shown.row_count() > rows.len() {
        shown.remove(shown.row_count() - 1);
    }
}

/// 报一次失败。走横幅,不走播放状态行(见 `crate::notice`)。
fn report(
    ui: &MainWindow,
    err: &api::ApiError,
    what: &str,
) {
    if crate::pages::account::handle_session_expiry(ui, err)
    {
        return;
    }
    crate::pages::account::report_failure(ui, what, err);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 歌单页撞到网易云没绑,得说「去个人页扫码」,不是上游原话。
    #[test]
    fn netease_unbound_playlist_failure_points_at_profile()
    {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");

        report(
            &ui,
            &api::ApiError::Server {
                code: "netease_not_logged_in".to_owned(),
                message: "netease: 未登录".to_owned(),
            },
            "取歌单失败",
        );

        assert_eq!(
            ui.global::<crate::Shell>().get_banner_text(),
            crate::pages::account::NETEASE_UNBOUND_TEXT
        );
        assert_eq!(
            ui.global::<crate::Shell>().get_banner_tab(),
            2,
            "这一句该能点进个人页"
        );
    }

    fn list(id: &str, name: &str) -> PlaylistDto {
        PlaylistDto {
            source: app_core::PlaylistSource::Platform,
            id: id.to_owned(),
            name: name.to_owned(),
            cover: None,
            track_count: 3,
        }
    }

    /// `/playlists` 回来一份新的:原地改有变化的那几行,不整表换模型(#137 ⑥)。
    ///
    /// 换模型会让行元素整批重建,正落在旧行上的那一下点击就被吞了 —— ④ 真机上
    /// 登录刚回来时点「我喜欢的」就这样丢过一次。
    #[test]
    fn a_fresh_playlist_list_updates_rows_in_place() {
        use slint::Model as _;

        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        let art = crate::imagery::artwork::Artwork::default();

        fill(&ui, &art, &[list("1", "甲"), list("2", "乙")]);
        let model = ui.global::<Library>().get_playlists();

        fill(&ui, &art, &[list("1", "甲"), list("2", "乙改"), list("3", "丙")]);

        let now = ui.global::<Library>().get_playlists();
        assert!(now == model, "歌单列表整张换了模型");
        let names: Vec<String> =
            now.iter().map(|row| row.name.to_string()).collect();
        assert_eq!(names, ["甲", "乙改", "丙"]);

        fill(&ui, &art, &[list("3", "丙")]);
        let names: Vec<String> = ui
            .global::<Library>()
            .get_playlists()
            .iter()
            .map(|row| row.name.to_string())
            .collect();
        assert_eq!(names, ["丙"], "变短了就截掉多出来的行");
    }
}
