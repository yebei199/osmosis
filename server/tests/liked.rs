//! 「我的喜欢」的集成测试(#146)。
//!
//! 它是一个系统自带的本地歌单:真相在自家库,网易云只是一次性导入的来源。
//! 这里测存储层对着真库的行为 —— 导入的次序与加入时间、重跑不重复、
//! 点心与取消只动这一份、它不能被当成普通本地歌单改名或删掉。

use server::error::AppError;
use server::store::account::{Account, register};
use server::store::cache::TrackRef as ImportRef;
use server::store::db;
use server::store::liked;
use server::store::playlist::{self, TrackRef};
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

fn ids(refs: &[TrackRef]) -> Vec<&str> {
    refs.iter().map(|t| t.track_id.as_str()).collect()
}

/// 每首歌在「我的喜欢」里的加入时刻,毫秒,按曲目 id 排。
async fn added_at_ms(
    tx: &mut Transaction<'static, Postgres>,
    account: &Account,
) -> Vec<(String, Option<i64>)> {
    sqlx::query_as(
        "SELECT t.track_id,
                (extract(epoch FROM t.added_at) * 1000)::bigint
         FROM local_playlist_tracks t
         JOIN local_playlists p ON p.id = t.playlist_id
         WHERE p.account_id = $1 AND p.system = 'liked'
         ORDER BY t.track_id",
    )
    .bind(account.id)
    .fetch_all(&mut **tx)
    .await
    .expect("读加入时间失败")
}

/// 「我的喜欢」不出现在普通本地歌单的列表里,也不能被改名或删掉。
///
/// 它在歌单页上由 `merged` 单独置顶;混进普通列表的话会出现两个,而其中
/// 那个「本地」的带着删除键。
#[tokio::test]
async fn liked_is_not_an_ordinary_local_playlist() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "lk_system").await;

    let id = liked::ensure(&mut tx, account.id)
        .await
        .expect("建「我的喜欢」应该成功");
    let again = liked::ensure(&mut tx, account.id)
        .await
        .expect("再要一次应该拿到同一个");
    assert_eq!(id, again, "每个账号只有一个「我的喜欢」");

    let listed =
        playlist::list(&mut tx, account.id).await.unwrap();
    assert!(
        listed.is_empty(),
        "普通列表里不该有它: {listed:?}"
    );

    assert!(matches!(
        playlist::rename(&mut tx, account.id, id, "改掉")
            .await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        playlist::delete(&mut tx, account.id, id).await,
        Err(AppError::NotFound)
    ));
}

/// 导入保留网易云的次序与加入时间,最近加的在最前。
///
/// 网易云的红心歌单按加入时间倒序给(`docs/adr/0021`);两首同一时刻、或者
/// 平台没给时间的,仍按平台给的先后排 —— 次序靠 position 兜住。
#[tokio::test]
async fn import_keeps_the_platform_order_and_added_at() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "lk_import").await;

    let from_netease = [
        ImportRef::new("c", Some(3_000)),
        ImportRef::new("b", Some(2_000)),
        ImportRef::new("b2", Some(2_000)),
        ImportRef::new("a", Some(1_000)),
        ImportRef::new("x", None),
    ];
    let added = liked::import(
        &mut tx,
        account.id,
        "netease",
        &from_netease,
    )
    .await
    .expect("导入应该成功");
    assert_eq!(added, 5);

    let refs =
        liked::refs(&mut tx, account.id).await.unwrap();
    assert_eq!(ids(&refs), vec!["c", "b", "b2", "a", "x"]);
    assert_eq!(
        added_at_ms(&mut tx, &account).await,
        vec![
            ("a".to_owned(), Some(1_000)),
            ("b".to_owned(), Some(2_000)),
            ("b2".to_owned(), Some(2_000)),
            ("c".to_owned(), Some(3_000)),
            ("x".to_owned(), None),
        ]
    );
}

/// 重跑导入不重复,只补新增,也不删这边已经有的。
///
/// 用户定的:导一次,之后以我们的为准。再导一次是为了捞回在网易云官方 App
/// 里新点的心,不是拿网易云覆盖这边。
#[tokio::test]
async fn reimport_only_adds_what_is_new() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "lk_reimport").await;

    liked::import(
        &mut tx,
        account.id,
        "netease",
        &[
            ImportRef::new("b", Some(2_000)),
            ImportRef::new("a", Some(1_000)),
        ],
    )
    .await
    .unwrap();
    // 在我们这边点的心,网易云那边没有
    liked::set(&mut tx, account.id, &track("mine"), true)
        .await
        .unwrap();

    let added = liked::import(
        &mut tx,
        account.id,
        "netease",
        &[
            ImportRef::new("new", Some(5_000)),
            ImportRef::new("b", Some(2_000)),
            ImportRef::new("a", Some(1_000)),
        ],
    )
    .await
    .expect("再导一次应该成功");
    assert_eq!(added, 1, "只有 new 是新的");

    let refs =
        liked::refs(&mut tx, account.id).await.unwrap();
    // mine 是此刻点的,比 5 秒(1970 年)晚得多
    assert_eq!(ids(&refs), vec!["mine", "new", "b", "a"]);
}

/// 点心加到最前,重复点不重复;取消只删这一首。
#[tokio::test]
async fn like_and_unlike_edit_our_own_liked() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "lk_toggle").await;

    liked::import(
        &mut tx,
        account.id,
        "netease",
        &[ImportRef::new("old", Some(1_000))],
    )
    .await
    .unwrap();

    liked::set(&mut tx, account.id, &track("new"), true)
        .await
        .expect("点心应该成功");
    liked::set(&mut tx, account.id, &track("new"), true)
        .await
        .expect("重复点心不该报错");
    assert_eq!(
        ids(&liked::refs(&mut tx, account.id)
            .await
            .unwrap()),
        vec!["new", "old"]
    );
    assert_eq!(
        liked::count(&mut tx, account.id).await.unwrap(),
        2
    );

    liked::set(&mut tx, account.id, &track("old"), false)
        .await
        .expect("取消应该成功");
    liked::set(
        &mut tx,
        account.id,
        &track("absent"),
        false,
    )
    .await
    .expect("取消一首本来就没有的不该报错");
    assert_eq!(
        ids(&liked::refs(&mut tx, account.id)
            .await
            .unwrap()),
        vec!["new"]
    );
}

/// 没导入过、也没点过心的账号:空的,不是错误。
#[tokio::test]
async fn a_fresh_account_has_an_empty_liked() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "lk_fresh").await;

    assert!(
        liked::refs(&mut tx, account.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        liked::count(&mut tx, account.id).await.unwrap(),
        0
    );
}

/// 各账号的「我的喜欢」互不可见。
#[tokio::test]
async fn liked_is_scoped_to_its_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "lk_scope_a").await;
    let theirs = make_account(&mut tx, "lk_scope_b").await;

    liked::set(&mut tx, theirs.id, &track("t"), true)
        .await
        .unwrap();

    assert!(
        liked::refs(&mut tx, mine.id)
            .await
            .unwrap()
            .is_empty()
    );
}
