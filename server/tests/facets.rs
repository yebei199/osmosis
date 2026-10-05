//! 歌单视图聚合的集成测试(#160),对着真实 Postgres 跑。

use std::time::{Duration, Instant};

use contract::{LyricKindDto, TrackDto, TrackFacetsDto};
use server::store::account::{Account, register};
use server::store::playlist::TrackRef;
use server::store::{
    cache, db, facets, feedback, history, tags,
};
use sqlx::{PgPool, Postgres, Transaction};

const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

const INVITE: &str = "let-me-in";

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );

    db::connect(&url).await.unwrap_or_else(|err| {
        panic!(
            "连不上数据库({url}): {err}\n\
             起一个:just pg"
        )
    })
}

async fn tx() -> Transaction<'static, Postgres> {
    pool().await.begin().await.expect("开事务失败")
}

async fn make_account(
    tx: &mut Transaction<'static, Postgres>,
    username: &str,
) -> Account {
    register(tx, username, "correct horse", INVITE, INVITE)
        .await
        .expect("注册应该成功")
}

fn track(id: &str) -> TrackDto {
    TrackDto {
        artist_identities: Vec::new(),
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: format!("曲 {id}"),
        alias: None,
        artists: vec!["某人".to_owned()],
        cover: None,
        duration_ms: 200_000,
        album: None,
        facets: TrackFacetsDto::default(),
    }
}

fn key(id: &str) -> TrackRef {
    TrackRef {
        platform: "netease".to_owned(),
        track_id: id.to_owned(),
    }
}

/// 播一次,`listened` 给了就补记听了多久(时长 200 秒)。
async fn play(
    tx: &mut Transaction<'static, Postgres>,
    account: &Account,
    id: &str,
    listened_ms: Option<i64>,
) {
    let event = history::record(tx, account.id, &key(id))
        .await
        .expect("记起播");
    if let Some(listened_ms) = listened_ms {
        history::report_listened(
            tx,
            account.id,
            event,
            listened_ms,
            200_000,
        )
        .await
        .expect("补记听了多久");
    }
}

/// 五样都从各自的表里聚合回来。
#[tokio::test]
async fn every_facet_is_aggregated() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "fc_all").await;
    let id = "fc_all_1";
    cache::put_details(&mut tx, &[track(id)])
        .await
        .expect("写详情");
    sqlx::query(
        "UPDATE platform_tracks SET lyric_kind = 'translated'
         WHERE track_id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .expect("标歌词");
    // 四次:30 秒整不算跳过、29.999 秒算、听了一大半也不算、一次没补记
    // (不进跳过率的分母)。口径见 facets::SKIP_WITHIN_MS
    play(
        &mut tx,
        &account,
        id,
        Some(facets::SKIP_WITHIN_MS),
    )
    .await;
    play(
        &mut tx,
        &account,
        id,
        Some(facets::SKIP_WITHIN_MS - 1),
    )
    .await;
    play(&mut tx, &account, id, Some(90_000)).await;
    play(&mut tx, &account, id, None).await;
    feedback::set(&mut tx, account.id, &key(id), -1)
        .await
        .expect("点踩");
    for name in ["夜", "雨"] {
        let tag = tags::create(&mut tx, account.id, name)
            .await
            .expect("建标签");
        tags::tag_track(
            &mut tx, account.id, tag.id, "netease", id,
        )
        .await
        .expect("打标签");
    }

    let mut tracks = vec![track(id)];
    facets::fill(&mut tx, account.id, &mut tracks)
        .await
        .expect("聚合");

    assert_eq!(
        tracks[0].facets,
        TrackFacetsDto {
            play_count: 4,
            verdict: Some(-1),
            skip_rate: Some(33),
            lyric_kind: Some(LyricKindDto::Translated),
            tags: vec!["夜".to_owned(), "雨".to_owned()],
        }
    );
}

/// 什么都没有的歌给默认值;别的账号的播放、赞踩、标签不算进来。
#[tokio::test]
async fn untouched_and_foreign_tracks_read_default() {
    let mut tx = tx().await;
    let me = make_account(&mut tx, "fc_me").await;
    let other = make_account(&mut tx, "fc_other").await;
    let id = "fc_foreign_1";
    play(&mut tx, &other, id, Some(1_000)).await;
    feedback::set(&mut tx, other.id, &key(id), 1)
        .await
        .expect("别人点赞");

    // 不在缓存里的那首:歌手页、最近播放的旧数据就是这样
    let mut tracks = vec![track(id), track(id)];
    facets::fill(&mut tx, me.id, &mut tracks)
        .await
        .expect("聚合");

    assert!(tracks.iter().all(|t| t.facets.is_empty()));
}

/// 千首的歌单一次聚合完,不随歌单变长而一首一查(#160 的风险项)。
#[tokio::test]
async fn a_thousand_tracks_fill_quickly() {
    const SIZE: usize = 1000;
    let mut tx = tx().await;
    let account = make_account(&mut tx, "fc_big").await;
    let tracks: Vec<TrackDto> = (0..SIZE)
        .map(|i| track(&format!("fc_big_{i}")))
        .collect();
    cache::put_details(&mut tx, &tracks)
        .await
        .expect("写详情");
    for i in (0..SIZE).step_by(3) {
        play(
            &mut tx,
            &account,
            &format!("fc_big_{i}"),
            Some(1_000),
        )
        .await;
    }

    let mut filled = tracks.clone();
    let started = Instant::now();
    facets::fill(&mut tx, account.id, &mut filled)
        .await
        .expect("聚合");
    let took = started.elapsed();
    println!("千首聚合耗时 {took:?}");

    assert_eq!(filled[0].facets.play_count, 1);
    assert_eq!(filled[1].facets.play_count, 0);
    assert!(
        took < Duration::from_millis(500),
        "千首聚合用了 {took:?}"
    );
}
