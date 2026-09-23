//! `GET /playlists` 平台那半「先回缓存、后台回源」的测试(#124)。
//!
//! 生产上这条每次当场回源两趟,9.9 秒贴着客户端 10 秒的超时。这里断言的是
//! **等不等上游**:上游卡死时照样回来,等了的实现会卡在测试的超时上。

use std::time::Duration;

use axum::extract::{Path, State};
use contract::{PlaylistDto, PlaylistSource};
use similar_asserts::assert_eq;

use server::bangdream::proto::Playlist;
use server::store::playlist;

use crate::routes::catalog::catalog_cache::{
    FRESH_WAIT, REFRESH_EVERY,
};
use crate::routes::testing::{self, FakeUpstream};

use super::playlists;
use crate::routes::library::likes::subscribe_playlist;

/// 上游「卡死」:比任何一条测试的超时都长得多。
const STALLED: Duration = Duration::from_secs(3600);

/// 一个已登录、有这些平台歌单的上游。
fn upstream_with(names: &[&str]) -> FakeUpstream {
    let mut fake =
        FakeUpstream::logged_in_with("42", vec![]);
    fake.playlists = names
        .iter()
        .map(|name| Playlist {
            id: format!("id-{name}"),
            name: (*name).to_owned(),
            ..Playlist::default()
        })
        .collect();
    fake
}

/// 响应里平台歌单的名字,按出现次序。
fn platform_names(lists: &[PlaylistDto]) -> Vec<String> {
    lists
        .iter()
        .filter(|list| {
            list.source == PlaylistSource::Platform
        })
        .map(|list| list.name.clone())
        .collect()
}

/// 平台那半有一份时,第二次打开不等上游。
#[tokio::test]
async fn playlists_answer_the_platform_half_without_waiting()
 {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pl_cached").await;

    let fake = upstream_with(&["甲"]);
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );
    let first =
        playlists(State(state.clone()), account.clone())
            .await
            .expect("第一次打开该成功")
            .0;

    state.platform_lists.age(account.id, REFRESH_EVERY);
    let stalled = testing::with_upstream(
        &state,
        testing::serve(FakeUpstream {
            lists_delay: STALLED,
            ..fake
        })
        .await,
    );
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        playlists(State(stalled), account),
    )
    .await
    .expect("第二次打开等了上游")
    .expect("该成功")
    .0;

    assert_eq!(platform_names(&first.playlists), ["甲"]);
    assert_eq!(second, first);
}

/// 第一次打开上游慢得等不起:最多等 `FRESH_WAIT`,先给本地那半。
///
/// 本地歌单在自家库里,一个上游慢不该把它也扣下 —— 与「上游要不到平台歌单时
/// 不整个失败」同一个理由。
#[tokio::test]
async fn a_slow_first_open_still_gives_the_local_half() {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pl_slow_first")
            .await;
    let mut conn =
        pool.acquire().await.expect("取不到连接");
    playlist::create(&mut conn, account.id, "自己的")
        .await
        .expect("建本地歌单该成功");

    let state = testing::state(
        pool.clone(),
        testing::serve(FakeUpstream {
            lists_delay: STALLED,
            ..upstream_with(&["甲"])
        })
        .await,
    );
    let got = tokio::time::timeout(
        FRESH_WAIT + Duration::from_secs(2),
        playlists(State(state), account),
    )
    .await
    .expect("等上游超过了 FRESH_WAIT")
    .expect("该成功")
    .0;

    let names: Vec<&str> = got
        .playlists
        .iter()
        .map(|list| list.name.as_str())
        .collect();
    assert_eq!(names, ["我喜欢的", "自己的"]);
}

/// 客户端等不及断开,回源照样跑完并记下,下一次就有平台那半。
#[tokio::test]
async fn a_first_fetch_finishes_even_when_the_client_gives_up()
 {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pl_client_gave_up")
            .await;

    let fake = upstream_with(&["甲"]);
    let state = testing::state(
        pool,
        testing::serve(FakeUpstream {
            lists_delay: Duration::from_millis(300),
            ..fake.clone()
        })
        .await,
    );
    tokio::time::timeout(
        Duration::from_millis(50),
        playlists(State(state.clone()), account.clone()),
    )
    .await
    .expect_err("上游要 300ms,50ms 内不该回来");

    tokio::time::timeout(Duration::from_secs(5), async {
        while state.platform_lists.get(account.id).is_none()
        {
            tokio::time::sleep(Duration::from_millis(20))
                .await;
        }
    })
    .await
    .expect("回源没跑完");

    let stalled = testing::with_upstream(
        &state,
        testing::serve(FakeUpstream {
            lists_delay: STALLED,
            ..fake
        })
        .await,
    );
    let got = tokio::time::timeout(
        Duration::from_secs(2),
        playlists(State(stalled), account),
    )
    .await
    .expect("有了那份还在等上游")
    .expect("该成功")
    .0;
    assert_eq!(platform_names(&got.playlists), ["甲"]);
}

/// 在这边收藏了一个歌单,下一次打开去问上游,而不是答缓存里那份。
#[tokio::test]
async fn subscribing_here_refreshes_the_platform_half_next_time()
 {
    let pool = testing::pool().await;
    let account =
        testing::fresh_account(&pool, "pl_subscribe").await;

    let state = testing::state(
        pool,
        testing::serve(upstream_with(&["甲"])).await,
    );
    let _ =
        playlists(State(state.clone()), account.clone())
            .await
            .expect("第一次打开该成功");

    let subscribed = testing::with_upstream(
        &state,
        testing::serve(upstream_with(&["甲", "乙"])).await,
    );
    subscribe_playlist(
        State(subscribed.clone()),
        account.clone(),
        Path("id-乙".to_owned()),
    )
    .await
    .expect("收藏该转发成功");

    let got = playlists(State(subscribed), account)
        .await
        .expect("该成功")
        .0;
    assert_eq!(
        platform_names(&got.playlists),
        ["甲", "乙"]
    );
}
