use super::*;

/// 关键词里的 `&`、空格、中文都必须转义。
///
/// 不转义的话 `q=a&b` 会被服务端解析成两个参数,关键词静默变成半截 ——
/// 不报错、不失败,只是搜出来的东西不对。
#[test]
fn search_url_percent_encodes_keyword() {
    let url = search_url("tracks", "紅蓮華 & LiSA");

    assert!(
        url.ends_with(
            "/search/tracks?q=%E7%B4%85%E8%93%AE%E8%8F%AF%20%26%20LiSA"
        ),
        "关键词没被完整转义: {url}"
    );
}

/// 路径拼接:id 原样落在 `/play/` 之后。
#[test]
fn play_url_contains_track_id() {
    assert!(
        play_url("1375305989")
            .ends_with("/play/1375305989"),
        "id 没落在路径末尾"
    );
}

/// 歌词地址与播放地址同构,id 一样要落在路径末尾。
#[test]
fn lyric_url_contains_track_id() {
    assert!(
        lyric_url("1375305989")
            .ends_with("/lyric/1375305989"),
        "id 没落在路径末尾"
    );
}

/// 歌单相关的三种地址各自成形,且 id 进路径要转义 ——
/// 不转义的话,一个带斜杠的 id 会把路径截成另一条路由。
#[test]
fn playlist_urls_are_built_per_kind() {
    assert!(
        playlist_url("3").ends_with("/playlists/local/3"),
        "实际 {}",
        playlist_url("3")
    );
    assert!(
        playlist_tracks_url("3")
            .ends_with("/playlists/local/3/tracks")
    );
    // 两种来源走两条路径 —— 混了的现象是「查无此歌单」,看着像数据没了
    assert!(
        platform_playlist_tracks_url("24381616").ends_with(
            "/playlists/platform/24381616/tracks"
        )
    );
    assert!(
        subscription_url("24381616")
            .ends_with("/subscriptions/playlists/24381616")
    );
    assert!(liked_url("347230").ends_with("/liked/347230"));
    assert!(
        artist_tracks_url("11972")
            .ends_with("/artists/11972/tracks")
    );
}

/// id 里的斜杠与空格都要转义。
#[test]
fn track_ids_are_escaped_in_paths() {
    assert!(
        liked_url("a/b c").ends_with("/liked/a%2Fb%20c")
    );
    assert!(
        playlist_url("a/b")
            .ends_with("/playlists/local/a%2Fb")
    );
    // 平台 id 来自平台,更该转义:它可能带任何字符
    assert!(
        platform_playlist_tracks_url("a/b")
            .ends_with("/playlists/platform/a%2Fb/tracks")
    );
}
