//! 预取队列:幂等入队、并发领取只领走一个、没办成的再入队重新排上、
//! 重试用尽记 failed、启动时只排红心、往歌单加歌不排(#186)、已在桶里的不入队。

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

/// 桶里只有非无损那份的(音源当时给不出无损)照样入队:
/// 之后给得出无损时好换掉它(#152)。
#[tokio::test]
async fn a_track_stored_below_lossless_is_enqueued_again() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_lossy").await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_lossy");
    let song = track(&p, "1");
    sqlx::query(
        "INSERT INTO stored_tracks
             (platform, track_id, quality, object_key, format, bit_rate, bytes, tier)
         VALUES ($1, '1', 'lossless', 'k', 'mp3', 320000, 1, 'high')",
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

    assert_eq!(queued, 1);
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

/// 启动时只排「我的喜欢」,以红心主人的凭据去取;普通歌单里的不排(#186)。
#[tokio::test]
async fn only_liked_tracks_are_enqueued_at_startup() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_seed").await;
    let mut tx = pool.begin().await.unwrap();
    let p = platform("pf_seed");
    let liked = track(&p, "1");
    let listed = track(&p, "2");
    server::store::liked::set(
        &mut tx, account.id, &liked, true,
    )
    .await
    .expect("点红心失败");
    let list = server::store::playlist::create(
        &mut tx, account.id, "预取",
    )
    .await
    .expect("建歌单失败");
    server::store::playlist::add_tracks(
        &mut tx,
        account.id,
        list.id,
        std::slice::from_ref(&listed),
    )
    .await
    .unwrap();

    prefetch::enqueue_all_liked(&mut tx, "lossless")
        .await
        .expect("排红心应当成功");

    let job = prefetch::claim(&mut tx, &p, LEASE)
        .await
        .unwrap()
        .expect("红心的歌应当排上了");
    assert_eq!(
        (job.account_id, job.track_id.as_str()),
        (account.id, "1")
    );
    assert_eq!(job_state(&mut tx, &listed).await, None);
}

/// 往普通歌单加歌不排预取(#186):不是红心的存进去一天没播就会被清掉。
#[tokio::test]
async fn adding_to_a_playlist_queues_nothing() {
    use axum::Json;
    use axum::extract::{Path, State};

    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pf_add").await;
    let state = crate::AppState {
        archive: Some(
            crate::routes::play::archive::Archive::new(
                std::sync::Arc::new(
                    testing::MemoryObjects::default(),
                ),
            ),
        ),
        ..testing::state(
            pool.clone(),
            testing::unreachable_upstream(),
        )
    };
    let list = {
        let mut conn = pool.acquire().await.unwrap();
        server::store::playlist::create(
            &mut conn, account.id, "加歌",
        )
        .await
        .expect("建歌单失败")
    };
    let id = testing::track_id("pf_add", 1);
    let body = serde_json::from_value(serde_json::json!({
        "tracks": [{"platform": "netease", "id": id}]
    }))
    .expect("请求体");

    crate::routes::library::playlists::add_playlist_tracks(
        State(state),
        account,
        Path(list.id),
        Json(body),
    )
    .await
    .expect("加歌应当成功");

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        job_state(&mut conn, &track("netease", &id)).await,
        None
    );
}

/// 这条测试专用的一个 worker 限速器:额度给足,限速不是这里要测的。
fn roomy_limiter() -> governor::DefaultDirectRateLimiter {
    governor::RateLimiter::direct(
        governor::Quota::per_minute(
            std::num::NonZeroU32::new(60).expect("非零"),
        ),
    )
}

/// 停止信号先到了:worker 立刻返回,排着的任务一次也没被领。
///
/// 滚动部署时旧 pod 在宽限期里接着领,会按旧规则办掉新 pod 刚排上的任务
/// (#152:31 首因此被旧代码判成 no_lossless)。
#[tokio::test]
async fn a_stopped_worker_claims_nothing() {
    let pool = testing::pool().await;
    let owner =
        testing::fresh_account(&pool, "pf_stopped").await;
    let p = platform("pf_stopped");
    let song = track(&p, "1");
    let mut conn = pool.acquire().await.unwrap();
    prefetch::enqueue(
        &mut conn,
        owner.id,
        std::slice::from_ref(&song),
        "lossless",
    )
    .await
    .expect("入队应当成功");
    let state = testing::state(
        pool.clone(),
        testing::unreachable_upstream(),
    );
    let stop = tokio_util::sync::CancellationToken::new();
    stop.cancel();

    let finished = tokio::time::timeout(
        Duration::from_secs(2),
        super::work(&state, &p, &roomy_limiter(), &stop),
    )
    .await;

    assert!(finished.is_ok(), "停了的 worker 该立刻返回");
    assert_eq!(
        job_state(&mut conn, &song).await,
        Some(("queued".to_owned(), 0))
    );
}

/// 歇着等新任务的 worker 收到停止信号就醒来退出,不把进程拖过宽限期。
#[tokio::test]
async fn an_idle_worker_stops_at_once() {
    let pool = testing::pool().await;
    let p = platform("pf_idle_stop");
    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );
    let stop = tokio_util::sync::CancellationToken::new();
    let worker = {
        let stop = stop.clone();
        tokio::spawn(async move {
            super::work(
                &state,
                &p,
                &roomy_limiter(),
                &stop,
            )
            .await;
        })
    };
    // 领一次扑空,进入歇着的那段
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.cancel();

    let finished = tokio::time::timeout(
        Duration::from_secs(2),
        worker,
    )
    .await;

    assert!(
        finished.is_ok(),
        "歇着的 worker 该被停止信号叫醒"
    );
}
