//! 预取队列:幂等入队、并发领取只领走一个、没办成的再入队重新排上、
//! 重试用尽记 failed、启动时把歌单排一遍、已在桶里的不入队。

use std::time::Duration;

use similar_asserts::assert_eq;
use sqlx::PgConnection;

use server::store::playlist::TrackRef;
use server::store::prefetch::{self, Job, Unfinished};

use crate::routes::testing;

const LEASE: Duration = Duration::from_secs(600);

/// 这条测试独有的平台名:领任务按平台领,别的测试排的网易云任务碰不到。
fn platform(case: &str) -> String {
    format!("pf-{}", testing::scoped(case))
}

fn track(platform: &str, id: &str) -> TrackRef {
    TrackRef {
        platform: platform.to_owned(),
        track_id: id.to_owned(),
    }
}

/// 这首在队列里的状态与领过几次;不在队列里是 `None`。
async fn job_state(
    conn: &mut PgConnection,
    track: &TrackRef,
) -> Option<(String, i32)> {
    sqlx::query_as(
        "SELECT state, attempts FROM prefetch_jobs
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(&track.platform)
    .bind(&track.track_id)
    .fetch_optional(conn)
    .await
    .expect("查队列失败")
}

/// 同一首入队两次(一次还在同一批里重复)只有一行,领的账号是先到的那个。
#[tokio::test]
async fn enqueueing_twice_keeps_one_row() {
    let pool = testing::pool().await;
    let first =
        testing::fresh_account(&pool, "pf_twice").await;
    let second =
        testing::fresh_account(&pool, "pf_twice_other")
            .await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_twice");
    let song = track(&p, "1");

    let queued = prefetch::enqueue(
        &mut tx,
        first.id,
        &[song.clone(), song.clone()],
        "lossless",
    )
    .await
    .expect("入队应当成功");
    let again = prefetch::enqueue(
        &mut tx,
        second.id,
        std::slice::from_ref(&song),
        "lossless",
    )
    .await
    .expect("重复入队不该报错");

    assert_eq!((queued, again), (1, 0));
    let owner: i64 = sqlx::query_scalar(
        "SELECT account_id FROM prefetch_jobs WHERE platform = $1",
    )
    .bind(&p)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(owner, first.id);
}

/// 已经按这一档存进桶的不入队。
#[tokio::test]
async fn a_stored_track_is_not_enqueued() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_stored").await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_stored");
    let song = track(&p, "1");
    sqlx::query(
        "INSERT INTO stored_tracks
             (platform, track_id, quality, object_key, format, bit_rate, bytes, tier)
         VALUES ($1, '1', 'lossless', 'k', 'flac', 900000, 1, 'lossless')",
    )
    .bind(&p)
    .execute(&mut *tx)
    .await
    .unwrap();

    let queued = prefetch::enqueue(
        &mut tx,
        account.id,
        std::slice::from_ref(&song),
        "lossless",
    )
    .await
    .unwrap();

    assert_eq!(queued, 0);
    assert_eq!(job_state(&mut tx, &song).await, None);
}

/// 两个 worker 同时来领一个任务:只有一个领到;领走的在租约内不会再被领。
#[tokio::test]
async fn concurrent_claims_take_a_job_once() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_claim").await;
    let p = platform("pf_claim");
    let mut conn = pool.acquire().await.unwrap();
    prefetch::enqueue(
        &mut conn,
        account.id,
        &[track(&p, "1")],
        "lossless",
    )
    .await
    .unwrap();

    let mut a = pool.acquire().await.unwrap();
    let mut b = pool.acquire().await.unwrap();
    let (x, y) = tokio::join!(
        prefetch::claim(&mut a, &p, LEASE),
        prefetch::claim(&mut b, &p, LEASE),
    );
    let claimed: Vec<Job> = [x.unwrap(), y.unwrap()]
        .into_iter()
        .flatten()
        .collect();

    assert_eq!(
        claimed,
        vec![Job {
            platform: p.clone(),
            track_id: "1".to_owned(),
            account_id: account.id,
            attempts: 1,
        }]
    );
    assert_eq!(
        prefetch::claim(&mut conn, &p, LEASE)
            .await
            .unwrap(),
        None,
        "租约内不该再被领走"
    );
}

/// 超出上限没存的,下一次入队重新排上、次数清零;正在排队的再入队不动它。
#[tokio::test]
async fn an_unfinished_job_is_requeued_on_the_next_enqueue()
{
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_requeue").await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_requeue");
    let song = track(&p, "1");
    let enqueue = async |tx: &mut PgConnection| {
        prefetch::enqueue(
            tx,
            account.id,
            std::slice::from_ref(&song),
            "lossless",
        )
        .await
        .unwrap()
    };

    enqueue(&mut tx).await;
    let job = prefetch::claim(&mut tx, &p, LEASE)
        .await
        .unwrap()
        .expect("应当领得到");
    assert_eq!(enqueue(&mut tx).await, 0, "排着的不动");
    assert_eq!(
        job_state(&mut tx, &song).await,
        Some(("queued".to_owned(), 1))
    );

    prefetch::settle(&mut tx, &job, Unfinished::OverCap)
        .await
        .unwrap();
    assert_eq!(
        job_state(&mut tx, &song).await,
        Some(("over_cap".to_owned(), 1))
    );

    assert_eq!(enqueue(&mut tx).await, 1);
    assert_eq!(
        job_state(&mut tx, &song).await,
        Some(("queued".to_owned(), 0))
    );
}

/// 失败退避;领够次数还失败就记 failed。
#[tokio::test]
async fn retries_run_out_into_failed() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_retry").await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_retry");
    let song = track(&p, "1");
    prefetch::enqueue(
        &mut tx,
        account.id,
        std::slice::from_ref(&song),
        "lossless",
    )
    .await
    .unwrap();

    let job = prefetch::claim(&mut tx, &p, LEASE)
        .await
        .unwrap()
        .unwrap();
    prefetch::retry_later(&mut tx, &job, LEASE, 2)
        .await
        .unwrap();
    assert_eq!(
        job_state(&mut tx, &song).await,
        Some(("queued".to_owned(), 1))
    );
    assert_eq!(
        prefetch::claim(&mut tx, &p, LEASE).await.unwrap(),
        None,
        "退避期内不该再领"
    );

    sqlx::query(
        "UPDATE prefetch_jobs SET run_after = now() WHERE platform = $1",
    )
    .bind(&p)
    .execute(&mut *tx)
    .await
    .unwrap();
    let job = prefetch::claim(&mut tx, &p, LEASE)
        .await
        .unwrap()
        .unwrap();
    prefetch::retry_later(&mut tx, &job, LEASE, 2)
        .await
        .unwrap();
    assert_eq!(
        job_state(&mut tx, &song).await,
        Some(("failed".to_owned(), 2))
    );
}

/// 启动时把所有歌单的曲目排上,以歌单主人的凭据去取。
#[tokio::test]
async fn every_playlist_track_is_enqueued_at_startup() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_seed").await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_seed");
    let song = track(&p, "1");
    let list = server::store::playlist::create(
        &mut tx, account.id, "预取",
    )
    .await
    .expect("建歌单失败");
    server::store::playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        std::slice::from_ref(&song),
    )
    .await
    .unwrap();

    prefetch::enqueue_all_playlists(&mut tx, "lossless")
        .await
        .expect("排歌单应当成功");

    let job = prefetch::claim(&mut tx, &p, LEASE)
        .await
        .unwrap()
        .expect("歌单里的歌应当排上了");
    assert_eq!(job.account_id, account.id);
}
