//! 测试共用的曲目夹具。

use app_core::TrackDto;

pub(super) fn track() -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: "1".to_owned(),
        title: "紅蓮華".to_owned(),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 234_000,
    }
}

/// 指定 id 的一首歌,用来分辨列表里的行。
pub(super) fn track_with_id(id: &str) -> TrackDto {
    TrackDto {
        id: id.to_owned(),
        title: format!("歌 {id}"),
        ..track()
    }
}
