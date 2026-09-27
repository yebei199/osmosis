//! 日推与搜索把列表里的曲目写进 `platform_tracks`(#156)。

use axum::extract::{Query, State};
use similar_asserts::assert_eq;

use server::bangdream::proto::Album;
use server::store::cache;

use crate::routes::testing::{
    self, FakeUpstream, track_id, upstream_track,
};

use super::{SearchQuery, daily, search_tracks};

/// 两首歌:一首带专辑,一首没有。
fn listed(case: &str) -> Vec<server::bangdream::proto::Track> {
    let mut with_album =
        upstream_track(&track_id(case, 1), "有专辑");
    with_album.album = Some(Album {
        id: "88888".to_owned(),
        name: "LiSA BEST".to_owned(),
        ..Album::default()
    });
    vec![with_album, upstream_track(&track_id(case, 2), "单曲")]
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

    assert_eq!(stored(&pool, case).await, response.0.tracks);
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

    let response = daily(State(state), account)
        .await
        .expect("日推应该成功");

    assert_eq!(stored(&pool, case).await, response.0.tracks);
}
