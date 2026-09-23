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

use super::{like_track, liked};

use std::time::Duration;

use axum::extract::Path;
use server::store::cache::{self, LIKED_PLAYLIST_ID};

use crate::routes::catalog::catalog_cache::REFRESH_EVERY;

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

/// 红心歌单里摆着这几首,按给出的先后依次加入。
fn liked_upstream(ids: &[(&str, &str)]) -> FakeUpstream {
    let mut fake = FakeUpstream::logged_in_with(
        "42",
        ids.iter()
            .map(|(id, title)| upstream_track(id, title))
            .collect(),
    );
    fake.playlists = vec![liked_playlist("liked-1")];
    fake.playlist = GetPlaylistResponse {
        playlist: Some(Playlist::default()),
        track_refs: ids
            .iter()
            .zip(1..)
            .map(|((id, _), at)| track_ref(id, at * 1_000))
            .collect(),
        tracks: ids
            .iter()
            .map(|(id, title)| upstream_track(id, title))
            .collect(),
    };
    fake
}

/// 库里有一份时,第二次打开不等上游。
///
/// 这是 #124 要的全部:`/liked` 本机约一秒,八成花在上游的 `GetPlaylist` 上。
/// 上游在这里卡死,等了它的实现会卡在超时上,而不是慢一点照样绿。
#[tokio::test]
async fn liked_answers_from_the_store_without_waiting_for_the_upstream()
 {
    let case = "lk_store_first";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let only = track_id(case, 1);

    let fake = liked_upstream(&[(&only, "那一首")]);
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );
    let first =
        liked(State(state.clone()), account.clone())
            .await
            .expect("第一次打开该回源成功")
            .0;

    // 到了该刷新的时候:后台那一次回源会被发出去,并且卡死
    state.playlists.age(
        account.id,
        LIKED_PLAYLIST_ID,
        REFRESH_EVERY,
    );
    let stalled = testing::with_upstream(
        &state,
        testing::serve(FakeUpstream {
            stall_playlist: true,
            ..fake
        })
        .await,
    );
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        liked(State(stalled), account),
    )
    .await
    .expect("第二次打开等了上游")
    .expect("库里有一份,不该失败")
    .0;

    assert_eq!(second, first);
}

/// 后台回源拿到的新成员关系写进库,下一次打开就看得到。
#[tokio::test]
async fn liked_refreshes_the_store_in_the_background() {
    let case = "lk_refresh";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let older = track_id(case, 1);
    let newer = track_id(case, 2);

    let state = testing::state(
        pool.clone(),
        testing::serve(liked_upstream(&[(
            &older,
            "先点的",
        )]))
        .await,
    );
    let _ = liked(State(state.clone()), account.clone())
        .await
        .expect("第一次打开该回源成功");

    state.playlists.age(
        account.id,
        LIKED_PLAYLIST_ID,
        REFRESH_EVERY,
    );
    let changed = testing::with_upstream(
        &state,
        testing::serve(liked_upstream(&[
            (&older, "先点的"),
            (&newer, "后点的"),
        ]))
        .await,
    );
    let stale =
        liked(State(changed.clone()), account.clone())
            .await
            .expect("库里有一份,不该失败")
            .0;
    assert_eq!(
        stale.tracks,
        vec![expected_dto(&older, "先点的")],
        "这一次答的是库里那份"
    );

    let mut conn =
        pool.acquire().await.expect("取不到连接");
    tokio::time::timeout(Duration::from_secs(5), async {
        while cache::tracks_of(
            &mut conn,
            account.id,
            LIKED_PLAYLIST_ID,
        )
        .await
        .expect("读缓存失败")
        .len()
            < 2
        {
            tokio::time::sleep(Duration::from_millis(20))
                .await;
        }
    })
    .await
    .expect("后台回源没把新点的那首写进库");

    let fresh = liked(State(changed), account)
        .await
        .expect("库里有一份,不该失败")
        .0;
    assert_eq!(
        fresh.tracks,
        vec![
            expected_dto(&newer, "后点的"),
            expected_dto(&older, "先点的"),
        ]
    );
}

/// 在这边点过心,下一次打开红心当场回源,不先回库里那份。
///
/// 先回库只该掩盖**平台那边**的变化(手机官方 App 里点的心,晚一次看到)。
/// 用户刚在这里点的心不能晚一次:他点完就去看,看不到就是 bug。
#[tokio::test]
async fn liking_a_track_here_skips_the_stored_copy_next_time()
 {
    let case = "lk_like_invalidates";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let older = track_id(case, 1);
    let newer = track_id(case, 2);

    let state = testing::state(
        pool,
        testing::serve(liked_upstream(&[(
            &older,
            "先点的",
        )]))
        .await,
    );
    let _ = liked(State(state.clone()), account.clone())
        .await
        .expect("第一次打开该回源成功");

    let liked_now = testing::with_upstream(
        &state,
        testing::serve(liked_upstream(&[
            (&older, "先点的"),
            (&newer, "后点的"),
        ]))
        .await,
    );
    like_track(
        State(liked_now.clone()),
        account.clone(),
        Path(newer.clone()),
    )
    .await
    .expect("点心该转发成功");

    let tracks = liked(State(liked_now), account)
        .await
        .expect("该取得到红心列表")
        .0;
    assert_eq!(
        tracks.tracks,
        vec![
            expected_dto(&newer, "后点的"),
            expected_dto(&older, "先点的"),
        ]
    );
}
