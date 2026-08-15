//! URL 的构造:百分号编码,以及各条路由的地址。

use crate::base_url;

/// 百分号编码一个 URL 组件。
///
/// 手写而不是引 `percent-encoding`:规则就是"非 unreserved 字符逐字节转义",
/// 一个 crate 换不来更少的代码。unreserved 集合见 RFC 3986 §2.3。
pub(crate) fn encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => out.push(*byte as char),
            other => {
                out.push_str(&format!("%{other:02X}"));
            }
        }
    }
    out
}

/// 拼搜索地址,关键词按 URL 查询串规则转义。
///
/// 抽出来单独可测:关键词直接插进 `format!` 的话,一个 `&` 就会把查询串截成
/// 两个参数,服务端只看到半截关键词 —— 而这既不会报错,也不会有测试失败。
pub(crate) fn search_url(
    kind: &str,
    keyword: &str,
) -> String {
    format!(
        "{}/search/{kind}?q={}",
        base_url(),
        encode_component(keyword)
    )
}

/// 拼播放地址。id 是路径的一段,同样要转义。
pub(crate) fn play_url(track_id: &str) -> String {
    format!(
        "{}/play/{}",
        base_url(),
        encode_component(track_id)
    )
}

/// `/lyric/{track_id}` 的完整地址。id 同样要转义(理由见 [`play_url`])。
pub(crate) fn lyric_url(track_id: &str) -> String {
    format!(
        "{}/lyric/{}",
        base_url(),
        encode_component(track_id)
    )
}

/// 本地歌单的地址。路径里带上 `local`,因为两种歌单的 id **不在同一个空间**:
/// 本地是整数主键,平台是平台自己的字符串 id。混了的现象是「查无此歌单」,
/// 看起来像数据没了。
pub(crate) fn playlist_url(id: &str) -> String {
    format!(
        "{}/playlists/local/{}",
        base_url(),
        encode_component(id)
    )
}

pub(crate) fn playlist_tracks_url(id: &str) -> String {
    format!("{}/tracks", playlist_url(id))
}

/// 平台歌单曲目的地址。
pub(crate) fn platform_playlist_tracks_url(
    id: &str,
) -> String {
    format!(
        "{}/playlists/platform/{}/tracks",
        base_url(),
        encode_component(id)
    )
}

/// 歌手热门曲目的地址。
pub(crate) fn artist_tracks_url(id: &str) -> String {
    format!(
        "{}/artists/{}/tracks",
        base_url(),
        encode_component(id)
    )
}

pub(crate) fn liked_url(track_id: &str) -> String {
    format!(
        "{}/liked/{}",
        base_url(),
        encode_component(track_id)
    )
}

pub(crate) fn subscription_url(
    playlist_id: &str,
) -> String {
    format!(
        "{}/subscriptions/playlists/{}",
        base_url(),
        encode_component(playlist_id)
    )
}

#[cfg(test)]
mod tests;
