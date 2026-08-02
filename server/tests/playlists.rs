//! 歌单的集成测试。
//!
//! 两类歌单在契约层归一成同一个 `PlaylistDto`,靠 `source` 区分(见 `docs/adr/0016`)。
//! 平台歌单的真相在网易云,这里只测**本地**那一半与两者的合并规则 ——
//! 打真实网易云的部分归 `live_bangdream.rs`。

use contract::PlaylistSource;
use server::account::{Account, register};
use server::db;
use server::error::AppError;
use server::playlist::{self, TrackRef};
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

fn track(id: &str) -> TrackRef {
    TrackRef {
        platform: "netease".to_owned(),
        track_id: id.to_owned(),
    }
}

/// 建出来的歌单能按账号列出来,带的字段与建时给的一致。
#[tokio::test]
async fn creating_a_local_playlist_then_listing_it() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "pl_create").await;

    let created =
        playlist::create(&mut tx, account.id, "睡前")
            .await
            .expect("建歌单应该成功");

    let listed = playlist::list(&mut tx, account.id)
        .await
        .expect("列歌单应该成功");

    assert_eq!(listed, vec![created.clone()]);
    assert_eq!(created.name, "睡前");
    assert_eq!(created.track_count, 0);
}

/// 只列自己的:另一个账号的本地歌单不会出现在我的列表里。
/// 这是多账号最容易做错的一处,而单账号自测永远发现不了。
#[tokio::test]
async fn local_playlists_are_scoped_to_their_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "pl_scope_a").await;
    let theirs = make_account(&mut tx, "pl_scope_b").await;

    playlist::create(&mut tx, mine.id, "我的")
        .await
        .unwrap();
    playlist::create(&mut tx, theirs.id, "别人的")
        .await
        .unwrap();

    let listed =
        playlist::list(&mut tx, mine.id).await.unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "我的");
}

/// 改名与删除只作用于自己的歌单;动别人的歌单按"不存在"处理,
/// 不能回 403 —— 那等于确认了这个 id 存在。
#[tokio::test]
async fn renaming_or_deleting_another_accounts_playlist_is_not_found()
 {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "pl_other_a").await;
    let theirs = make_account(&mut tx, "pl_other_b").await;

    let victim =
        playlist::create(&mut tx, theirs.id, "别人的")
            .await
            .unwrap();

    assert!(matches!(
        playlist::rename(
            &mut tx, mine.id, victim.id, "改了"
        )
        .await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        playlist::delete(&mut tx, mine.id, victim.id).await,
        Err(AppError::NotFound)
    ));

    // 而且真的没动
    let theirs_still =
        playlist::list(&mut tx, theirs.id).await.unwrap();
    assert_eq!(theirs_still, vec![victim]);
}

/// 往本地歌单加曲目后能按顺序取回;顺序是加入顺序,不是曲目 id 的顺序。
#[tokio::test]
async fn tracks_come_back_in_the_order_they_were_added() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "pl_order").await;
    let list =
        playlist::create(&mut tx, account.id, "顺序")
            .await
            .unwrap();

    // 故意逆着 id 的大小加
    playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        &[track("300"), track("100"), track("200")],
    )
    .await
    .expect("加曲目应该成功");

    let tracks =
        playlist::tracks(&mut tx, account.id, list.id)
            .await
            .unwrap();

    assert_eq!(
        tracks,
        vec![track("300"), track("100"), track("200")]
    );
}

/// 同一首歌重复加不产生第二条,也不报错 —— 用户点两下加入的意图是一样的。
#[tokio::test]
async fn adding_the_same_track_twice_is_idempotent() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "pl_dup").await;
    let list =
        playlist::create(&mut tx, account.id, "重复")
            .await
            .unwrap();

    playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        &[track("1")],
    )
    .await
    .unwrap();
    playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        &[track("1")],
    )
    .await
    .expect("重复加入不该报错");

    let tracks =
        playlist::tracks(&mut tx, account.id, list.id)
            .await
            .unwrap();

    assert_eq!(tracks, vec![track("1")]);
}

/// 从歌单里移掉一首,其余顺序不变。
#[tokio::test]
async fn removing_a_track_keeps_the_rest_in_order() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "pl_remove").await;
    let list =
        playlist::create(&mut tx, account.id, "移除")
            .await
            .unwrap();

    playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        &[track("1"), track("2"), track("3")],
    )
    .await
    .unwrap();

    playlist::remove_tracks(
        &mut tx,
        account.id,
        list.id,
        &[track("2")],
    )
    .await
    .expect("移除应该成功");

    let tracks =
        playlist::tracks(&mut tx, account.id, list.id)
            .await
            .unwrap();

    assert_eq!(tracks, vec![track("1"), track("3")]);
}

/// 删掉歌单时它的曲目关联一并消失,不留孤儿行。
#[tokio::test]
async fn deleting_a_playlist_takes_its_tracks_with_it() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "pl_cascade").await;
    let list =
        playlist::create(&mut tx, account.id, "级联")
            .await
            .unwrap();

    playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        &[track("1"), track("2")],
    )
    .await
    .unwrap();

    playlist::delete(&mut tx, account.id, list.id)
        .await
        .expect("删除应该成功");

    let orphans: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM local_playlist_tracks WHERE playlist_id = $1",
    )
    .bind(list.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    assert_eq!(orphans.0, 0);
}

/// 合并后的列表里:「我喜欢的」在最前,本地歌单与平台歌单都带上正确的 source。
/// 客户端靠 source 决定哪些操作可用,标错了会让删除键出现在平台歌单上。
#[tokio::test]
async fn merged_list_puts_liked_first_and_marks_each_source()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "pl_merge").await;

    let local =
        playlist::create(&mut tx, account.id, "本地的")
            .await
            .unwrap();
    let locals =
        playlist::list(&mut tx, account.id).await.unwrap();

    let platform = vec![contract::PlaylistDto {
        source: PlaylistSource::Platform,
        id: "24381616".to_owned(),
        name: "平台的".to_owned(),
        cover: None,
        track_count: 120,
    }];

    let merged = playlist::merged(7, platform, locals);

    assert_eq!(merged[0].source, PlaylistSource::Liked);
    assert_eq!(merged[0].track_count, 7);

    let by_source: Vec<_> =
        merged.iter().map(|list| list.source).collect();
    assert_eq!(
        by_source,
        vec![
            PlaylistSource::Liked,
            PlaylistSource::Local,
            PlaylistSource::Platform,
        ]
    );

    // 本地那条带的是本地歌单的 id,不是平台 id —— 混了会让改名打到别人身上
    assert_eq!(merged[1].id, local.id.to_string());
}
