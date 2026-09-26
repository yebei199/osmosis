//! 歌单路由的测试。
//!
//! `GET /playlists` 只列我们自己的歌单(#146):假上游照样摆着几个平台歌单,
//! 响应里一个都不该有;列表本身不再依赖上游,配一个连不上的上游也照常回来。
//! 后半是平台歌单曲目的「先回库、后台回源」(#124)。

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use contract::{PlaylistSource, TracksDto};
use similar_asserts::assert_eq;

use server::bangdream::proto::{
    GetPlaylistResponse, Playlist,
};
use server::error::Failure;
use server::store::account::Account;
use server::store::{cache, playlist};

use crate::AppState;
use crate::routes::catalog::catalog_cache::{
    FRESH_WAIT, MAX_AGE, REFRESH_EVERY,
};
use crate::routes::library::likes::like_track;
use crate::routes::testing::{
    self, FakeUpstream, expected_dto, track_id, track_ref,
    upstream_track,
};

use super::{platform_playlist_tracks, playlists};

/// 上游有平台歌单,列表里也只有「我的喜欢」与本地歌单。
#[tokio::test]
async fn playlists_list_only_our_own() {
    let case = "pl_own_only";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let mut conn = pool.acquire().await.unwrap();
    let local =
        playlist::create(&mut conn, account.id, "睡前")
            .await
            .unwrap();
    drop(conn);

    let mut fake =
        FakeUpstream::logged_in_with("42", vec![]);
    fake.playlists = vec![Playlist {
        id: "netease-1".to_owned(),
        name: "网易云的".to_owned(),
        ..Playlist::default()
    }];
    let state =
        testing::state(pool, testing::serve(fake).await);

    let lists = playlists(State(state), account)
        .await
        .expect("该列得出来")
        .0
        .playlists;

    let shape: Vec<_> = lists
        .iter()
        .map(|list| (list.source, list.name.as_str()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (PlaylistSource::Liked, "我的喜欢"),
            (PlaylistSource::Local, "睡前"),
        ]
    );
    assert_eq!(lists[1].id, local.id.to_string());
}

/// 「我的喜欢」的数目来自自家库,上游连不上也照常列出。
#[tokio::test]
async fn liked_count_comes_from_our_own_liked() {
    let case = "pl_liked_count";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );
    for n in 1..=2 {
        like_track(
            State(state.clone()),
            account.clone(),
            Path(testing::track_id(case, n)),
        )
        .await
        .expect("点心不该要网易云");
    }

    let lists = playlists(State(state), account)
        .await
        .expect("列歌单不该要网易云")
        .0
        .playlists;

    assert_eq!(lists[0].source, PlaylistSource::Liked);
    assert_eq!(lists[0].track_count, 2);
}

// ---- 平台歌单(搜出来点进去的那种)的「先回库、后台回源」(#124) ----
//
// 这几条原先挂在 `/liked` 上。#146 把红心收归自家库之后,走 `store_first` 的只剩
// `/playlists/platform/{id}/tracks`,于是原样搬到这里,断言一条没改。

/// 上游「卡死」:比任何一条测试的超时都长得多。
const STALLED: Duration = Duration::from_secs(3600);

/// 测试里那个平台歌单的 id。假上游不看 id,谁问都给同一份。
const PID: &str = "netease-list";

/// 平台歌单里摆着这几首,按给出的先后依次加入。
fn platform_upstream(ids: &[(&str, &str)]) -> FakeUpstream {
    let mut fake = FakeUpstream::logged_in_with(
        "42",
        ids.iter()
            .map(|(id, title)| upstream_track(id, title))
            .collect(),
    );
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

async fn open(
    state: AppState,
    account: Account,
) -> Result<TracksDto, Failure> {
    platform_playlist_tracks(
        State(state),
        account,
        Path(PID.to_owned()),
    )
    .await
    .map(|json| json.0)
}

/// 平台歌单按**加入时间**倒排,不是平台数组的原序(`docs/adr/0021`)。
#[tokio::test]
async fn a_platform_playlist_is_ordered_by_when_each_track_was_added()
 {
    let case = "pp_order";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let older = track_id(case, 1);
    let newer = track_id(case, 2);

    // 故意按「先加的排前面」给,照搬原序的实现会在这里露出来
    let state = testing::state(
        pool,
        testing::serve(platform_upstream(&[
            (&older, "先加的"),
            (&newer, "后加的"),
        ]))
        .await,
    );
    let tracks = open(state, account)
        .await
        .expect("该取得到平台歌单");

    assert_eq!(tracks.unavailable, 0);
    assert_eq!(
        tracks.tracks,
        vec![
            expected_dto(&newer, "后加的"),
            expected_dto(&older, "先加的"),
        ]
    );
}

/// 上游不可达时是 502,不是一个空歌单。
#[tokio::test]
async fn a_platform_playlist_maps_an_unreachable_upstream_to_a_gateway_error()
 {
    let case = "pp_down";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );
    let (status, body) = open(state, account)
        .await
        .expect_err("上游连不上却当成功返回了");

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body.code, "upstream_unreachable");
}

/// 库里有一份时,第二次打开不等上游。上游在这里卡死,等了它的实现会卡在超时上。
#[tokio::test]
async fn a_platform_playlist_answers_from_the_store_without_waiting()
 {
    let case = "pp_store_first";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let only = track_id(case, 1);

    let fake = platform_upstream(&[(&only, "那一首")]);
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );
    let first = open(state.clone(), account.clone())
        .await
        .expect("第一次打开该回源成功");

    // 到了该刷新的时候:后台那一次回源会被发出去,并且卡死
    state.playlists.age(account.id, PID, REFRESH_EVERY);
    let stalled = testing::with_upstream(
        &state,
        testing::serve(FakeUpstream {
            playlist_delay: STALLED,
            ..fake
        })
        .await,
    );
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        open(stalled, account),
    )
    .await
    .expect("第二次打开等了上游")
    .expect("库里有一份,不该失败");

    assert_eq!(second, first);
}

/// 后台回源拿到的新成员关系写进库,下一次打开就看得到。
#[tokio::test]
async fn a_platform_playlist_refreshes_the_store_in_the_background()
 {
    let case = "pp_refresh";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let older = track_id(case, 1);
    let newer = track_id(case, 2);

    let state = testing::state(
        pool.clone(),
        testing::serve(platform_upstream(&[(
            &older,
            "先加的",
        )]))
        .await,
    );
    let _ = open(state.clone(), account.clone())
        .await
        .expect("第一次打开该回源成功");

    state.playlists.age(account.id, PID, REFRESH_EVERY);
    let changed = testing::with_upstream(
        &state,
        testing::serve(platform_upstream(&[
            (&older, "先加的"),
            (&newer, "后加的"),
        ]))
        .await,
    );
    let stale = open(changed.clone(), account.clone())
        .await
        .expect("库里有一份,不该失败");
    assert_eq!(
        stale.tracks,
        vec![expected_dto(&older, "先加的")],
        "这一次答的是库里那份"
    );

    until_stored(&pool, account.id, 2).await;
    let fresh = open(changed, account)
        .await
        .expect("库里有一份,不该失败");
    assert_eq!(
        fresh.tracks,
        vec![
            expected_dto(&newer, "后加的"),
            expected_dto(&older, "先加的"),
        ]
    );
}

/// 等库里这个平台歌单有 `n` 首,最多等 5 秒。
async fn until_stored(
    pool: &sqlx::PgPool,
    account_id: i64,
    n: usize,
) {
    let mut conn =
        pool.acquire().await.expect("取不到连接");
    tokio::time::timeout(Duration::from_secs(5), async {
        while cache::tracks_of(&mut conn, account_id, PID)
            .await
            .expect("读缓存失败")
            .len()
            < n
        {
            tokio::time::sleep(Duration::from_millis(20))
                .await;
        }
    })
    .await
    .expect("回源没把结果写进库");
}

/// 服务端重启过(「多新」的记录全忘了),库里有一份就先回它,不等上游。
///
/// 生产上一次回源 13 秒多,客户端 10 秒就放弃。部署一次之后第一次打开要是
/// 当场回源,那一次必然超时。
#[tokio::test]
async fn after_a_restart_a_platform_playlist_still_answers_from_the_store()
 {
    let case = "pp_after_restart";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let only = track_id(case, 1);

    let fake = platform_upstream(&[(&only, "那一首")]);
    let before = testing::state(
        pool.clone(),
        testing::serve(fake.clone()).await,
    );
    let first = open(before, account.clone())
        .await
        .expect("第一次打开该回源成功");

    // 新的 state 就是新的进程:库还在,内存里的记录没了
    let restarted = testing::state(
        pool,
        testing::serve(FakeUpstream {
            playlist_delay: STALLED,
            ..fake
        })
        .await,
    );
    let again = tokio::time::timeout(
        FRESH_WAIT + Duration::from_secs(2),
        open(restarted, account),
    )
    .await
    .expect("重启后第一次打开等了上游")
    .expect("库里有一份,不该失败");

    assert_eq!(again.tracks, first.tracks);
}

/// 第一次打开(库里还没有)时客户端等不及断开,回源照样跑完、写进库。
#[tokio::test]
async fn a_first_platform_fetch_finishes_even_when_the_client_gives_up()
 {
    let case = "pp_client_gave_up";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let only = track_id(case, 1);

    let state = testing::state(
        pool.clone(),
        testing::serve(FakeUpstream {
            playlist_delay: Duration::from_millis(300),
            ..platform_upstream(&[(&only, "那一首")])
        })
        .await,
    );
    tokio::time::timeout(
        Duration::from_millis(50),
        open(state, account.clone()),
    )
    .await
    .expect_err("上游要 300ms,50ms 内不该回来");

    until_stored(&pool, account.id, 1).await;
}

/// 库里那份过期了而上游答得快,就等它、给新的。
#[tokio::test]
async fn an_expired_platform_playlist_is_replaced_when_the_upstream_is_quick()
 {
    let case = "pp_expired";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let older = track_id(case, 1);
    let newer = track_id(case, 2);

    let state = testing::state(
        pool,
        testing::serve(platform_upstream(&[(
            &older,
            "先加的",
        )]))
        .await,
    );
    let _ = open(state.clone(), account.clone())
        .await
        .expect("第一次打开该回源成功");

    state.playlists.age(account.id, PID, MAX_AGE);
    let changed = testing::with_upstream(
        &state,
        testing::serve(platform_upstream(&[
            (&older, "先加的"),
            (&newer, "后加的"),
        ]))
        .await,
    );
    let tracks = open(changed, account)
        .await
        .expect("该取得到平台歌单");

    assert_eq!(
        tracks.tracks,
        vec![
            expected_dto(&newer, "后加的"),
            expected_dto(&older, "先加的"),
        ],
        "过期了该给新的"
    );
}
