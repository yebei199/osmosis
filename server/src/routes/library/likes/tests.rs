//! 红心路由的测试(#146):「我的喜欢」归自家库,网易云只在导入时出场。
//!
//! 上游由 `routes::testing` 的假 gRPC 服务扮演,库是真的。读与点心的测试
//! 故意配一个连不上的上游:它们一旦去问网易云就会失败,而不是悄悄照样绿。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use similar_asserts::assert_eq;

use server::bangdream::proto::{
    GetPlaylistResponse, Playlist,
};

use crate::routes::testing::{
    self, FakeUpstream, expected_dto, liked_playlist,
    track_id, track_ref, upstream_track,
};

use super::{
    import_liked, like_track, liked, liked_ids,
    unlike_track,
};

/// 网易云红心歌单里摆着这几首,`ids` 按平台给的次序(最近加的在前)。
/// `with_details` 是平台随歌单详情带回来的那几首。
fn netease_liked(
    ids: &[(&str, i64)],
    with_details: &[(&str, &str)],
) -> FakeUpstream {
    let mut fake =
        FakeUpstream::logged_in_with("42", vec![]);
    fake.playlists = vec![liked_playlist("liked-1")];
    fake.playlist = GetPlaylistResponse {
        playlist: Some(Playlist::default()),
        track_refs: ids
            .iter()
            .map(|(id, at)| track_ref(id, *at))
            .collect(),
        tracks: with_details
            .iter()
            .map(|(id, title)| upstream_track(id, title))
            .collect(),
    };
    fake
}

/// 导入之后,读「我的喜欢」不再问网易云,次序与网易云一致。
#[tokio::test]
async fn liked_reads_our_own_copy_without_netease() {
    let case = "lk_own_read";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let newer = track_id(case, 2);
    let older = track_id(case, 1);

    let state = testing::state(
        pool,
        testing::serve(netease_liked(
            &[(&newer, 2_000), (&older, 1_000)],
            &[(&newer, "后点的"), (&older, "先点的")],
        ))
        .await,
    );
    let imported =
        import_liked(State(state.clone()), account.clone())
            .await
            .expect("导入该成功")
            .0;
    assert_eq!((imported.added, imported.total), (2, 2));

    let offline = testing::with_upstream(
        &state,
        testing::unreachable_upstream(),
    );
    let tracks =
        liked(State(offline.clone()), account.clone())
            .await
            .expect("读自家的「我的喜欢」不该要网易云")
            .0;
    assert_eq!(
        tracks.tracks,
        vec![
            expected_dto(&newer, "后点的"),
            expected_dto(&older, "先点的"),
        ]
    );
    assert_eq!(tracks.unavailable, 0);

    let ids = liked_ids(State(offline), account)
        .await
        .expect("读红心标识不该要网易云")
        .0;
    assert_eq!(ids.track_ids, vec![newer, older]);
}

/// 平台给不出详情的那首照样导进来,只是读的时候算作不可用。
///
/// 「我的喜欢」是我们的,少一首就是丢了一首;详情以后补得回来,成员关系补不回来。
#[tokio::test]
async fn import_keeps_tracks_the_platform_has_no_details_for()
 {
    let case = "lk_no_detail";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let shown = track_id(case, 1);
    let gone = track_id(case, 2);

    let state = testing::state(
        pool,
        testing::serve(netease_liked(
            &[(&gone, 2_000), (&shown, 1_000)],
            &[(&shown, "还在的")],
        ))
        .await,
    );
    let imported =
        import_liked(State(state.clone()), account.clone())
            .await
            .expect("导入该成功")
            .0;
    assert_eq!(imported.total, 2);

    let tracks =
        liked(State(state.clone()), account.clone())
            .await
            .expect("该取得到")
            .0;
    assert_eq!(
        tracks.tracks,
        vec![expected_dto(&shown, "还在的")]
    );
    assert_eq!(tracks.unavailable, 1);

    let ids = liked_ids(State(state), account)
        .await
        .expect("该取得到")
        .0;
    assert_eq!(ids.track_ids, vec![gone, shown]);
}

/// 重跑导入不重复。
#[tokio::test]
async fn importing_twice_adds_nothing_the_second_time() {
    let case = "lk_import_twice";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let only = track_id(case, 1);

    let state = testing::state(
        pool,
        testing::serve(netease_liked(
            &[(&only, 1_000)],
            &[(&only, "那一首")],
        ))
        .await,
    );
    let _ =
        import_liked(State(state.clone()), account.clone())
            .await
            .expect("第一次导入该成功");
    let again = import_liked(State(state), account)
        .await
        .expect("第二次导入该成功")
        .0;

    assert_eq!((again.added, again.total), (0, 1));
}

/// 网易云没登录时导入报「没登录」,不是导进零首当成功。
#[tokio::test]
async fn import_refuses_when_netease_is_not_logged_in() {
    let case = "lk_import_logged_out";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let state = testing::state(
        pool,
        testing::serve(FakeUpstream::default()).await,
    );
    let (status, body) =
        import_liked(State(state), account)
            .await
            .expect_err("没登录网易云却导入成功了");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body.code, "netease_not_logged_in");
}

/// 点心与取消只改自家库,一次都不问网易云。
#[tokio::test]
async fn liking_and_unliking_never_touch_netease() {
    let case = "lk_toggle_local";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let song = track_id(case, 1);

    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );

    let status = like_track(
        State(state.clone()),
        account.clone(),
        Path(song.clone()),
    )
    .await
    .expect("点心不该要网易云");
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        liked_ids(State(state.clone()), account.clone())
            .await
            .expect("读红心标识不该要网易云")
            .0
            .track_ids,
        vec![song.clone()]
    );

    unlike_track(
        State(state.clone()),
        account.clone(),
        Path(song),
    )
    .await
    .expect("取消不该要网易云");
    assert!(
        liked_ids(State(state), account)
            .await
            .expect("读红心标识不该要网易云")
            .0
            .track_ids
            .is_empty()
    );
}

/// 「我的喜欢」还没建过:第一次读它时自动从网易云导入一次(#147),
/// 不必有人先打 `POST /liked/import`。
#[tokio::test]
async fn the_first_read_imports_from_netease() {
    let case = "lk_auto_import";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let newer = track_id(case, 2);
    let older = track_id(case, 1);
    let state = testing::state(
        pool,
        testing::serve(netease_liked(
            &[(&newer, 2_000), (&older, 1_000)],
            &[(&newer, "后点的"), (&older, "先点的")],
        ))
        .await,
    );

    let ids = liked_ids(State(state), account)
        .await
        .expect("该取得到")
        .0;

    assert_eq!(ids.track_ids, vec![newer, older]);
}

/// 导不进来(网易云没登录)时不建一份空的「我的喜欢」,下一次用到再试;
/// 登上之后的下一次读就导进来了。
#[tokio::test]
async fn a_failed_first_import_is_retried_next_time() {
    let case = "lk_auto_retry";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let only = track_id(case, 1);
    let logged_out = testing::state(
        pool.clone(),
        testing::serve(FakeUpstream::default()).await,
    );

    let ids = liked_ids(
        State(logged_out.clone()),
        account.clone(),
    )
    .await
    .expect("导不进来也照样回答")
    .0;
    assert!(ids.track_ids.is_empty());
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        !server::store::liked::exists(
            &mut conn, account.id
        )
        .await
        .unwrap(),
        "导不进来时不该建一份空的"
    );

    let logged_in = testing::with_upstream(
        &logged_out,
        testing::serve(netease_liked(
            &[(&only, 1_000)],
            &[(&only, "那一首")],
        ))
        .await,
    );
    let ids = liked_ids(State(logged_in), account)
        .await
        .expect("该取得到")
        .0;
    assert_eq!(ids.track_ids, vec![only]);
}
