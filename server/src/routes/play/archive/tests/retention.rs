//! 留多久、放不下时谁让位:清扫的保留规则、名次、空间上限的取舍与统计入口(#126、#147)。

use similar_asserts::assert_eq;

use super::*;

/// 在事务里记一首存歌(连同桶里的对象),最后一次播放拨回 `days_ago` 天前。
async fn stored_days_ago(
    tx: &mut sqlx::PgConnection,
    objects: &MemoryObjects,
    id: &str,
    days_ago: i32,
) -> String {
    stored_at(tx, objects, id, days_ago, &quality()).await
}

/// 同上,按 `quality` 那一档记。
async fn stored_at(
    tx: &mut sqlx::PgConnection,
    objects: &MemoryObjects,
    id: &str,
    days_ago: i32,
    quality: &str,
) -> String {
    use server::objects::Objects as _;

    let key = format!("tracks/{id}/{quality}.flac");
    objects.put(&key, audio(), "audio/flac").await.unwrap();
    ledger::record(
        tx,
        &ledger::StoredTrack {
            platform: "netease".to_owned(),
            track_id: id.to_owned(),
            quality: quality.to_owned(),
            object_key: key.clone(),
            format: "flac".to_owned(),
            bit_rate: 999_000,
            bytes: 4096,
            tier: quality.to_owned(),
            bits_per_sample: None,
            sample_rate: None,
        },
    )
    .await
    .expect("记账失败");
    sqlx::query(
        "UPDATE stored_tracks
         SET last_played_at = now() - $2 * interval '1 day'
         WHERE track_id = $1",
    )
    .bind(id)
    .bind(days_ago)
    .execute(&mut *tx)
    .await
    .expect("拨时间失败");
    key
}

/// 把一首放进某个账号的「我的喜欢」(#146 起红心归自家库)。
async fn liked_by(
    tx: &mut sqlx::PgConnection,
    account: &Account,
    id: &str,
) {
    server::store::liked::set(
        tx,
        account.id,
        &TrackRef {
            platform: "netease".to_owned(),
            track_id: id.to_owned(),
        },
        true,
    )
    .await
    .expect("写红心失败");
}

/// 把一首放进这个账号新建的一个普通歌单。
async fn in_playlist(
    tx: &mut sqlx::PgConnection,
    account: &Account,
    id: &str,
) {
    let list = server::store::playlist::create(
        &mut *tx,
        account.id,
        &format!("歌单 {id}"),
    )
    .await
    .expect("建歌单失败");
    server::store::playlist::add_tracks(
        tx,
        account.id,
        list.id,
        &[netease(id)],
    )
    .await
    .expect("加歌失败");
}

/// 清理只删「哪都不在、且三天没播」的;红心的、在歌单或日推里的、三天内播过的都留着。
#[tokio::test]
async fn the_sweep_keeps_liked_and_recent_tracks() {
    let f = fixture("ar_sweep", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let old = testing::track_id("ar_sweep", 1);
    let liked = testing::track_id("ar_sweep", 2);
    let recent = testing::track_id("ar_sweep", 3);
    let listed = testing::track_id("ar_sweep", 4);
    let picked = testing::track_id("ar_sweep", 5);
    let listed_key =
        stored_days_ago(&mut tx, &f.objects, &listed, 4)
            .await;
    in_playlist(&mut tx, &f.account, &listed).await;
    let picked_key =
        stored_days_ago(&mut tx, &f.objects, &picked, 4)
            .await;
    server::store::daily::replace(
        &mut tx,
        f.account.id,
        &[netease(&picked)],
    )
    .await
    .unwrap();

    let old_key =
        stored_days_ago(&mut tx, &f.objects, &old, 4).await;
    let liked_key =
        stored_days_ago(&mut tx, &f.objects, &liked, 4)
            .await;
    liked_by(&mut tx, &f.account, &liked).await;
    let recent_key =
        stored_days_ago(&mut tx, &f.objects, &recent, 2)
            .await;

    super::super::sweep(&mut tx, f.objects.as_ref())
        .await
        .expect("清理应当成功");

    assert_eq!(f.objects.get(&old_key), None);
    assert!(f.objects.get(&liked_key).is_some());
    assert!(f.objects.get(&recent_key).is_some());
    // 普通歌单与当天日推里的也长期留着(#147)
    assert!(f.objects.get(&listed_key).is_some());
    assert!(f.objects.get(&picked_key).is_some());
    let left = |id: &str| {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM stored_tracks WHERE track_id = $1",
        )
        .bind(id.to_owned())
    };
    assert_eq!(
        left(&old).fetch_one(&mut *tx).await.unwrap(),
        0
    );
    assert_eq!(
        left(&liked).fetch_one(&mut *tx).await.unwrap(),
        1
    );
    assert_eq!(
        left(&recent).fetch_one(&mut *tx).await.unwrap(),
        1
    );
}

/// #147 之前按 320k 存的那批不论红心、不论新旧,连对象带账清掉。
#[tokio::test]
async fn the_sweep_purges_the_old_high_copies() {
    let f = fixture("ar_legacy", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let id = testing::track_id("ar_legacy", 1);
    let key =
        stored_at(&mut tx, &f.objects, &id, 0, "high")
            .await;
    liked_by(&mut tx, &f.account, &id).await;

    super::super::sweep(&mut tx, f.objects.as_ref())
        .await
        .expect("清理应当成功");

    assert_eq!(f.objects.get(&key), None);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stored_tracks WHERE track_id = $1",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

/// 对象删不掉时那一行留着,下一轮再来 —— 先删行的话桶里就多一个没人记得的孤儿。
#[tokio::test]
async fn the_sweep_keeps_the_row_when_the_object_cannot_be_deleted()
 {
    let f = fixture("ar_orphan", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let id = testing::track_id("ar_orphan", 1);
    stored_days_ago(&mut tx, &f.objects, &id, 4).await;

    super::super::sweep(&mut tx, &Unreachable)
        .await
        .expect("删不掉对象不算清理失败");

    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stored_tracks WHERE track_id = $1",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(left, 1);
}

/// 一个只会回 `status` 的 S3 端点,前面接真的 S3 客户端:删对象走的是生产上那条签名 + HTTP 路。
async fn s3_answering(
    status: axum::http::StatusCode,
) -> server::objects::S3 {
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    let app = axum::Router::new()
        .fallback(move || async move { status });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    server::objects::S3::new(server::objects::S3Config {
        endpoint: format!("http://{addr}"),
        public_endpoint: format!("http://{addr}"),
        bucket: "tracks".to_owned(),
        region: "us-east-1".to_owned(),
        access_key_id: "test".to_owned(),
        secret_access_key: "test".to_owned(),
    })
    .expect("S3 配置应当合法")
}

/// 这首还剩几行账。
async fn rows_of(
    tx: &mut sqlx::PgConnection,
    id: &str,
) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM stored_tracks WHERE track_id = $1",
    )
    .bind(id)
    .fetch_one(tx)
    .await
    .unwrap()
}

/// 账上有、桶里早没了(RustFS 删不存在的键回 404):清扫当它删掉了,那一行跟着删,
/// 不会每小时重试一遍、永远留着(#147 R-1)。
#[tokio::test]
async fn the_sweep_forgets_a_row_whose_object_is_already_gone()
 {
    let f = fixture("ar_gone", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let old = testing::track_id("ar_gone", 1);
    let legacy = testing::track_id("ar_gone", 2);
    stored_days_ago(&mut tx, &f.objects, &old, 4).await;
    stored_at(&mut tx, &f.objects, &legacy, 0, "high")
        .await;
    let s3 =
        s3_answering(axum::http::StatusCode::NOT_FOUND)
            .await;

    super::super::sweep(&mut tx, &s3)
        .await
        .expect("清理应当成功");

    assert_eq!(rows_of(&mut tx, &old).await, 0);
    assert_eq!(rows_of(&mut tx, &legacy).await, 0);
}

/// 真的失败(5xx):那一行留着,下一轮再来。
#[tokio::test]
async fn the_sweep_keeps_the_row_when_the_store_answers_5xx()
 {
    let f = fixture("ar_5xx", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let legacy = testing::track_id("ar_5xx", 1);
    stored_at(&mut tx, &f.objects, &legacy, 0, "high")
        .await;
    let s3 = s3_answering(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;

    super::super::sweep(&mut tx, &s3)
        .await
        .expect("删不掉对象不算清理失败");

    assert_eq!(rows_of(&mut tx, &legacy).await, 1);
}

/// 取消红心从那一刻重新数三天,而不是按很久以前那次播放当场就删。
#[tokio::test]
async fn unliking_restarts_the_clock() {
    let f = fixture("ar_unlike", false).await;
    let id = testing::track_id("ar_unlike", 1);
    let mut conn = f.state.pool.acquire().await.unwrap();
    stored_days_ago(&mut conn, &f.objects, &id, 4).await;
    drop(conn);

    crate::routes::library::likes::unlike_track(
        State(f.state.clone()),
        f.account.clone(),
        axum::extract::Path(id.clone()),
    )
    .await
    .expect("取消红心应当成功");

    let mut conn = f.state.pool.acquire().await.unwrap();
    let expired = ledger::expired(
        &mut conn,
        super::super::RETAIN,
        &quality(),
    )
    .await
    .unwrap();
    assert!(
        expired.iter().all(|track| track.track_id != id),
        "刚取消红心的歌不该算过期"
    );
}

/// 放得下就不让;放不下按顺序请前几个让位;全让掉也放不下就不存。
#[test]
fn make_room_takes_as_few_as_it_needs() {
    use super::super::make_room;

    assert_eq!(make_room(50, 40, 100, &[30]), Some(0));
    assert_eq!(
        make_room(90, 40, 100, &[20, 20, 20]),
        Some(2)
    );
    assert_eq!(make_room(90, 40, 100, &[30, 5]), Some(1));
    assert_eq!(make_room(90, 40, 100, &[10, 10]), None);
    assert_eq!(make_room(90, 40, 100, &[]), None);
}

/// 名次:「我的喜欢」0、当天日推 1、其他歌单 2、哪都不在 3;换了日推,旧的那批掉到 3。
#[tokio::test]
async fn ranks_follow_liked_then_daily_then_playlists() {
    let f = fixture("ar_rank", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let id = |n| testing::track_id("ar_rank", n);
    liked_by(&mut tx, &f.account, &id(1)).await;
    in_playlist(&mut tx, &f.account, &id(3)).await;
    server::store::daily::replace(
        &mut tx,
        f.account.id,
        &[netease(&id(1)), netease(&id(2))],
    )
    .await
    .unwrap();

    let mut ranks = Vec::new();
    for n in 1..=4 {
        ranks.push(
            ledger::rank(&mut tx, "netease", &id(n))
                .await
                .unwrap(),
        );
    }
    assert_eq!(ranks, vec![0, 1, 2, ledger::UNKEPT]);

    server::store::daily::replace(
        &mut tx,
        f.account.id,
        &[netease(&id(4))],
    )
    .await
    .unwrap();
    assert_eq!(
        ledger::rank(&mut tx, "netease", &id(2))
            .await
            .unwrap(),
        ledger::UNKEPT
    );
}

/// 为名次靠前的让位时,只请名次在它之后的;越靠后越先,同名次里最久没播的先。
#[tokio::test]
async fn the_ones_behind_yield_last_played_first() {
    let f = fixture("ar_yield", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let id = |n| testing::track_id("ar_yield", n);
    stored_days_ago(&mut tx, &f.objects, &id(1), 1).await;
    liked_by(&mut tx, &f.account, &id(1)).await;
    stored_days_ago(&mut tx, &f.objects, &id(2), 1).await;
    in_playlist(&mut tx, &f.account, &id(2)).await;
    stored_days_ago(&mut tx, &f.objects, &id(3), 1).await;
    stored_days_ago(&mut tx, &f.objects, &id(4), 2).await;

    let ours = async |tx: &mut sqlx::PgConnection, rank| {
        ledger::yielding_to(tx, rank)
            .await
            .unwrap()
            .into_iter()
            .map(|track| track.track_id)
            .filter(|track_id| {
                track_id.starts_with(&testing::scoped(
                    "ar_yield",
                ))
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(
        ours(&mut tx, 0).await,
        vec![id(4), id(3), id(2)]
    );
    assert_eq!(ours(&mut tx, 2).await, vec![id(4), id(3)]);
}

/// 统计入口带着配置的空间上限。
#[tokio::test]
async fn the_stats_carry_the_cap() {
    let f = fixture("ar_stats", false).await;
    let state = AppState {
        archive: Some(
            Archive::new(f.objects.clone()).with_cap(123),
        ),
        ..f.state.clone()
    };

    let stats = super::super::stats(
        State(state),
        f.account.clone(),
    )
    .await
    .expect("统计应当取得到")
    .0;

    assert_eq!(stats.cap_bytes, 123);
}
