//! 曲目行的封面缩略图:滑进可见区才取,取到就记住。
//!
//! 与 `crate::artwork`(歌单封面)是两套,不是一套的两个用法 —— 差别在**键**:
//! 歌单封面按稳定的歌单 id 存,曲目缩略图按封面 URL 存。一张专辑封面被十几首歌
//! 共用,按 URL 存就只取一次、只解一次、只占一份内存;按曲目 id 存则是十几份。
//! (顺带避开一个静默的坑:歌单 id 与歌曲 id 都是平台的数字字符串,塞进同一张表
//! 会撞车,而撞车的现象是列表里顶着别人的封面 —— 两边都不报错。)
//!
//! 取不到就没有图。封面是装饰,CDN 会过期,失败是常态不是故障。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;

use crate::MainWindow;

/// 内存里最多留几张缩略图。
///
/// 96×96 RGBA 一张 36KB,512 张约 18MB。这个数字大到日常使用几乎碰不到 ——
/// 往回滚一屏图还在,不会出现"刚看过的又没了"。
const CAPACITY: usize = 512;

/// 滚动停下多久之后才真的去取。
///
/// 少了它,一次拉到底就是路过的每一行各发一条请求 —— 而滚动过程中本来也看不清图。
const DEBOUNCE: Duration = Duration::from_millis(150);

/// 一批最多取几个。一屏二十几行,40 留出上下余量。
const BATCH: usize = 40;

/// 待办表最多攒多少。超出就丢掉最旧的 —— 那些是已经滚过去的行。
///
/// 有这个上限,去重时的线性扫描才是有界的;不设的话快速滚过千行会让每次 push
/// 都扫一遍上千个字符串。
const PENDING_CAP: usize = BATCH * 4;

/// 封面 URL 变成一个安全的文件名。
///
/// URL 里有 `/` 与 `?`,不能直接当文件名;而 `artwork::cache_name` 那种"非法字符
/// 换成下划线"的做法在这里会把两个不同的 URL 映成同一个名字。散列则不会。
///
/// 用 blake3 而不是 std 的 `DefaultHasher`:这个名字要跨进程复用(它就是缓存
/// 文件名),而 `DefaultHasher` 的输出不保证跨 Rust 版本稳定。
pub fn cache_name(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    Some(blake3::hash(url.as_bytes()).to_hex().to_string())
}

/// 定容的缩略图表,满了从最久没碰过的开始丢。
#[derive(Default)]
struct Lru {
    entries: HashMap<String, slint::Image>,
    /// 使用顺序,最旧的在前。
    ///
    /// ponytail: 刷新新鲜度是一次线性扫描,上限 512 个字符串比较,不值得再引
    /// 一个侵入式链表。真要更快,换 `lru` crate 而不是自己写一个。
    order: VecDeque<String>,
}

impl Lru {
    fn contains(&self, url: &str) -> bool {
        self.entries.contains_key(url)
    }

    /// 拿一张,并把它刷成最新。
    fn get(&mut self, url: &str) -> Option<slint::Image> {
        let image = self.entries.get(url)?.clone();
        self.touch(url);
        Some(image)
    }

    fn touch(&mut self, url: &str) {
        if let Some(at) =
            self.order.iter().position(|seen| seen == url)
        {
            let key = self
                .order
                .remove(at)
                .expect("position 刚给出的下标不该越界");
            self.order.push_back(key);
        }
    }

    fn put(&mut self, url: String, image: slint::Image) {
        if self.entries.insert(url.clone(), image).is_some()
        {
            self.touch(&url);
        } else {
            self.order.push_back(url);
        }

        while self.order.len() > CAPACITY {
            let Some(oldest) = self.order.pop_front()
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// 等着去取的那些 URL,最新的在后。
#[derive(Default)]
struct Pending {
    queue: VecDeque<String>,
}

impl Pending {
    fn push(&mut self, url: String) {
        if self.queue.iter().any(|seen| *seen == url) {
            return;
        }
        self.queue.push_back(url);
        while self.queue.len() > PENDING_CAP {
            self.queue.pop_front();
        }
    }

    /// 取走最后一批,其余的扔掉。
    ///
    /// 扔掉的是用户已经滚过去的行 —— 它们现在不在屏幕上,取回来也没人看,
    /// 而它们会挤占带宽,让真正可见的那一屏更晚出图。
    fn take_last(&mut self, count: usize) -> Vec<String> {
        let stale = self.queue.len().saturating_sub(count);
        self.queue.drain(..stale);
        self.queue.drain(..).collect()
    }
}

/// 曲目缩略图的全部状态。克隆的是几个 `Rc`,克隆出来的仍是同一份。
#[derive(Clone, Default)]
pub struct Thumbnails {
    cache: Rc<RefCell<Lru>>,
    pending: Rc<RefCell<Pending>>,
    /// 已经在取的那些,免得同一张封面被发两次请求。
    inflight: Rc<RefCell<HashSet<String>>>,
    /// 防抖用的单次定时器。
    ///
    /// 它的回调只捕获上面那几个 `Rc`,不捕获 `Thumbnails` 自己 ——
    /// 捕获自己就成了 `Rc` 环,这个定时器到进程结束都不会被释放。
    timer: Rc<slint::Timer>,
}

impl Thumbnails {
    /// 某一行滑进了可见区,它要这张封面。
    ///
    /// 已经在手上的立刻摆;没有的进待办表,并把防抖往后推 —— 滚动不停就一直推。
    pub fn request(&self, ui: &MainWindow, url: &str) {
        if url.is_empty() {
            return;
        }

        if self.cache.borrow().contains(url) {
            apply(ui, &self.cache);
            return;
        }

        self.pending.borrow_mut().push(url.to_owned());

        let cache = self.cache.clone();
        let pending = self.pending.clone();
        let inflight = self.inflight.clone();
        let weak = ui.as_weak();
        self.timer.start(
            slint::TimerMode::SingleShot,
            DEBOUNCE,
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                flush(&ui, &cache, &pending, &inflight);
            },
        );
    }

    /// 把手上有的封面填进列表。换过一批歌之后要叫一次 ——
    /// 列表模型是整个换掉的,新模型里每一行的图都是空的。
    pub fn apply(&self, ui: &MainWindow) {
        apply(ui, &self.cache);
    }
}

/// 防抖到点:把待办表里最后一批取回来。
fn flush(
    ui: &MainWindow,
    cache: &Rc<RefCell<Lru>>,
    pending: &Rc<RefCell<Pending>>,
    inflight: &Rc<RefCell<HashSet<String>>>,
) {
    let batch = pending.borrow_mut().take_last(BATCH);

    for url in batch {
        if cache.borrow().contains(&url) {
            continue;
        }

        // 磁盘上那一份。命中就地解码 —— 一张 96px 的图,几毫秒的事。
        if let Some(name) = cache_name(&url)
            && let Some(bytes) =
                api::load_track_artwork(&name)
            && let Some(image) =
                crate::cover::decode_thumbnail(&bytes)
        {
            cache.borrow_mut().put(url, image);
            continue;
        }

        if !inflight.borrow_mut().insert(url.clone()) {
            continue;
        }

        let cache = cache.clone();
        let inflight = inflight.clone();
        let weak = ui.as_weak();
        let _ = slint::spawn_local(async move {
            let fetched = api::fetch_bytes(&url).await;
            inflight.borrow_mut().remove(&url);

            let Ok(bytes) = fetched else {
                // CDN 会过期、也会挡住不常见的 UA:取不到是常态
                log::debug!("取缩略图失败: {url}");
                return;
            };
            let Some(image) =
                crate::cover::decode_thumbnail(&bytes)
            else {
                log::debug!("缩略图不是图: {url}");
                return;
            };

            if let Some(name) = cache_name(&url) {
                api::save_track_artwork(&name, &bytes);
            }
            cache.borrow_mut().put(url, image);

            if let Some(ui) = weak.upgrade() {
                apply(&ui, &cache);
            }
        });
    }

    // 磁盘命中的那些这一帧就摆上,不必等网络那一批
    apply(ui, cache);
}

/// 把手上有的缩略图填进曲目列表里对应的行。
///
/// 与 `artwork::apply` 同一条规矩:每次有新图就整表扫一遍,而不是记住"这张图是
/// 第几行"—— 行会因为刷新、搜索、换歌单而换位置,记下的下标随时指向另一首歌。
fn apply(ui: &MainWindow, cache: &Rc<RefCell<Lru>>) {
    use slint::Model as _;

    let rows = ui.get_tracks();
    for i in 0..rows.row_count() {
        let Some(mut row) = rows.row_data(i) else {
            continue;
        };
        if row.cover.size().width > 0 {
            continue;
        }
        let Some(image) =
            cache.borrow_mut().get(row.cover_url.as_str())
        else {
            continue;
        };
        row.cover = image;
        rows.set_row_data(i, row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张 1×1 的图,只用来占位 —— 这些用例关心的是表怎么进怎么出。
    fn image() -> slint::Image {
        slint::Image::from_rgba8(
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                1, 1,
            ),
        )
    }

    /// 同一个 URL 永远算出同一个名字,不同 URL 算出不同的名字。
    ///
    /// 撞了的现象是列表里两首歌顶着同一张封面 —— 而两边都不报错。
    #[test]
    fn same_url_maps_to_the_same_file_name() {
        let one = cache_name("https://cdn/a.jpg");
        assert_eq!(one, cache_name("https://cdn/a.jpg"));
        assert_ne!(one, cache_name("https://cdn/b.jpg"));
    }

    /// 名字里不含路径分隔符。
    ///
    /// URL 来自平台,直接当文件名等于把缓存目录交给上游 —— 一个 `../../`
    /// 就能写到目录外面去。与 `artwork::cache_name` 同一条理由。
    #[test]
    fn a_file_name_cannot_escape_its_directory() {
        let name =
            cache_name("https://cdn/../../etc/passwd")
                .expect("非空 URL 应有名字");
        assert!(
            !name.contains('/'),
            "结果里不该有路径分隔符"
        );
        assert!(!name.contains('.'), "结果里不该有点");
    }

    /// 空 URL 没有缓存文件 —— 平台没给封面就是 `None`,不该和别的空串挤一起。
    #[test]
    fn an_empty_url_has_no_cache_file() {
        assert_eq!(cache_name(""), None);
    }

    /// 装满之后再放,最久没碰过的那张被挤出去。
    #[test]
    fn the_cache_evicts_the_least_recently_used() {
        let mut lru = Lru::default();
        for i in 0..CAPACITY {
            lru.put(format!("u{i}"), image());
        }
        lru.put("overflow".to_owned(), image());

        assert!(
            !lru.contains("u0"),
            "最旧的那张该被挤出去"
        );
        assert!(lru.contains("overflow"));
        assert_eq!(lru.entries.len(), CAPACITY);
    }

    /// 命中会刷新新鲜度。
    ///
    /// 少了这一步,正在看的那一屏会因为"进表早"而先被淘汰 —— 缓存在,却总不命中。
    #[test]
    fn a_hit_refreshes_the_entry() {
        let mut lru = Lru::default();
        for i in 0..CAPACITY {
            lru.put(format!("u{i}"), image());
        }
        lru.get("u0").expect("刚放进去的该在");
        lru.put("overflow".to_owned(), image());

        assert!(lru.contains("u0"), "碰过的那张不该先走");
        assert!(!lru.contains("u1"), "该走的是它后面那张");
    }

    /// 快速滚过几百行后,只取最后一批 —— 已经滚过去的行不该再占带宽。
    #[test]
    fn the_pending_set_keeps_only_the_last_batch() {
        let mut pending = Pending::default();
        for i in 0..PENDING_CAP {
            pending.push(format!("u{i}"));
        }

        let batch = pending.take_last(BATCH);

        assert_eq!(batch.len(), BATCH);
        assert_eq!(
            batch.last().map(String::as_str),
            Some(format!("u{}", PENDING_CAP - 1).as_str()),
            "最后进来的那个必须在这一批里"
        );
    }

    /// 取走一批之后待办表就空了 —— 剩下的是陈的,不该在下一轮又冒出来。
    #[test]
    fn taking_a_batch_drains_the_pending_set() {
        let mut pending = Pending::default();
        for i in 0..PENDING_CAP {
            pending.push(format!("u{i}"));
        }

        pending.take_last(BATCH);

        assert!(pending.take_last(BATCH).is_empty());
    }

    /// 同一个 URL 在待办表里只占一格。
    ///
    /// 一张专辑封面被十几首歌共用,不去重的话一屏就能把这一批塞满,
    /// 而其中十几个是同一张图。
    #[test]
    fn the_pending_set_dedupes_by_url() {
        let mut pending = Pending::default();
        pending.push("same".to_owned());
        pending.push("same".to_owned());
        pending.push("other".to_owned());

        assert_eq!(pending.take_last(BATCH).len(), 2);
    }
}
