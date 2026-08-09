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

    for rows in
        [ui.get_playlists(), ui.get_found_playlists()]
    {
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
    if let Some(image) =
        art.get(ui.get_open_playlist_id().as_str())
    {
        ui.set_open_playlist_cover(image);
    }
}

#[cfg(test)]
mod tests {
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
}
