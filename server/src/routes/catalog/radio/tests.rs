//! 电台:听过的丢掉、不够再拉、拉满上限就停,心动模式挂在红心歌单上(#159)。

use axum::extract::{Query, State};
use similar_asserts::assert_eq;

use server::store::history;
use server::store::playlist::TrackRef;

use crate::routes::testing::{
    self, FakeUpstream, liked_playlist, track_id,
    upstream_track,
};

use super::{
    ENOUGH, MAX_PULLS, RadioMode, RadioQuery, radio,
};

fn fm() -> Query<RadioQuery> {
    Query(RadioQuery {
        mode: RadioMode::Fm,
        seed: None,
    })
}

/// 这个账号听过 `ids` 里的每一首。
async fn played(
    pool: &sqlx::PgPool,
    account_id: i64,
    ids: &[String],
) {
    let mut conn = pool.acquire().await.expect("取连接");
    for id in ids {
        history::record(
            &mut conn,
            account_id,
            &TrackRef {
                platform: "netease".to_owned(),
                track_id: id.clone(),
            },
        )
        .await
        .expect("记播放");
    }
}

fn ids(response: &contract::TracksDto) -> Vec<String> {
    response
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect()
}

/// 第一批里听过的丢掉,不够 [`ENOUGH`] 就再拉一批;批内批间重复的只留一首。
#[tokio::test]
async fn fm_drops_played_tracks_and_pulls_again() {
    let case = "radio_fm_refill";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let id = |n| track_id(case, n);
    played(&pool, account.id, &[id(1), id(2)]).await;

    let fake = FakeUpstream::default();
    fake.fm_batches.lock().unwrap().extend([
        vec![
            upstream_track(&id(1), "听过"),
            upstream_track(&id(2), "也听过"),
            upstream_track(&id(3), "新"),
        ],
        vec![
            upstream_track(&id(3), "又来"),
            upstream_track(&id(4), "新"),
            upstream_track(&id(4), "批内重复"),
            upstream_track(&id(5), "新"),
        ],
    ]);
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let response = radio(State(state), account, fm())
        .await
        .expect("电台应该成功");

    assert_eq!(ids(&response.0), vec![id(3), id(4), id(5)]);
    assert_eq!(fake.fm_pulls(), 2);
}

/// 全是听过的,问满 [`MAX_PULLS`] 回就停,交出空批而不是一直问下去。
#[tokio::test]
async fn fm_stops_at_the_pull_limit() {
    let case = "radio_fm_limit";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let old = track_id(case, 1);
    played(&pool, account.id, std::slice::from_ref(&old))
        .await;

    let fake = FakeUpstream::default();
    fake.fm_batches.lock().unwrap().extend(
        (0..MAX_PULLS + 3)
            .map(|_| vec![upstream_track(&old, "听过")]),
    );
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let response = radio(State(state), account, fm())
        .await
        .expect("电台应该成功");

    assert!(response.0.tracks.is_empty());
    assert_eq!(fake.fm_pulls(), MAX_PULLS);
}

/// 一批就够了不再问;交出的详情写进缓存,与日推同一个 upsert。
#[tokio::test]
async fn fm_stops_once_enough_and_caches_details() {
    let case = "radio_fm_enough";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let batch: Vec<_> = (1..=ENOUGH)
        .map(|n| upstream_track(&track_id(case, n), "新"))
        .collect();

    let fake = FakeUpstream::default();
    fake.fm_batches
        .lock()
        .unwrap()
        .extend([batch.clone(), batch]);
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let response = radio(State(state), account, fm())
        .await
        .expect("电台应该成功");

    assert_eq!(fake.fm_pulls(), 1);
    let mut conn = pool.acquire().await.expect("取连接");
    let cached = server::store::cache::details_of(
        &mut conn,
        "netease",
        &ids(&response.0),
    )
    .await
    .expect("读详情");
    assert_eq!(cached, response.0.tracks);
}

/// 心动模式把种子与红心歌单 id 交给上游 —— 空歌单 id 上游报 400。
#[tokio::test]
async fn heart_seeds_from_the_liked_playlist() {
    let case = "radio_heart";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let seed = track_id(case, 1);
    played(&pool, account.id, std::slice::from_ref(&seed))
        .await;

    let mut fake =
        FakeUpstream::logged_in_with("nuid", Vec::new());
    fake.playlists = vec![liked_playlist("liked-9")];
    fake.listed = (1..=4)
        .map(|n| upstream_track(&track_id(case, n), "心动"))
        .collect();
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let response = radio(
        State(state),
        account,
        Query(RadioQuery {
            mode: RadioMode::Heart,
            seed: Some(seed.clone()),
        }),
    )
    .await
    .expect("心动应该成功");

    assert_eq!(
        ids(&response.0),
        (2..=4)
            .map(|n| track_id(case, n))
            .collect::<Vec<_>>()
    );
    let asks = fake.heart_asks();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].seed_track_id, seed);
    assert_eq!(asks[0].playlist_id, "liked-9");
}

/// 心动模式没给种子是调用方的错,不去问上游。
#[tokio::test]
async fn heart_without_a_seed_is_rejected() {
    let case = "radio_heart_no_seed";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake =
        FakeUpstream::logged_in_with("nuid", Vec::new());
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let failure = radio(
        State(state),
        account,
        Query(RadioQuery {
            mode: RadioMode::Heart,
            seed: Some(String::new()),
        }),
    )
    .await
    .expect_err("没种子应该失败");

    assert_eq!(
        failure.0,
        axum::http::StatusCode::BAD_REQUEST
    );
    assert!(fake.heart_asks().is_empty());
}

/// 心动模式把「表过态」的也丢掉(2026-09-28 并进 #161):本地红心、平台红心歌单
/// 缓存里的那份、赞过、踩过、命中屏蔽规则的,一首都不该出来。
#[tokio::test]
async fn heart_drops_liked_judged_and_blocked_tracks() {
    use contract::{BlockKind, TrackDto};
    use server::store::{blocks, cache, feedback, liked};

    let case = "radio_heart_known";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let id = |n| track_id(case, n);
    let at = |n| TrackRef {
        platform: "netease".to_owned(),
        track_id: id(n),
    };
    let seed = id(1);
    played(&pool, account.id, std::slice::from_ref(&seed))
        .await;

    let mut conn = pool.acquire().await.expect("取连接");
    liked::set(&mut conn, account.id, &at(2), true)
        .await
        .expect("点红心");
    feedback::set(&mut conn, account.id, &at(3), 1)
        .await
        .expect("点赞");
    feedback::set(&mut conn, account.id, &at(4), -1)
        .await
        .expect("点踩");
    let in_platform_liked: TrackDto =
        testing::expected_dto(&id(5), "平台红心");
    cache::set_playlist(
        &mut conn,
        account.id,
        "liked-9",
        &[in_platform_liked],
    )
    .await
    .expect("写平台红心缓存");
    blocks::create(
        &mut conn,
        account.id,
        BlockKind::Track,
        &id(6),
        None,
    )
    .await
    .expect("屏蔽一首");
    drop(conn);

    let mut fake =
        FakeUpstream::logged_in_with("nuid", Vec::new());
    fake.playlists = vec![liked_playlist("liked-9")];
    fake.listed = (1..=7)
        .map(|n| upstream_track(&id(n), "心动"))
        .collect();
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );

    let response = radio(
        State(state),
        account,
        Query(RadioQuery {
            mode: RadioMode::Heart,
            seed: Some(seed),
        }),
    )
    .await
    .expect("心动应该成功");

    assert_eq!(ids(&response.0), vec![id(7)]);
}
