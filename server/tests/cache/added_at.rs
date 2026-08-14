use server::cache;

use super::*;

/// 加入时间倒序压过数组下标:平台数组第 0 个若是最早收藏的,它要排到最后。
///
/// 这是 `docs/adr/0021` 的主张,也是「新点的红心看不到」那个报告的正解。
#[tokio::test]
async fn added_at_decides_the_order_not_the_array_index() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_added_at_order").await;

    let tracks = vec![
        track("1", "最早收藏"),
        track("2", "最晚收藏"),
        track("3", "居中"),
    ];
    cache::put_details(&mut tx, &tracks)
        .await
        .expect("写详情应该成功");

    // 数组顺序是 1/2/3,加入时间却是 2 最新、3 次之、1 最旧
    cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[
            cache::TrackRef::new("1", Some(1_000)),
            cache::TrackRef::new("2", Some(3_000)),
            cache::TrackRef::new("3", Some(2_000)),
        ],
    )
    .await
    .expect("写成员关系应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    let ids: Vec<&str> =
        got.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        ["2", "3", "1"],
        "该按加入时间倒序,而不是平台数组的下标"
    );
}

/// 边界:没有加入时间的成员关系(平台没给、或迁移前的老行)排在有时间的之后,
/// 它们之间仍按数组下标保持稳定 —— 即 `added_at DESC NULLS LAST, position ASC`。
#[tokio::test]
async fn tracks_without_added_at_fall_back_to_the_array_order()
 {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_added_at_nulls").await;

    cache::put_details(
        &mut tx,
        &[
            track("1", "有时间"),
            track("2", "没时间乙"),
            track("3", "没时间甲"),
        ],
    )
    .await
    .expect("写详情应该成功");

    // 只有 1 带时间。3 在数组里先于 2,没时间的两条要照这个次序跟在后面
    cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[
            cache::TrackRef::new("1", Some(1_000)),
            cache::TrackRef::new("3", None),
            cache::TrackRef::new("2", None),
        ],
    )
    .await
    .expect("写成员关系应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    let ids: Vec<&str> =
        got.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        ["1", "3", "2"],
        "没时间的该排在后面,它们之间仍按数组下标稳定"
    );
}

/// 加入时间跟着整表替换走。刷新后残留旧的时间会让顺序停在上一次的样子,
/// 而那种错不会有任何人报错。
#[tokio::test]
async fn refreshing_a_playlist_replaces_added_at_too() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_added_at_refresh")
            .await;

    cache::put_details(
        &mut tx,
        &[track("1", "甲"), track("2", "乙")],
    )
    .await
    .expect("写详情应该成功");

    cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[
            cache::TrackRef::new("1", Some(1_000)),
            cache::TrackRef::new("2", Some(2_000)),
        ],
    )
    .await
    .expect("首次写成员关系应该成功");

    // 用户在平台上把两首的先后调了个个儿
    cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[
            cache::TrackRef::new("1", Some(3_000)),
            cache::TrackRef::new("2", Some(2_000)),
        ],
    )
    .await
    .expect("刷新成员关系应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    let ids: Vec<&str> =
        got.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        ["1", "2"],
        "残留旧时间会让顺序停在上一次的样子"
    );
}

/// 加入时间属于**成员关系**,不属于曲目本身:同一首歌在两个歌单里各有各的
/// 加入时刻。写进 `platform_tracks` 就会让后写的那个歌单覆盖前一个。
#[tokio::test]
async fn the_same_track_can_have_a_different_added_at_per_playlist()
 {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_added_at_per_list")
            .await;

    cache::put_details(
        &mut tx,
        &[track("1", "同一首"), track("2", "陪衬")],
    )
    .await
    .expect("写详情应该成功");

    // p1 里 1 是老收藏,p2 里同一首是新收藏
    cache::set_membership(
        &mut tx,
        account.id,
        "p1",
        "netease",
        &[
            cache::TrackRef::new("1", Some(1_000)),
            cache::TrackRef::new("2", Some(2_000)),
        ],
    )
    .await
    .expect("写 p1 应该成功");
    cache::set_membership(
        &mut tx,
        account.id,
        "p2",
        "netease",
        &[
            cache::TrackRef::new("1", Some(3_000)),
            cache::TrackRef::new("2", Some(2_000)),
        ],
    )
    .await
    .expect("写 p2 应该成功");

    let first = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读 p1 应该成功");
    let second =
        cache::tracks_of(&mut tx, account.id, "p2")
            .await
            .expect("读 p2 应该成功");

    assert_eq!(
        first[0].id, "2",
        "p1 里 1 是老收藏,该排在后面"
    );
    assert_eq!(
        second[0].id, "1",
        "p2 里同一首是新收藏,该排在前面 —— \
         写进 platform_tracks 的话后写的会覆盖前一个"
    );
}

/// 迁移的老数据:`0005` 之前写下的成员关系没有加入时间,不能因此消失或报错,
/// 只是退回按数组下标排。
#[tokio::test]
async fn rows_written_before_the_migration_still_read_back()
{
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_added_at_legacy")
            .await;

    cache::put_details(
        &mut tx,
        &[track("1", "老行甲"), track("2", "老行乙")],
    )
    .await
    .expect("写详情应该成功");

    // 绕开 set_membership,照 0005 之前的形状直接插:没有 added_at 这一列的值
    for (id, position) in [("2", 0_i64), ("1", 1_i64)] {
        sqlx::query(
            "INSERT INTO platform_playlist_tracks
             (account_id, playlist_id, platform, track_id, position)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(account.id)
        .bind("p1")
        .bind("netease")
        .bind(id)
        .bind(position)
        .execute(&mut *tx)
        .await
        .expect("插老行应该成功");
    }

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("迁移前的老行该照样读得回来");

    let ids: Vec<&str> =
        got.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        ["2", "1"],
        "没有加入时间就退回按数组下标排,不该消失也不该报错"
    );
}
