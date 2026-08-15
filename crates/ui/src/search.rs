//! 搜索的三个页签:歌曲 / 歌手 / 歌单。
//!
//! 一次搜索打三条独立的路由(`/search/tracks|artists|playlists`),不是一条带
//! 类型参数的 —— 三者的响应形状本来就不同,合成一条只会让每个调用点先去问一遍
//! 「这次回的是哪一种」。
//!
//! 结果各存各的:歌手与歌单有自己的属性,**不与「我的歌单」共用** —— 共用的话,
//! 切回我的歌单会看见上一次的搜索结果。曲目仍然填进 `tracks`,那个属性本来就是
//! 「当前这批歌」。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::ArtistDto;
use slint::ComponentHandle;

use crate::Library;
use crate::Player;
use crate::{ArtistRow, MainWindow, PlaylistRow};

/// 搜索结果的三类。数值即 `.slint` 里 `SearchTabs.items` 的下标 ——
/// 两处手工对齐,错位的现象是点「歌手」出歌单,而两边各自都是对的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Tracks,
    Artists,
    Playlists,
}

impl Tab {
    /// 由界面给的编号认回来。认不出的当歌曲 —— 那是默认那一页。
    ///
    /// 只有这一个方向:编号由界面给,Rust 侧从不反过来算它。
    /// (歌单的 `Source` 两个方向都要,因为每一行的来源是 Rust 填的。)
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Artists,
            2 => Self::Playlists,
            _ => Self::Tracks,
        }
    }
}

/// 一位歌手的副标题:他有多少张专辑。
///
/// 与歌单的「暂无曲目」同一个理由:0 张时说「暂无专辑」而不是「0 张专辑」——
/// 后者读起来像个统计数字,而这里要说的是"没什么可看的"。
pub fn artist_subtitle(album_count: i32) -> String {
    if album_count <= 0 {
        "暂无专辑".to_owned()
    } else {
        format!("{album_count} 张专辑")
    }
}

/// 把契约里的歌手翻成界面要的一行。
pub fn to_artist_row(artist: &ArtistDto) -> ArtistRow {
    ArtistRow {
        id: artist.id.clone().into(),
        name: artist.name.clone().into(),
        subtitle: artist_subtitle(artist.album_count)
            .into(),
    }
}

/// 接上搜索框与三个页签。
///
/// `tracks` 是搜歌那一路 —— 它要往播放队列里塞东西,而队列归 `music`,
/// 所以由那边传进来。
///
/// 关键词记在这里:换页签要用上一次的关键词重搜,而输入框在一个 `if` 里,
/// Slint 不让外面引用它(见 crates/ui/README.md)。记一份是唯一的路。
pub fn bind(
    ui: &MainWindow,
    tracks: impl Fn(&MainWindow, &str) + Clone + 'static,
) {
    let last: Rc<RefCell<String>> = Rc::default();

    let weak = ui.as_weak();
    let keep = last.clone();
    let search_tracks = tracks.clone();
    ui.global::<Player>().on_search(move |keyword| {
        let keyword = keyword.trim().to_owned();
        if keyword.is_empty() {
            return;
        }
        let Some(ui) = weak.upgrade() else { return };

        *keep.borrow_mut() = keyword.clone();
        run(
            &ui,
            &keyword,
            current_tab(&ui),
            &search_tracks,
        );
    });

    let weak = ui.as_weak();
    ui.global::<Library>().on_select_search_tab(
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            ui.global::<Library>().set_search_tab(index);

            // 没搜过就只是换个空页签,不打一次空搜索
            let keyword = last.borrow().clone();
            if keyword.is_empty() {
                return;
            }
            run(
                &ui,
                &keyword,
                Tab::from_index(index),
                &tracks,
            );
        },
    );
}

/// 界面当前停在哪个页签。
fn current_tab(ui: &MainWindow) -> Tab {
    Tab::from_index(ui.global::<Library>().get_search_tab())
}

/// 按页签搜一次。
fn run(
    ui: &MainWindow,
    keyword: &str,
    tab: Tab,
    tracks: &impl Fn(&MainWindow, &str),
) {
    match tab {
        Tab::Tracks => tracks(ui, keyword),
        Tab::Artists => search_artists(ui, keyword),
        Tab::Playlists => search_playlists(ui, keyword),
    }
}

/// 搜歌手,填进歌手列表。
fn search_artists(ui: &MainWindow, keyword: &str) {
    let keyword = keyword.to_owned();
    let weak = ui.as_weak();

    let _ = slint::spawn_local(async move {
        let found = api::search_artists(&keyword).await;
        let Some(ui) = weak.upgrade() else { return };

        match found {
            Ok(dto) => {
                let rows: Vec<ArtistRow> = dto
                    .artists
                    .iter()
                    .map(to_artist_row)
                    .collect();
                ui.global::<Library>().set_found_artists(
                    slint::ModelRc::new(
                        slint::VecModel::from(rows),
                    ),
                );
            }
            Err(err) => report(&ui, &err, "搜歌手失败"),
        }
    });
}

/// 搜歌单,填进搜索的歌单列表。
///
/// 用的是与「我的歌单」同一个行类型与同一个组件 —— 点开走的也是同一条路,
/// 因为搜到的歌单和收藏来的歌单是同一种东西。
fn search_playlists(ui: &MainWindow, keyword: &str) {
    let keyword = keyword.to_owned();
    let weak = ui.as_weak();

    let _ = slint::spawn_local(async move {
        let found = api::search_playlists(&keyword).await;
        let Some(ui) = weak.upgrade() else { return };

        match found {
            Ok(dto) => {
                let rows: Vec<PlaylistRow> = dto
                    .playlists
                    .iter()
                    .map(crate::playlist::to_row)
                    .collect();
                ui.global::<Library>().set_found_playlists(
                    slint::ModelRc::new(
                        slint::VecModel::from(rows),
                    ),
                );
            }
            Err(err) => report(&ui, &err, "搜歌单失败"),
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

    /// 编号对上 `SearchTabs.items` 的顺序。
    ///
    /// 断言的是**具体数值**而不是来回转一圈:自洽的转换在两边一起写反时
    /// 照样通过,而这里要钉住的正是 `.slint` 那张表的顺序。
    /// 错位的现象是点「歌手」出歌单,两边各自看都没错。
    #[test]
    fn tab_index_maps_to_the_right_kind() {
        assert_eq!(Tab::from_index(0), Tab::Tracks);
        assert_eq!(Tab::from_index(1), Tab::Artists);
        assert_eq!(Tab::from_index(2), Tab::Playlists);
    }

    /// 认不出的编号退回歌曲页签 —— 那是默认那一页。
    #[test]
    fn an_unknown_tab_falls_back_to_tracks() {
        assert_eq!(Tab::from_index(9), Tab::Tracks);
        assert_eq!(Tab::from_index(-1), Tab::Tracks);
    }

    /// 副标题读起来像句话,不像个统计数字。
    #[test]
    fn artist_subtitle_reads_naturally() {
        assert_eq!(artist_subtitle(12), "12 张专辑");
        assert_eq!(artist_subtitle(0), "暂无专辑");
        // 平台偶尔给负数(字段缺失时的默认值),按没有算
        assert_eq!(artist_subtitle(-1), "暂无专辑");
    }
}
