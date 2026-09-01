//! `GET /liked` 走完整条链的测试:问账号 → 找红心歌单 → 取详情 → 回填缓存。
//!
//! 三步各自成功、拼起来却对不上,是这条路上真正会出的错;单独测每一步
//! 看不见它。上游由 `routes::testing` 的假 gRPC 服务扮演,库是真的。

use axum::extract::State;
use axum::http::StatusCode;
use similar_asserts::assert_eq;

use crate::routes::testing::{
    self, FakeUpstream, expected_dto, liked_playlist,
    track_id, track_ref, upstream_track,
};

use super::liked;

use server::bangdream::proto::{
    GetPlaylistResponse, Playlist,
};

/// 网易云没登录时报错,不能回空列表。
///
/// 上游用 `logged_in` 而不是错误码回答「没登录」(它的 `docs/adr/0005`),
/// 照搬过来就成了一个空的 `TracksDto` —— 界面把它读成「一首喜欢的都没有」,
/// 于是用户以为自己的红心全没了,而真正该做的是提示去扫码。
#[tokio::test]
async fn liked_refuses_instead_of_reporting_an_empty_library()
 {
    let case = "lk_logged_out";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let state = testing::state(
        pool,
        testing::serve(FakeUpstream::default()).await,
    );

    let (status, body) = liked(State(state), account)
        .await
        .expect_err("没登录网易云却拿到了红心列表");

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "没登录该是一个客户端认得出的状态,不是通用失败"
    );
    assert_eq!(body.code, "netease_not_logged_in");
}

/// 红心列表按**加入时间**倒排,不是平台数组的原序。
///
/// 走红心歌单而不是 `/liked/ids` 正是为了这个:红心接口返回裸数字数组,
/// 结构上挂不住加入时间,顺序稳定却不表示任何东西(见 `docs/adr/0021`)。
/// 退回原序的现象是「今天刚点的心排在第 120 位」。
#[tokio::test]
async fn liked_orders_the_playlist_by_when_each_track_was_added()
 {
    let case = "lk_order";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let older = track_id(case, 1);
    let newer = track_id(case, 2);

    let mut fake =
        FakeUpstream::logged_in_with("42", vec![]);
    fake.playlists = vec![liked_playlist("liked-1")];
    fake.playlist = GetPlaylistResponse {
        playlist: Some(Playlist::default()),
        // refs 故意按「先加的排前面」给,照搬原序的实现会在这里露出来
        track_refs: vec![
            track_ref(&older, 1_000),
            track_ref(&newer, 2_000),
        ],
        tracks: vec![
            upstream_track(&older, "先点的"),
            upstream_track(&newer, "后点的"),
        ],
    };

    let state =
        testing::state(pool, testing::serve(fake).await);

    let tracks = liked(State(state), account)
        .await
        .expect("登着的账号该取得到红心列表")
        .0;

    assert_eq!(
        tracks.unavailable, 0,
        "平台每一首都给了详情,不该有被剔掉的"
    );
    assert_eq!(
        tracks.tracks,
        vec![
            expected_dto(&newer, "后点的"),
            expected_dto(&older, "先点的"),
        ]
    );
}

/// 歌单列表里认不出红心歌单时报错,不是回空列表。
///
/// 每个账号都有这个歌单,认不出只有两种可能:上游给的列表不完整,或者
/// `special_type` 的判据错了。这两种都不该被翻译成「一首喜欢的都没有」——
/// 那是一个没人会去查的假象。
#[tokio::test]
async fn liked_fails_when_no_playlist_carries_the_liked_marker()
 {
    let case = "lk_no_marker";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let mut fake =
        FakeUpstream::logged_in_with("42", vec![]);
    // 一个普通歌单:有,但没有红心标记
    fake.playlists = vec![Playlist {
        id: "ordinary".to_owned(),
        name: "随便一个歌单".to_owned(),
        ..Playlist::default()
    }];

    let state =
        testing::state(pool, testing::serve(fake).await);

    let (status, body) = liked(State(state), account)
        .await
        .expect_err("认不出红心歌单却当成功返回了");

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body.code, "not_found");
}

/// 上游不可达时是 502,不是一个空的红心列表。
#[tokio::test]
async fn liked_maps_an_unreachable_upstream_to_a_gateway_error()
 {
    let case = "lk_down";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );

    let (status, body) = liked(State(state), account)
        .await
        .expect_err("上游连不上却当成功返回了");

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body.code, "upstream_unreachable");
}
