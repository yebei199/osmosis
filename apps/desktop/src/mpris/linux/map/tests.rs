use super::super::tests::now_playing;
use super::*;

/// 曲目 id 要变成一条合法的 object path。
///
/// `mpris:trackid` 的类型是 object path,不是字符串 —— 平台 id 里的 `-`、`.`
/// 甚至中文会让 D-Bus 拒收**整条** Metadata,于是 bar 上什么都不显示。
#[test]
fn a_track_id_becomes_a_valid_object_path() {
    let path = track_path("ne-1962165898.v2");

    assert_eq!(
        path,
        "/io/github/osmosis/track/ne_1962165898_v2"
    );
    // 真正的判据不是长相,是 D-Bus 收不收。
    assert!(ObjectPath::try_from(path).is_ok());
    assert!(
        ObjectPath::try_from(track_path("尼古喵喵"))
            .is_ok()
    );
}

/// 时长以微秒报出。
///
/// seam 那边统一是毫秒,`mpris:length` 是微秒。差这一个千倍,进度条会缩成
/// 一条几乎不动的线,而且看起来「像是对的」。
#[test]
fn metadata_carries_length_in_microseconds() {
    let map = metadata_of(&now_playing());

    let length = i64::try_from(
        map["mpris:length"].try_clone().unwrap(),
    )
    .unwrap();
    assert_eq!(length, 240_000_000);
}

/// 艺术家保持列表形态。
///
/// `xesam:artist` 的类型是字符串数组。join 成一句会让外面拿到一个
/// 名叫「甲/乙」的人。
#[test]
fn artists_stay_a_list() {
    let map = metadata_of(&now_playing());

    let artists = Vec::<String>::try_from(
        map["xesam:artist"].try_clone().unwrap(),
    )
    .unwrap();
    assert_eq!(artists, ["一个狼人", "另一个"]);
}

/// 没有封面就不写这个键。
///
/// `mpris:artUrl` 给空串比不给更糟:外面会当成一条取不到的图去拉,
/// 拉失败之后未必回退到占位图。
#[test]
fn a_track_without_a_cover_omits_art_url() {
    let mut now = now_playing();
    now.art_url = None;

    let map = metadata_of(&now);

    assert!(!map.contains_key("mpris:artUrl"));
    assert!(map.contains_key("xesam:title"));
}

/// 什么都没放时报 NoTrack。
///
/// 规范给了一条专用路径。空 Metadata 与「有一首歌但字段都是空的」在客户端
/// 那头长得一样,而后者会让 bar 挂着一行空白。
#[test]
fn an_empty_now_playing_reports_the_no_track_path() {
    let map = metadata_of(&ui::NowPlaying::default());

    let id = ObjectPath::try_from(
        map["mpris:trackid"].try_clone().unwrap(),
    )
    .unwrap();
    assert_eq!(id.as_str(), NO_TRACK);
    // 没有歌就别摆歌名与时长的空壳。
    assert!(!map.contains_key("xesam:title"));
    assert!(!map.contains_key("mpris:length"));
}
