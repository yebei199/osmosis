//! 平台曲目缓存的集成测试。
//!
//! 这里测的是「存得对、读得回」。刷新时机、以及界面什么时候看到新的那一份,
//! 是另一件事,断言的东西完全不同。
//!
//! 缓存不是镜像(见 `docs/adr/0018`):这些测试里没有一条断言「缓存里有而
//! 平台没有的东西要留着」—— 那正是它与镜像的分界线。

use contract::TrackDto;
use server::account::{Account, register};
use server::cache::{self, LIKED_PLAYLIST_ID};
use server::db;
use sqlx::{PgPool, Postgres, Transaction};

/// 与 `main.rs` 的默认值一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

const INVITE: &str = "let-me-in";

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );

    db::connect(&url).await.unwrap_or_else(|err| {
        panic!(
            "连不上数据库({url}): {err}\n\
             起一个:just pg"
        )
    })
}

/// 一个用完即回滚的事务。测试之间因此互不可见,也不留数据。
async fn tx() -> Transaction<'static, Postgres> {
    pool().await.begin().await.expect("开事务失败")
}

/// 造一个账号。用户名带上测试名,免得并行时撞在唯一索引上。
async fn make_account(
    tx: &mut Transaction<'static, Postgres>,
    username: &str,
) -> Account {
    register(tx, username, "correct horse", INVITE, INVITE)
        .await
        .expect("注册应该成功")
}

fn track(id: &str, title: &str) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: title.to_owned(),
        alias: None,
        artists: vec!["某人".to_owned()],
        cover: None,
        duration_ms: 200_000,
    }
}

/// 缓存里现有多少条曲目详情。直接查表:这几条测试要断言的正是
/// 「详情存了几份」,而那个数目没有任何对外接口会告诉你。
async fn detail_count(
    tx: &mut Transaction<'static, Postgres>,
) -> i64 {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM platform_tracks",
    )
    .fetch_one(&mut **tx)
    .await
    .expect("数详情条数失败");

    count
}

/// 存进去再读出来,拿到的是同一批曲目。
#[tokio::test]
async fn tracks_survive_a_round_trip() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_roundtrip").await;

    let tracks =
        vec![track("1", "第一首"), track("2", "第二首")];

    cache::set_playlist(&mut tx, account.id, "p1", &tracks)
        .await
        .expect("写缓存应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    assert_eq!(got, tracks);
}

/// 顺序按 position 还原,而不是按插入顺序或 id 大小 ——
/// 平台给的次序是有意义的,丢了用户的歌单就乱了。
#[tokio::test]
async fn tracks_come_back_in_playlist_order() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_order").await;

    // id 故意反着排:按 id 排序的实现会在这里露出来
    let tracks = vec![
        track("30", "甲"),
        track("10", "乙"),
        track("20", "丙"),
    ];

    cache::set_playlist(&mut tx, account.id, "p1", &tracks)
        .await
        .expect("写缓存应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    let titles: Vec<&str> =
        got.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["甲", "乙", "丙"]);
}

/// 再存一次同一个歌单是**替换**不是追加。
///
/// 平台那边删了一首,这边刷新之后也该没有它;写成追加的话,删掉的歌会永远
/// 留在缓存里,而且每刷一次列表就长一截。
#[tokio::test]
async fn refreshing_a_playlist_replaces_its_tracks() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_replace").await;

    cache::set_playlist(
        &mut tx,
        account.id,
        "p1",
        &[track("1", "留下"), track("2", "被删掉")],
    )
    .await
    .expect("首次写缓存应该成功");

    // 平台那边删掉了第二首
    cache::set_playlist(
        &mut tx,
        account.id,
        "p1",
        &[track("1", "留下")],
    )
    .await
    .expect("刷新缓存应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    assert_eq!(got.len(), 1, "删掉的那首不该还在");
    assert_eq!(got[0].id, "1");
}

/// 同一首歌在两个歌单里,详情只存一份 —— 这是 `platform_tracks`
/// 按 (平台, id) 做主键而不是跟着歌单走的全部理由。
#[tokio::test]
async fn the_same_track_in_two_playlists_is_stored_once() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_shared").await;

    let before = detail_count(&mut tx).await;

    let shared = track("1", "同一首");
    cache::set_playlist(
        &mut tx,
        account.id,
        "p1",
        std::slice::from_ref(&shared),
    )
    .await
    .expect("写第一个歌单应该成功");
    cache::set_playlist(
        &mut tx,
        account.id,
        "p2",
        std::slice::from_ref(&shared),
    )
    .await
    .expect("写第二个歌单应该成功");

    assert_eq!(
        detail_count(&mut tx).await - before,
        1,
        "详情该只多出一条"
    );

    // 两个歌单都还读得出它来 —— 去重不能把成员关系一起吞了
    for id in ["p1", "p2"] {
        let got = cache::tracks_of(&mut tx, account.id, id)
            .await
            .expect("读缓存应该成功");
        assert_eq!(
            got,
            vec![shared.clone()],
            "{id} 读不到"
        );
    }
}

/// 同一首再存一次是更新详情,不是插入失败也不是静默跳过。
/// 歌名改了、封面换了,刷新就该看到新的。
#[tokio::test]
async fn storing_a_track_again_updates_its_detail() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_upsert").await;

    cache::set_playlist(
        &mut tx,
        account.id,
        "p1",
        &[track("1", "旧名字")],
    )
    .await
    .expect("首次写缓存应该成功");

    let mut renamed = track("1", "新名字");
    renamed.cover =
        Some("https://example.invalid/a.jpg".into());

    cache::set_playlist(
        &mut tx,
        account.id,
        "p1",
        std::slice::from_ref(&renamed),
    )
    .await
    .expect("覆盖写应该成功");

    let got = cache::tracks_of(&mut tx, account.id, "p1")
        .await
        .expect("读缓存应该成功");

    assert_eq!(got, vec![renamed]);
}

/// 两个账号收藏了同一个平台歌单,各自刷各自那份,互不干扰。
#[tokio::test]
async fn playlists_are_scoped_to_the_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "cache_scope_a").await;
    let theirs =
        make_account(&mut tx, "cache_scope_b").await;

    cache::set_playlist(
        &mut tx,
        mine.id,
        "p1",
        &[track("1", "我的")],
    )
    .await
    .expect("写我的缓存应该成功");
    cache::set_playlist(
        &mut tx,
        theirs.id,
        "p1",
        &[track("2", "他的"), track("3", "他的另一首")],
    )
    .await
    .expect("写他的缓存应该成功");

    let got = cache::tracks_of(&mut tx, mine.id, "p1")
        .await
        .expect("读缓存应该成功");

    assert_eq!(got.len(), 1);
    assert_eq!(got[0].title, "我的");
}

/// 没缓存过的歌单读出来是空列表,不是错误。
/// 冷启动走的就是这条路,它要能安静地什么都不给。
#[tokio::test]
async fn an_uncached_playlist_reads_empty() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "cache_cold").await;

    let got =
        cache::tracks_of(&mut tx, account.id, "从没存过")
            .await
            .expect("读没缓存过的歌单不该是错误");

    assert!(got.is_empty());
}

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

/// 「我喜欢的」用保留 id,走的是同一张表、同一套代码。
#[tokio::test]
async fn the_liked_list_is_an_ordinary_playlist() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "cache_liked").await;

    let tracks = vec![track("1", "红心里的")];

    cache::set_playlist(
        &mut tx,
        account.id,
        LIKED_PLAYLIST_ID,
        &tracks,
    )
    .await
    .expect("写我喜欢的应该成功");

    let got = cache::tracks_of(
        &mut tx,
        account.id,
        LIKED_PLAYLIST_ID,
    )
    .await
    .expect("读我喜欢的应该成功");

    assert_eq!(got, tracks);
}

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
