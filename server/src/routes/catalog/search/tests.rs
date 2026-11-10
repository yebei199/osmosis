//! 日推与搜索把列表里的曲目写进 `platform_tracks`(#156)。

use axum::extract::{Query, State};
use similar_asserts::assert_eq;

use server::bangdream::proto::Album;
use server::store::cache;

use crate::routes::testing::{
    self, FakeUpstream, track_id, upstream_track,
};

use server::store::playlist::TrackRef;

use super::{
    SearchQuery, daily, daily_heard, search_tracks,
};

/// 两首歌:一首带专辑,一首没有。
fn listed(
    case: &str,
) -> Vec<server::bangdream::proto::Track> {
    let mut with_album =
        upstream_track(&track_id(case, 1), "有专辑");
    with_album.album = Some(Album {
        id: "88888".to_owned(),
        name: "LiSA BEST".to_owned(),
        ..Album::default()
    });
    vec![
        with_album,
        upstream_track(&track_id(case, 2), "单曲"),
    ]
}

/// 库里这两首的详情,按 id 顺序。
async fn stored(
    pool: &sqlx::PgPool,
    case: &str,
) -> Vec<contract::TrackDto> {
    let mut conn = pool.acquire().await.expect("取连接");
    cache::details_of(
        &mut conn,
        "netease",
        &[track_id(case, 1), track_id(case, 2)],
    )
    .await
    .expect("读详情")
}

#[tokio::test]
async fn search_writes_its_tracks_into_the_cache() {
    let case = "search_details";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake = FakeUpstream {
        listed: listed(case),
        ..FakeUpstream::default()
    };
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );

    let response = search_tracks(
        State(state),
        account,
        Query(SearchQuery {
            q: "紅蓮華".to_owned(),
            limit: None,
            offset: None,
        }),
    )
    .await
    .expect("搜索应该成功");

    assert_eq!(
        stored(&pool, case).await,
        response.0.tracks
    );
    assert_eq!(
        response.0.tracks[0]
            .album
            .as_ref()
            .map(|album| album.name.as_str()),
        Some("LiSA BEST")
    );
}

#[tokio::test]
async fn daily_writes_its_tracks_into_the_cache() {
    let case = "daily_details";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake = FakeUpstream {
        listed: listed(case),
        ..FakeUpstream::default()
    };
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );

    let mut response = daily(State(state), account)
        .await
        .expect("日推应该成功")
        .0;

    // 推荐日是日推出口挂上的,不在缓存的详情里
    for track in &mut response.tracks {
        assert!(track.facets.picked_on.is_some());
        track.facets.picked_on = None;
    }
    assert_eq!(stored(&pool, case).await, response.tracks);
}

/// 过了推荐日,昨天没听的留着,听过的归「已听过」,红心的不再摆(ADR 0037)。
#[tokio::test]
async fn yesterdays_unheard_picks_stay() {
    let case = "daily_keeps";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let id = |n| track_id(case, n);
    let fake = FakeUpstream {
        listed: listed(case),
        ..FakeUpstream::default()
    };
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );
    let _ = daily(State(state.clone()), account.clone())
        .await
        .expect("第一批");
    let mut conn = pool.acquire().await.unwrap();
    let old = |n| TrackRef {
        platform: "netease".to_owned(),
        track_id: id(n),
    };
    cache::put_details(
        &mut conn,
        &[3, 4, 5].map(|n| {
            server::bangdream::track_to_dto(upstream_track(
                &id(n),
                "昨天",
            ))
        }),
    )
    .await
    .unwrap();
    server::store::daily::add(
        &mut conn,
        account.id,
        &[old(3), old(4), old(5)],
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE daily_picks SET day = day - 1
         WHERE account_id = $1 AND track_id = ANY($2)",
    )
    .bind(account.id)
    .bind(vec![id(3), id(4), id(5)])
    .execute(&mut *conn)
    .await
    .unwrap();
    crate::routes::testing::hear(
        &mut conn,
        account.id,
        &old(4),
    )
    .await;
    server::store::liked::set(
        &mut conn,
        account.id,
        &old(5),
        true,
    )
    .await
    .unwrap();

    let shown =
        daily(State(state.clone()), account.clone())
            .await
            .unwrap()
            .0;
    let heard =
        daily_heard(State(state), account).await.unwrap().0;

    let ids = |tracks: &[contract::TrackDto]| {
        tracks
            .iter()
            .map(|track| track.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&shown.tracks),
        vec![id(1), id(2), id(3)]
    );
    assert_eq!(ids(&heard.tracks), vec![id(4)]);
    // 每一首带上它是哪天推的:今天两首在前,昨天那首在后
    let days: Vec<_> = shown
        .tracks
        .iter()
        .map(|track| track.facets.picked_on.clone())
        .collect();
    assert!(days[0].is_some() && days[0] == days[1]);
    assert!(days[2].is_some() && days[2] < days[0]);
}

/// 以前推过、今天又推的歌挪到今天这批,排在今天这批里它该在的位置,
/// 不因为留着旧行号插到今天真正新推的前面。
#[tokio::test]
async fn a_track_recommended_again_takes_its_place_in_todays_batch() {
    let case = "daily_again";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let id = |n| track_id(case, n);
    let at = |n| TrackRef {
        platform: "netease".to_owned(),
        track_id: id(n),
    };
    let mut conn = pool.acquire().await.unwrap();
    server::store::daily::add(&mut conn, account.id, &[at(1), at(2)])
        .await
        .unwrap();
    sqlx::query(
        "UPDATE daily_picks SET day = day - 1 WHERE account_id = $1",
    )
    .bind(account.id)
    .execute(&mut *conn)
    .await
    .unwrap();

    server::store::daily::add(&mut conn, account.id, &[at(3), at(1)])
        .await
        .unwrap();

    let order: Vec<String> = server::store::daily::list(&mut conn, account.id)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.track.track_id)
        .collect();
    assert_eq!(order, vec![id(3), id(1), id(2)]);
}
