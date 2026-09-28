//! 电台:听过的丢掉、不够再拉、拉满上限就停,心动模式挂在红心歌单上(#159)。

use axum::extract::{Query, State};
use similar_asserts::assert_eq;

use server::bangdream::proto::Track;
use server::store::history;
use server::store::playlist::TrackRef;

use crate::routes::testing::{
    self, FakeUpstream, liked_playlist, track_id,
    upstream_track,
};

use super::{
    ENOUGH, MAX_FILTERED_PULLS, MAX_PULLS, RadioMode,
    RadioQuery, radio,
};

fn fm() -> Query<RadioQuery> {
    Query(RadioQuery {
        mode: RadioMode::Fm,
        seed: None,
        filter: None,
    })
}

/// 带筛选的私人 FM。`filter` 是 `[FacetPickDto]` 的 JSON。
fn fm_filtered(filter: &str) -> Query<RadioQuery> {
    Query(RadioQuery {
        mode: RadioMode::Fm,
        seed: None,
        filter: Some(filter.to_owned()),
    })
}

/// 上游给的一首,时长 `ms`。
fn lasting(id: &str, ms: i64) -> Track {
    let mut track = upstream_track(id, "新");
    track.duration_ms = ms;
    track
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
            filter: None,
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
            filter: None,
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
            filter: None,
        }),
    )
    .await
    .expect("心动应该成功");

    assert_eq!(ids(&response.0), vec![id(7)]);
}

/// 私人 FM 也认平台红心歌单(缓存里那份):问一次红心歌单 id,
/// 里面的歌与本地「我的喜欢」一样丢掉(#161)。
#[tokio::test]
async fn fm_drops_tracks_in_the_platform_liked_playlist() {
    use server::store::cache;

    let case = "radio_fm_platform_liked";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let id = |n| track_id(case, n);

    let mut conn = pool.acquire().await.expect("取连接");
    cache::set_playlist(
        &mut conn,
        account.id,
        "liked-9",
        &[testing::expected_dto(&id(1), "平台红心")],
    )
    .await
    .expect("写平台红心缓存");
    drop(conn);

    let mut fake =
        FakeUpstream::logged_in_with("nuid", Vec::new());
    fake.playlists = vec![liked_playlist("liked-9")];
    fake.fm_batches.lock().unwrap().extend([vec![
        upstream_track(&id(1), "红心过"),
        upstream_track(&id(2), "新"),
        upstream_track(&id(3), "新"),
        upstream_track(&id(4), "新"),
    ]]);
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );

    let response = radio(State(state), account, fm())
        .await
        .expect("电台应该成功");

    assert_eq!(ids(&response.0), vec![id(2), id(3), id(4)]);
}

/// 带筛选:排除之后再按筛选过一道,不够就接着问(#166)。
#[tokio::test]
async fn fm_keeps_only_tracks_passing_the_filter() {
    let case = "radio_fm_filter";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let id = |n| track_id(case, n);

    let fake = FakeUpstream::default();
    fake.fm_batches.lock().unwrap().extend([
        vec![lasting(&id(1), 100_000), lasting(&id(2), 400_000)],
        vec![lasting(&id(3), 400_000), lasting(&id(4), 170_000)],
        vec![lasting(&id(5), 60_000)],
    ]);
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let response = radio(
        State(state),
        account,
        fm_filtered(
            r#"[{"facet":"duration","label":"3 分钟以内"}]"#,
        ),
    )
    .await
    .expect("电台应该成功");

    assert_eq!(ids(&response.0), vec![id(1), id(4), id(5)]);
    assert_eq!(fake.fm_pulls(), 3);
}

/// 筛选窄到一首不剩:问满带筛选的上限就停,交空批。
#[tokio::test]
async fn a_too_narrow_filter_stops_at_its_own_limit() {
    let case = "radio_fm_filter_limit";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let fake = FakeUpstream::default();
    fake.fm_batches.lock().unwrap().extend(
        (0..MAX_FILTERED_PULLS + 3).map(|n| {
            vec![lasting(&track_id(case, n), 400_000)]
        }),
    );
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let response = radio(
        State(state),
        account,
        fm_filtered(
            r#"[{"facet":"duration","label":"3 分钟以内"}]"#,
        ),
    )
    .await
    .expect("电台应该成功");

    assert!(response.0.tracks.is_empty());
    assert_eq!(fake.fm_pulls(), MAX_FILTERED_PULLS);
}

/// 读不懂的筛选是调用方的错,不去问上游。
#[tokio::test]
async fn a_garbled_filter_is_rejected() {
    let case = "radio_fm_filter_garbled";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake = FakeUpstream::default();
    let state = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );

    let failure =
        radio(State(state), account, fm_filtered("[{"))
            .await
            .expect_err("读不懂的筛选应该失败");

    assert_eq!(
        failure.0,
        axum::http::StatusCode::BAD_REQUEST
    );
    assert_eq!(fake.fm_pulls(), 0);
}
