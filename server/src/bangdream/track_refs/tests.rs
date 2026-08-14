use similar_asserts::assert_eq;

use super::*;

fn dto(id: &str) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: id.to_owned(),
        alias: None,
        artists: Vec::new(),
        cover: None,
        duration_ms: 1,
    }
}

fn track_ref(id: &str) -> TrackRef {
    TrackRef::new(id, None)
}

/// 快路径:歌单详情一次把每首的详情都给全了,一次补拉都不必发。
#[test]
fn nothing_is_backfilled_when_detail_covers_every_ref() {
    let missing = refs_missing_from(
        &[dto("1"), dto("2")],
        &[track_ref("1"), track_ref("2")],
    );

    assert!(
        missing.is_empty(),
        "详情够全时不该有任何补拉,实际 {missing:?}"
    );
}

/// 平台把 `tracks` 截断时,只对差额补拉 —— 不是整份重取。
///
/// 这正是不敢直接吃 `tracks` 的理由:`trackIds` 是全量的,`tracks` 不是。
#[test]
fn only_the_refs_missing_from_detail_are_backfilled() {
    let missing = refs_missing_from(
        &[dto("1"), dto("3")],
        &[
            track_ref("1"),
            track_ref("2"),
            track_ref("3"),
            track_ref("4"),
        ],
    );

    assert_eq!(
        missing,
        vec!["2".to_owned(), "4".to_owned()],
        "只该补详情里没有的那些,且保持 refs 的次序"
    );
}

/// 常态:每条成员关系都有详情,一条都不剔,报 0。
#[test]
fn nothing_is_dropped_when_every_ref_has_details() {
    let (known, dropped) = keep_available(
        &[track_ref("1"), track_ref("2")],
        &HashSet::new(),
    );

    assert_eq!(known.len(), 2);
    assert_eq!(dropped, 0, "常态就是这一条");
}

/// 剔掉的正是拿不到详情的那些,剩下的保持原次序。
#[test]
fn only_the_refs_without_details_are_dropped() {
    let unavailable: HashSet<String> =
        ["2".to_owned()].into_iter().collect();

    let (known, dropped) = keep_available(
        &[track_ref("1"), track_ref("2"), track_ref("3")],
        &unavailable,
    );

    let ids: Vec<&str> =
        known.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["1", "3"], "过滤不该重排");
    assert_eq!(dropped, 1);
}

/// 报出来的条数是**这个歌单里**少的那些,不是那个集合的大小。
///
/// 集合里可能有不属于这个歌单的 id(它是按整批 id 问出来的)。拿集合大小
/// 当条数,用户会在这张列表上看到一个永远对不上的数字。
#[test]
fn the_dropped_count_ignores_ids_from_other_playlists() {
    let unavailable: HashSet<String> =
        ["2".to_owned(), "别的歌单里的".to_owned()]
            .into_iter()
            .collect();

    let (_known, dropped) = keep_available(
        &[track_ref("1"), track_ref("2")],
        &unavailable,
    );

    assert_eq!(
        dropped, 1,
        "只该数这个歌单里真的少掉的那一条"
    );
}

/// 边界:平台一条 `tracks` 都没给(整份被截断),那就全部补拉。
#[test]
fn an_empty_detail_backfills_every_ref() {
    let missing = refs_missing_from(
        &[],
        &[track_ref("1"), track_ref("2")],
    );

    assert_eq!(
        missing,
        vec!["1".to_owned(), "2".to_owned()]
    );
}
