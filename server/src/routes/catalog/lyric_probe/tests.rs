//! 歌词探测:分类判据、领了就记、失败退避、下架当没有、刷新不重置、领满不再领。

use std::collections::HashMap;
use std::num::NonZeroU32;

use governor::{Quota, RateLimiter};
use similar_asserts::assert_eq;
use sqlx::PgPool;
use tonic::Code;

use server::bangdream::proto::{Lyric, LyricLine};
use server::store::cache;

use crate::routes::testing::{self, FakeUpstream};

use super::{MAX_ATTEMPTS, kind_of, step};

fn line(text: &str, translation: &str) -> LyricLine {
    LyricLine {
        text: text.to_owned(),
        translation: translation.to_owned(),
        ..LyricLine::default()
    }
}

fn lyric(lines: Vec<LyricLine>) -> Lyric {
    Lyric {
        lines,
        ..Lyric::default()
    }
}

#[test]
fn empty_lyric_is_none() {
    assert_eq!(kind_of(&Lyric::default()).as_str(), "none");
}

/// 网易云给纯音乐的是一句「纯音乐,请欣赏」,前面可能挂着作曲署名。
#[test]
fn the_instrumental_notice_is_instrumental() {
    let notice = lyric(vec![
        line("作曲 : 梶浦由記", ""),
        line("纯音乐,请欣赏", ""),
    ]);
    assert_eq!(kind_of(&notice).as_str(), "instrumental");
}

/// 真歌词里恰好唱到「纯音乐」不算纯音乐。
#[test]
fn a_long_lyric_mentioning_instrumental_is_still_a_lyric() {
    let mut lines: Vec<_> = (0..20)
        .map(|n| line(&format!("第 {n} 行"), ""))
        .collect();
    lines.push(line("我只想听纯音乐", ""));
    assert_eq!(kind_of(&lyric(lines)).as_str(), "lyric");
}

#[test]
fn a_lyric_with_any_translation_is_translated() {
    let lines = vec![
        line("紅蓮華", ""),
        line("強くなれる理由を", "变强的理由"),
    ];
    assert_eq!(kind_of(&lyric(lines)).as_str(), "translated");
}

/// 这条测试独有的平台名:领任务按平台领,别的测试写的网易云行碰不到。
fn platform(case: &str) -> String {
    format!("lp-{}", testing::scoped(case))
}

/// 这条测试的第 n 首。id 带进程前缀,陈旧行由 `sweep_stale_runs` 清掉。
fn id(case: &str, n: usize) -> String {
    testing::track_id(case, n)
}

/// 在这个平台下写几首歌的详情,标记都还是 unknown。
async fn put(pool: &PgPool, case: &str, count: usize) {
    let tracks: Vec<_> = (1..=count)
        .map(|n| contract::TrackDto {
            platform: platform(case),
            ..testing::expected_dto(&id(case, n), "歌")
        })
        .collect();
    let mut conn = pool.acquire().await.expect("取连接");
    cache::put_details(&mut conn, &tracks)
        .await
        .expect("写详情");
}

/// 这首的标记与领过几次。
async fn probed(
    pool: &PgPool,
    case: &str,
    n: usize,
) -> (String, i32) {
    sqlx::query_as(
        "SELECT lyric_kind, lyric_attempts FROM platform_tracks
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(platform(case))
    .bind(id(case, n))
    .fetch_one(pool)
    .await
    .expect("查标记")
}

/// 摆好假上游,跑 `step` 直到领不到,返回办了几首。
async fn drain(
    pool: &PgPool,
    case: &str,
    lyrics: HashMap<String, Result<Lyric, Code>>,
) -> usize {
    let fake = FakeUpstream {
        lyrics,
        ..FakeUpstream::default()
    };
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );
    let limiter = RateLimiter::direct(Quota::per_second(
        NonZeroU32::new(1000).expect("非零"),
    ));
    let mut done = 0;
    while step(&state, &platform(case), &limiter).await {
        done += 1;
    }
    done
}

#[tokio::test]
async fn each_unknown_track_is_probed_once_and_settled() {
    let pool = testing::pool().await;
    let case = "lp_settle";
    // 领任务要库里至少有一个账号当提问的身份;它也清掉同前缀的旧详情,所以先于写详情
    testing::fresh_account(&pool, case).await;
    put(&pool, case, 3).await;

    let done = drain(
        &pool,
        case,
        HashMap::from([
            (
                id(case, 1),
                Ok(lyric(vec![line("歌词", "")])),
            ),
            (id(case, 2), Ok(Lyric::default())),
            // 下架:平台说没这首,也当没有歌词,不无限重试
            (id(case, 3), Err(Code::NotFound)),
        ]),
    )
    .await;

    assert_eq!(done, 3);
    assert_eq!(
        probed(&pool, case, 1).await,
        ("lyric".to_owned(), 1)
    );
    assert_eq!(
        probed(&pool, case, 2).await,
        ("none".to_owned(), 1)
    );
    assert_eq!(
        probed(&pool, case, 3).await,
        ("none".to_owned(), 1)
    );
}

/// 上游一时失败:留在 unknown、记一次,退避期内不再领。
#[tokio::test]
async fn a_failed_probe_backs_off() {
    let pool = testing::pool().await;
    let case = "lp_backoff";
    // 领任务要库里至少有一个账号当提问的身份;它也清掉同前缀的旧详情,所以先于写详情
    testing::fresh_account(&pool, case).await;
    put(&pool, case, 1).await;

    let done = drain(
        &pool,
        case,
        HashMap::from([(
            id(case, 1),
            Err(Code::Unavailable),
        )]),
    )
    .await;

    assert_eq!(done, 1, "失败的那首退避之后不该马上被再领");
    assert_eq!(
        probed(&pool, case, 1).await,
        ("unknown".to_owned(), 1)
    );
}

/// 领满上限的不再领,免得一首坏歌把限速额度吃光。
#[tokio::test]
async fn a_track_at_the_attempt_cap_is_not_claimed() {
    let pool = testing::pool().await;
    let case = "lp_cap";
    // 领任务要库里至少有一个账号当提问的身份;它也清掉同前缀的旧详情,所以先于写详情
    testing::fresh_account(&pool, case).await;
    put(&pool, case, 1).await;
    sqlx::query(
        "UPDATE platform_tracks SET lyric_attempts = $2
         WHERE platform = $1",
    )
    .bind(platform(case))
    .bind(MAX_ATTEMPTS)
    .execute(&pool)
    .await
    .expect("改次数");

    assert_eq!(drain(&pool, case, HashMap::new()).await, 0);
}

/// 详情刷新(歌单重拉、日推再取)不把探过的标记打回 unknown。
#[tokio::test]
async fn refreshing_details_keeps_the_lyric_kind() {
    let pool = testing::pool().await;
    let case = "lp_refresh";
    // 领任务要库里至少有一个账号当提问的身份;它也清掉同前缀的旧详情,所以先于写详情
    testing::fresh_account(&pool, case).await;
    put(&pool, case, 1).await;
    drain(
        &pool,
        case,
        HashMap::from([(
            id(case, 1),
            Ok(lyric(vec![line("歌词", "翻译")])),
        )]),
    )
    .await;

    put(&pool, case, 1).await;

    assert_eq!(
        probed(&pool, case, 1).await,
        ("translated".to_owned(), 1)
    );
}
