use server::cache::{self, LIKED_PLAYLIST_ID};

use super::*;

/// 只报没缓存过的那些 id —— 已经有详情的不该再问平台要一遍。
///
/// 红心 973 首里点一个心,变的只有成员关系,详情一条都不必重取。
/// 这条要是错了,每点一次心就是 973 首的一次全量拉取,缓存等于没有。
#[tokio::test]
async fn missing_details_reports_only_the_uncached_ids() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_missing").await;

    cache::set_playlist(
        &mut tx,
        account.id,
        "p1",
        &[track("1", "已缓存"), track("2", "也缓存了")],
    )
    .await
    .expect("写缓存应该成功");

    let asked = [
        "1".to_owned(),
        "2".to_owned(),
        "3".to_owned(),
        "4".to_owned(),
    ];
    let mut missing =
        cache::missing_details(&mut tx, "netease", &asked)
            .await
            .expect("问缺哪些应该成功");
    missing.sort();

    assert_eq!(missing, ["3", "4"]);
}

/// 一个歌单能直接用别处缓存下来的详情。
///
/// 这是「详情按 (平台, id) 存、与歌单无关」在读路径上的兑现:收藏的歌单里的歌
/// 大半已经在红心里了,那些详情一条都不该重取。
#[tokio::test]
async fn a_playlist_can_reuse_details_cached_elsewhere() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_reuse").await;

    // 详情从红心那边进的库
    cache::set_playlist(
        &mut tx,
        account.id,
        LIKED_PLAYLIST_ID,
        &[track("1", "两处都有")],
    )
    .await
    .expect("写我喜欢的应该成功");

    // 另一个歌单只写成员关系,一条详情都不给
    cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[cache::TrackRef::new("1", None)],
    )
    .await
    .expect("写成员关系应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].title, "两处都有",
        "详情该是复用的那份"
    );
}

/// 成员关系里出现没有详情的 id 会被拒绝。
///
/// 插得进去的话它会在 JOIN 时悄悄消失 —— 歌单少一首,而没有任何人报错。
#[tokio::test]
async fn membership_without_details_is_rejected() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_orphan").await;

    let result = cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[cache::TrackRef::new("从没存过详情", None)],
    )
    .await;

    assert!(
        result.is_err(),
        "没有详情的成员关系不该插得进去"
    );
}

/// 按给定的 id 顺序读详情,与歌单无关。
///
/// 本地歌单的成员关系在自家表里,这里只借详情那一半 —— 把它也写进
/// `platform_playlist_tracks` 的话,本地歌单的整数 id 会和平台歌单的字符串 id
/// 撞在同一列上,而那时两个歌单会互相看见对方的歌。
#[tokio::test]
async fn details_of_follows_the_requested_order() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_details_order").await;

    cache::put_details(
        &mut tx,
        &[
            track("10", "甲"),
            track("20", "乙"),
            track("30", "丙"),
        ],
    )
    .await
    .expect("写详情应该成功");

    // 要的顺序与存的顺序不同
    let asked =
        ["30".to_owned(), "10".to_owned(), "20".to_owned()];
    let got = cache::details_of(&mut tx, "netease", &asked)
        .await
        .expect("读详情应该成功");

    let titles: Vec<&str> =
        got.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["丙", "甲", "乙"]);

    // 账号在这条路径上不参与:详情是全账号共用的
    let _ = account;
}

/// 没有详情的 id 直接跳过,不留空洞也不报错。
///
/// 平台不肯给的歌(下架、无权限)就是这样,而本地歌单里完全可能存着它 ——
/// 那时整个歌单打不开,比少一首更坏。
#[tokio::test]
async fn details_of_skips_ids_without_details() {
    let mut tx = tx().await;

    cache::put_details(&mut tx, &[track("1", "有详情")])
        .await
        .expect("写详情应该成功");

    let asked = ["1".to_owned(), "下架了".to_owned()];
    let got = cache::details_of(&mut tx, "netease", &asked)
        .await
        .expect("缺详情不该是错误");

    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "1");
}
