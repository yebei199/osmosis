//! 封面字节的取用与落盘门面,平台差异在 [`crate::platform`] 里。

use crate::{ApiError, platform};

/// 拉取任意 URL 的原始字节(封面图这类二进制资源)。
///
/// 与 `play_source` 的直链同一注意事项:封面 URL 指向平台 CDN,可能过期或
/// 返回 HTML 错误页 —— 调用方必须把「字节不是图」当常态处理,不能 panic。
pub async fn fetch_bytes(
    url: &str,
) -> Result<Vec<u8>, ApiError> {
    platform::get_bytes(url.to_owned()).await
}

/// 读一张缓存下来的封面。没有就是没有。
///
/// `name` 必须已经过调用方的过滤(见 `ui::artwork::cache_name`)——
/// 它会成为路径的一段,而歌单标识来自平台。
pub fn load_artwork(name: &str) -> Option<Vec<u8>> {
    platform::load_artwork(name)
}

/// 存一张封面。失败只记一笔:封面是装饰,存不下只是下次再取一遍。
pub fn save_artwork(name: &str, bytes: &[u8]) {
    platform::save_artwork(name, bytes);
}

/// 曲目缩略图目录的字节上限。
///
/// 歌单封面按稳定的歌单 id 存,取多少就是多少;曲目缩略图按**封面 URL** 存,
/// 而 CDN 会换 URL —— 换掉那一刻旧文件就再没人会查,却还占着盘。所以这一层
/// 必须有个硬上限,而歌单那一层不需要。
pub const TRACK_ARTWORK_BUDGET: u64 = 64 * 1024 * 1024;

/// 读一张缓存下来的曲目缩略图。
///
/// 与 [`load_artwork`] 分开是因为两者的键不同(URL 的散列 vs 歌单 id),
/// 因而淘汰规则也不同 —— 混在一个目录里,清理会误伤歌单封面。
pub fn load_track_artwork(name: &str) -> Option<Vec<u8>> {
    platform::load_track_artwork(name)
}

/// 存一张曲目缩略图。与 [`save_artwork`] 同理,失败只记一笔。
pub fn save_track_artwork(name: &str, bytes: &[u8]) {
    platform::save_track_artwork(name, bytes);
}

/// 把曲目缩略图目录削回 [`TRACK_ARTWORK_BUDGET`] 以内,从最旧的删起。
///
/// 进程启动时跑一次就够:几百个文件的 `metadata()` 是毫秒级,而放在写入路径上
/// 会让滚一次列表 stat 整个目录几十遍。
pub fn sweep_track_artwork() {
    platform::sweep_track_artwork(TRACK_ARTWORK_BUDGET);
}
