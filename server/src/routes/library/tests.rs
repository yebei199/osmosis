//! 歌单类出口的排序(`?sort=`,ADR 0042):纯函数,不碰库。

use contract::{
    TrackDto, TrackFacetsDto, TrackSortDto, TracksDto,
};
use similar_asserts::assert_eq;

use super::sorted;

/// 一首歌:喜爱度 `score`,放过 `plays` 次。
fn track(id: &str, score: i16, plays: u32) -> TrackDto {
    TrackDto {
        artist_identities: Vec::new(),
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: id.to_owned(),
        alias: None,
        artists: Vec::new(),
        cover: None,
        duration_ms: 200_000,
        album: None,
        facets: TrackFacetsDto {
            score,
            play_count: plays,
            ..TrackFacetsDto::default()
        },
    }
}

/// 按加入时间(新的在前)摆好的一批。
fn found() -> TracksDto {
    TracksDto {
        tracks: vec![
            track("a", 0, 7),
            track("b", 464, 1),
            track("c", 800, 0),
            track("d", 464, 7),
        ],
        unavailable: 0,
        hidden: 0,
    }
}

fn ids(found: &TracksDto) -> Vec<&str> {
    found.tracks.iter().map(|t| t.id.as_str()).collect()
}

#[test]
fn added_keeps_the_order_it_came_in() {
    assert_eq!(
        ids(&sorted(found(), TrackSortDto::Added)),
        vec!["a", "b", "c", "d"]
    );
}

/// 同分的保持加入次序:b 比 d 加得晚,排在前面。
#[test]
fn score_puts_the_most_loved_first_and_is_stable() {
    assert_eq!(
        ids(&sorted(found(), TrackSortDto::Score)),
        vec!["c", "b", "d", "a"]
    );
}

#[test]
fn plays_puts_the_most_played_first_and_is_stable() {
    assert_eq!(
        ids(&sorted(found(), TrackSortDto::Plays)),
        vec!["a", "d", "b", "c"]
    );
}
