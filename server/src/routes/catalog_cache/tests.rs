//! 缓存回填的集成测试:真实的 Postgres,加一个进程内的假上游。
//!
//! 这里断言的几乎都是**问了平台几次**,而不只是拿回了什么 —— 全量重取与
//! 只补差额给出的曲目列表一模一样,把缓存写成摆设也没有任何断言会红。

use similar_asserts::assert_eq;
use std::collections::HashSet;

use crate::routes::testing::{
    self, FakeUpstream, expected_dto, track_id,
    upstream_track,
};

use super::{DETAIL_BATCH, cached_tracks, fill_details};

use server::cache::TrackRef;

/// 歌单详情随手带回来的那一批够全时,一次补拉都不该发。
///
/// 这是 `docs/adr/0018` 的核心:红心里点一个心,变的只有成员关系,详情
/// 一条都不必重取。写成「每次都全量取」的话,缓存等于不存在,而列表照样
/// 显示正确 —— 除了数问了几次,没有别的办法把这个退化捕获。
#[tokio::test]
async fn cached_tracks_does_not_ask_upstream_when_the_detail_batch_is_complete()
 {
    let case = "cc_complete";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let fake = FakeUpstream::default();
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );

    let first = track_id(case, 1);
    let second = track_id(case, 2);
    let refs = vec![
        TrackRef::new(&first, Some(1_000)),
        TrackRef::new(&second, Some(2_000)),
    ];
    let details = vec![
        expected_dto(&first, "甲"),
        expected_dto(&second, "乙"),
    ];

    let (tracks, dropped) = cached_tracks(
        &state, &account, "p1", &refs, &details,
    )
    .await
    .expect("详情都在手上,不该失败");

    assert_eq!(
        fake.batches(),
        Vec::<Vec<String>>::new(),
        "详情已经随歌单一起拿到了,不该再向平台要一遍"
    );
    assert_eq!(dropped, 0);
    // 次序按加入时间倒排,不是 refs 的原序 —— 见 docs/adr/0021
    assert_eq!(
        tracks,
        vec![
            expected_dto(&second, "乙"),
            expected_dto(&first, "甲"),
        ]
    );
}

/// 只向平台要**缺详情**的那些,不是整份重取。
///
/// 歌单详情带回来的那一批会被平台截断,差额才走补拉。这条盯的是差额算错:
/// 把 refs 整个当成缺的,973 首的歌单每次进去就是五次上游往返。
#[tokio::test]
async fn cached_tracks_only_asks_for_the_ids_the_detail_batch_missed()
 {
    let case = "cc_partial";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let known = track_id(case, 1);
    let missing = track_id(case, 2);

    let fake = FakeUpstream::logged_in_with(
        "42",
        vec![upstream_track(&missing, "乙")],
    );
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );

    let refs = vec![
        TrackRef::new(&known, Some(1_000)),
        TrackRef::new(&missing, Some(2_000)),
    ];

    let (tracks, dropped) = cached_tracks(
        &state,
        &account,
        "p1",
        &refs,
        &[expected_dto(&known, "甲")],
    )
    .await
    .expect("平台给得出缺的那首,不该失败");

    assert_eq!(
        fake.batches(),
        vec![vec![missing.clone()]],
        "只该问缺详情的那一首,随歌单拿到的那首不该再问一遍"
    );
    assert_eq!(dropped, 0);
    assert_eq!(tracks.len(), 2, "两首歌都该在歌单里");
}

/// 平台给不出详情的曲目要被剔出成员关系,并把剔掉的条数报出来。
///
/// 两个都会静默的坑:留着它,写成员关系时外键会拒绝,整个歌单打不开;
/// 剔掉却不报数,歌单静默变短 —— 用户只看到数目对不上,分不清是自己
/// 少点了一个红心还是平台不给这首歌。
#[tokio::test]
async fn cached_tracks_drops_and_counts_the_tracks_the_platform_withholds()
 {
    let case = "cc_dropped";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let available = track_id(case, 1);
    let withheld = track_id(case, 2);

    let fake = FakeUpstream::logged_in_with(
        "42",
        vec![upstream_track(&available, "甲")],
    );
    let state =
        testing::state(pool, testing::serve(fake).await);

    let refs = vec![
        TrackRef::new(&available, Some(1_000)),
        TrackRef::new(&withheld, Some(2_000)),
    ];

    let (tracks, dropped) =
        cached_tracks(&state, &account, "p1", &refs, &[])
            .await
            .expect("少一首详情不该让整个歌单打不开");

    assert_eq!(
        dropped, 1,
        "平台扣下了一首,这个数目必须报出来"
    );
    assert_eq!(
        tracks,
        vec![expected_dto(&available, "甲")]
    );
}

/// 上游整个不可达时报错,而不是回一个残缺的歌单。
///
/// 回空列表或半份列表的话,界面上看到的是「歌少了」而不是「服务出问题了」,
/// 而下一次刷新还会把这份残缺写回缓存。
#[tokio::test]
async fn cached_tracks_fails_loudly_when_the_upstream_is_down()
 {
    let case = "cc_down";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );

    let id = track_id(case, 1);
    let refs = vec![TrackRef::new(&id, Some(1_000))];

    let (status, body) =
        cached_tracks(&state, &account, "p1", &refs, &[])
            .await
            .expect_err("上游连不上却当成功返回了");

    assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY);
    assert_eq!(body.code, "upstream_unreachable");
}

/// 详情已经在库里时,一个请求都不发。
///
/// `fill_details` 分两步问「谁还缺」,第一步问的正是「要不要发请求」。
/// 少了它,冷启动之后的每一次刷新都还在向平台重取全部详情。
#[tokio::test]
async fn fill_details_skips_the_upstream_when_everything_is_cached()
 {
    let case = "fd_cached";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let fake = FakeUpstream::default();
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );
    let mut conn = crate::conn(&state.pool)
        .await
        .expect("取不到数据库连接");

    let id = track_id(case, 1);
    server::cache::put_details(
        &mut conn,
        &[expected_dto(&id, "甲")],
    )
    .await
    .expect("写详情失败");

    let unavailable = fill_details(
        &state,
        &account,
        &mut conn,
        std::slice::from_ref(&id),
    )
    .await
    .expect("详情都在库里,不该失败");

    assert_eq!(
        fake.batches(),
        Vec::<Vec<String>>::new(),
        "详情已经在库里了,不该向平台要"
    );
    assert_eq!(unavailable, HashSet::new());
}

/// 平台跳过的 id 要与「从没问过的」区分开。
///
/// 这正是分两步问的理由:合成一次的话,发完请求之后仍然缺详情的那些,与
/// 压根没发过请求的那些混在一起 —— 而前者该被剔出成员关系,后者不该。
#[tokio::test]
async fn fill_details_reports_only_the_ids_the_platform_skipped()
 {
    let case = "fd_skipped";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    let given = track_id(case, 1);
    let skipped = track_id(case, 2);

    let fake = FakeUpstream::logged_in_with(
        "42",
        vec![upstream_track(&given, "甲")],
    );
    let state =
        testing::state(pool, testing::serve(fake).await);
    let mut conn = crate::conn(&state.pool)
        .await
        .expect("取不到数据库连接");

    let unavailable = fill_details(
        &state,
        &account,
        &mut conn,
        &[given.clone(), skipped.clone()],
    )
    .await
    .expect("平台跳过一首不该让这一步失败");

    assert_eq!(
        unavailable,
        HashSet::from([skipped]),
        "只有平台跳过的那首算「给不出详情」"
    );

    // 平台肯给的那首必须落进库里,否则下次还要重问一遍
    let still_missing = server::cache::missing_details(
        &mut conn,
        "netease",
        std::slice::from_ref(&given),
    )
    .await
    .expect("查缺失详情失败");
    assert_eq!(
        still_missing,
        Vec::<String>::new(),
        "拿回来的详情没写进缓存"
    );
}

/// 补拉按 [`DETAIL_BATCH`] 切批,不是一个请求塞完。
///
/// 973 首的歌单一次要不完 —— 上游把这些 id 拼进一个请求体发给平台,而平台
/// 对请求大小有自己的想法。写成一次全发的现象是冷启动时整个歌单打不开,
/// 而歌少的账号上永远复现不了。
#[tokio::test]
async fn fill_details_asks_in_batches_the_platform_will_accept()
 {
    let case = "fd_batched";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;

    // 刚好比一批多一首:切批写错(比如漏了最后一个残批)立刻露出来
    let ids: Vec<String> = (0..DETAIL_BATCH + 1)
        .map(|n| track_id(case, n))
        .collect();
    let details: Vec<_> = ids
        .iter()
        .map(|id| upstream_track(id, "甲"))
        .collect();

    let fake = FakeUpstream::logged_in_with("42", details);
    let state = testing::state(
        pool,
        testing::serve(fake.clone()).await,
    );
    let mut conn = crate::conn(&state.pool)
        .await
        .expect("取不到数据库连接");

    let unavailable =
        fill_details(&state, &account, &mut conn, &ids)
            .await
            .expect("分批补拉不该失败");

    let sizes: Vec<usize> = fake
        .batches()
        .iter()
        .map(|batch| batch.len())
        .collect();
    assert_eq!(
        sizes,
        vec![DETAIL_BATCH, 1],
        "{} 个 id 该切成一整批加一个残批",
        ids.len()
    );
    assert_eq!(
        unavailable,
        HashSet::new(),
        "平台每一首都给了,不该有给不出的"
    );
}
