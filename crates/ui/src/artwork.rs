//! 歌单封面:取一次、记住、下次直接给。
//!
//! 两级:进程内一张表,以及磁盘上一个目录(见 `docs/adr/0015` —— 客户端不建库,
//! 封面落磁盘)。**按歌单标识存,不按 URL 存**:平台的 CDN 会换 URL,拿它当键
//! 等于每换一次全部作废。
//!
//! 取不到就没有图 —— 封面是装饰,拿不到它不该挡住任何事(与 `crate::cover`
//! 同一条规矩:CDN 过期后回的是 HTML 错误页,不是图)。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::Library;
use crate::MainWindow;

/// 进程内的封面表:歌单标识 → 已解码的图。
///
/// 只增不删。三十几个歌单的缩略图在内存里不值得一套淘汰策略 —— 真到了要给
/// 每一行曲目配图那天(973 行),那时才需要可见区加载与 LRU,而那是另一件事。
#[derive(Clone, Default)]
pub struct Artwork {
    memory: Rc<RefCell<HashMap<String, slint::Image>>>,
    /// 已经在取的那些,免得同一个歌单被连点几次就发几次请求。
    inflight: Rc<RefCell<HashMap<String, ()>>>,
}

/// 歌单标识变成一个安全的文件名。
///
/// 这个标识来自平台,而它会成为路径的一段 —— 不过滤的话,一个
/// `../../` 就能让缓存写到目录外面去。只留 ASCII 字母数字与 `-_`,
/// 别的一律换成 `_`;空标识返回 `None`,那种东西不该有缓存文件。
pub fn cache_name(playlist_id: &str) -> Option<String> {
    if playlist_id.is_empty() {
        return None;
    }

    Some(
        playlist_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric()
                    || c == '-'
                    || c == '_'
                {
                    c
                } else {
                    '_'
                }
            })
            .collect(),
    )
}

impl Artwork {
    /// 已经有的那一张。
    pub fn get(
        &self,
        playlist_id: &str,
    ) -> Option<slint::Image> {
        self.memory.borrow().get(playlist_id).cloned()
    }

    /// 记住一张。
    fn put(&self, playlist_id: &str, image: slint::Image) {
        self.memory
            .borrow_mut()
            .insert(playlist_id.to_owned(), image);
    }

    /// 这个歌单的封面是不是已经在路上了。
    fn claim(&self, playlist_id: &str) -> bool {
        self.inflight
            .borrow_mut()
            .insert(playlist_id.to_owned(), ())
            .is_none()
    }

    fn release(&self, playlist_id: &str) {
        self.inflight.borrow_mut().remove(playlist_id);
    }
}

/// 确保某个歌单的封面在手上,拿到之后把列表里对应那一行填上。
///
/// 三步依次问:内存里有吗 → 磁盘上有吗 → 向 CDN 要。前两步都是同步的,
/// 所以已经缓存过的封面在这一帧就摆上了,不会先闪一下空白。
pub fn ensure(
    ui: &MainWindow,
    art: &Artwork,
    playlist_id: &str,
    url: &str,
) {
    if url.is_empty() || art.get(playlist_id).is_some() {
        return;
    }

    // 磁盘上那一份。命中就地解码 —— 一张缩略图,几毫秒的事。
    if let Some(name) = cache_name(playlist_id)
        && let Some(bytes) = api::load_artwork(&name)
        && let Some((image, _)) =
            crate::cover::decode(&bytes)
    {
        art.put(playlist_id, image);
        apply(ui, art);
        return;
    }

    if !art.claim(playlist_id) {
        return;
    }

    let art = art.clone();
    let weak = ui.as_weak();
    let playlist_id = playlist_id.to_owned();
    let url = url.to_owned();

    let _ = slint::spawn_local(async move {
        let fetched = api::fetch_bytes(&url).await;
        art.release(&playlist_id);

        let Ok(bytes) = fetched else {
            // 封面取不到是常态,不是故障:CDN 会过期,也会挡住不常见的 UA
            log::debug!("取封面失败: {url}");
            return;
        };
        let Some((image, _)) = crate::cover::decode(&bytes)
        else {
            log::debug!("封面不是图: {url}");
            return;
        };

        if let Some(name) = cache_name(&playlist_id) {
            api::save_artwork(&name, &bytes);
        }
        art.put(&playlist_id, image);

        if let Some(ui) = weak.upgrade() {
            apply(&ui, &art);
        }
    });
}

/// 把手上有的封面填进列表里对应的行。
///
/// 每次有新图就整表扫一遍,而不是记住"这张图是第几行"—— 行会因为刷新、
/// 搜索、改名而换位置,记下的下标随时会指向另一个歌单。
pub fn apply(ui: &MainWindow, art: &Artwork) {
    use slint::Model as _;

    for rows in [
        ui.global::<Library>().get_playlists(),
        ui.global::<Library>().get_found_playlists(),
    ] {
        for i in 0..rows.row_count() {
            let Some(mut row) = rows.row_data(i) else {
                continue;
            };
            let Some(image) = art.get(row.id.as_str())
            else {
                continue;
            };
            if row.cover.size().width == 0 {
                row.cover = image;
                rows.set_row_data(i, row);
            }
        }
    }

    // 详情页那张大的:当前打开的歌单如果有图,一并推上去
    if let Some(image) = art.get(
        ui.global::<Library>()
            .get_open_playlist_id()
            .as_str(),
    ) {
        ui.global::<Library>()
            .set_open_playlist_cover(image);
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 歌单标识变成文件名时,路径分隔符与相对路径一律被吃掉。
    ///
    /// 这个标识来自平台,而它会成为路径的一段 —— 不过滤的话,一个 `../../`
    /// 就能让缓存写到目录外面去。与 bang-dream 的凭据分片同一个理由。
    #[test]
    fn a_cache_name_cannot_escape_its_directory() {
        assert_eq!(
            cache_name("../../etc/passwd").as_deref(),
            Some("______etc_passwd")
        );
        assert_eq!(
            cache_name("a/b").as_deref(),
            Some("a_b")
        );
        assert!(
            !cache_name("../x").unwrap().contains('/'),
            "结果里不该还剩路径分隔符"
        );
    }

    /// 正常的标识原样通过 —— 平台歌单是数字,本地歌单是整数主键。
    #[test]
    fn an_ordinary_id_is_left_alone() {
        assert_eq!(
            cache_name("24381616").as_deref(),
            Some("24381616")
        );
        assert_eq!(
            cache_name("163").as_deref(),
            Some("163")
        );
    }

    /// 空标识没有缓存文件。
    ///
    /// 「我喜欢的」就是空标识(它是账号的属性,不是一个歌单实体),
    /// 不特判的话它会和别的空串挤在同一个文件上。
    #[test]
    fn an_empty_id_has_no_cache_file() {
        assert_eq!(cache_name(""), None);
    }

    // ── 取一次的那道闸([`ensure`] 与 [`apply`])──
    //
    // 三步依次问:内存 → 磁盘 → CDN。前两步答得上就不该有第三步,而多发的
    // 那几次请求在界面上一点痕迹都没有 —— 只有那张表说得出到底问到了哪一步。

    use slint::{Model as _, ModelRc, VecModel};

    use crate::{PlaylistRow, Session, Shell};

    /// 一张 1×1 的图。有没有图才是被测的东西,画的什么无关紧要。
    fn image() -> slint::Image {
        slint::Image::from_rgba8(
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                1, 1,
            ),
        )
    }

    /// 一行歌单,封面槽空着。
    fn row(id: &str) -> PlaylistRow {
        PlaylistRow {
            id: id.into(),
            name: "歌单".into(),
            subtitle: "1 首".into(),
            source: 0,
            cover: slint::Image::default(),
        }
    }

    /// 无头音乐页,「我的歌单」里摆着这几行。
    fn window_with(rows: Vec<PlaylistRow>) -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.global::<Session>().set_logged_in(true);
        ui.global::<Shell>().set_current_tab(1);
        ui.global::<Library>().set_playlists(ModelRc::new(
            VecModel::from(rows),
        ));
        ui
    }

    /// 平台没给封面时不占「正在取」的名额。
    ///
    /// 占了的话,以后这个歌单**真的**有封面了也再取不到 —— 那张表只增不减,
    /// 而空 URL 根本没有请求会回来把它释放掉。
    #[test]
    fn a_playlist_without_a_cover_is_never_claimed() {
        let ui = window_with(vec![row("163")]);
        let art = Artwork::default();

        ensure(&ui, &art, "163", "");

        assert!(
            art.claim("163"),
            "空 URL 什么都没发出去,不该把这个歌单占住"
        );
    }

    /// 内存里已经有的那张不再取一遍。
    ///
    /// 列表每次刷新都会把每一行都问一遍(见 `playlist::fetch_covers`),
    /// 不认这一步就是每刷新一次把三十几张封面全下一遍。
    #[test]
    fn a_cover_already_in_memory_is_not_fetched_again() {
        let ui = window_with(vec![row("163")]);
        let art = Artwork::default();
        art.put("163", image());

        ensure(&ui, &art, "163", "https://cdn/163.jpg");

        assert!(
            art.claim("163"),
            "手上已经有图了,不该再发一次请求"
        );
    }

    /// 同一个歌单连点几次只发一次请求。
    ///
    /// 点开歌单、返回、再点开,每一下都会走到这里 —— 没有这道闸就是几条
    /// 并发的下载抢同一张图,而它们最后写进的是同一格。
    #[test]
    fn a_cover_already_on_its_way_is_not_asked_for_twice() {
        let ui = window_with(vec![row("163")]);
        let art = Artwork::default();

        ensure(&ui, &art, "163", "https://cdn/163.jpg");
        ensure(&ui, &art, "163", "https://cdn/163.jpg");

        assert!(
            !art.claim("163"),
            "第一次就该把这个歌单占住,后面几次才有东西可挡"
        );
        assert!(
            art.get("163").is_none(),
            "图还在路上,这时不该有任何东西被摆进内存表"
        );
    }

    /// 手上的封面按标识回填到行里,而不是按下标。
    ///
    /// 行会因为刷新、搜索、改名而换位置 —— 记下的下标随时会指向另一个歌单,
    /// 而那时列表里显示的就是别人的封面,两边都不报错。
    #[test]
    fn covers_are_filled_in_by_id_not_by_position() {
        let ui =
            window_with(vec![row("163"), row("24381616")]);
        let art = Artwork::default();
        art.put("24381616", image());

        apply(&ui, &art);

        let rows = ui.global::<Library>().get_playlists();
        assert_eq!(
            rows.row_data(0)
                .expect("第一行该在")
                .cover
                .size()
                .width,
            0,
            "手上没有这个歌单的图,第一行不该被填上别人的"
        );
        assert_eq!(
            rows.row_data(1)
                .expect("第二行该在")
                .cover
                .size()
                .width,
            1
        );
    }

    /// 当前打开的那个歌单,详情页那张大图也一并推上去。
    ///
    /// 详情页按标识索引而不是名字 —— 两个歌单可以同名,而封面挂错了
    /// 只有人眼看得出来。
    #[test]
    fn the_open_playlist_gets_its_own_big_cover() {
        let ui = window_with(vec![row("163")]);
        let art = Artwork::default();
        art.put("163", image());
        ui.global::<Library>()
            .set_open_playlist_id("163".into());

        apply(&ui, &art);

        assert_eq!(
            ui.global::<Library>()
                .get_open_playlist_cover()
                .size()
                .width,
            1
        );
    }
}
