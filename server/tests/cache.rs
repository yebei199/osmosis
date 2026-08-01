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
    "postgres://slint:devonly@127.0.0.1:5432/slint_study";

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
