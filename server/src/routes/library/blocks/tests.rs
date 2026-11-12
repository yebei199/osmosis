//! 屏蔽规则路由的测试(#161)。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use contract::{BlockKind, NewBlockRuleDto};

use crate::routes::testing::{self, FakeUpstream};

use super::*;

fn new_rule(
    kind: BlockKind,
    value: &str,
) -> Json<NewBlockRuleDto> {
    Json(NewBlockRuleDto {
        disliked_track: None,
        kind,
        value: value.to_owned(),
        label: None,
    })
}

/// 建三种规则各一条,列出来是三条;同一条再建一次不多出一行;删掉一条剩两条。
#[tokio::test]
async fn create_list_dedupe_and_delete() {
    let case = "blocks_crud";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool.clone(),
        testing::serve(FakeUpstream::default()).await,
    );

    let artist = create_block(
        State(state.clone()),
        account.clone(),
        new_rule(BlockKind::Artist, "某人"),
    )
    .await
    .expect("建歌手规则该成功")
    .0;
    let _tag = create_block(
        State(state.clone()),
        account.clone(),
        new_rule(BlockKind::Tag, "吵"),
    )
    .await
    .expect("建标签规则该成功");
    let track = create_block(
        State(state.clone()),
        account.clone(),
        Json(NewBlockRuleDto {
            disliked_track: None,
            kind: BlockKind::Track,
            value: "123".to_owned(),
            label: Some("某首歌".to_owned()),
        }),
    )
    .await
    .expect("建单曲规则该成功")
    .0;
    assert_eq!(
        artist.label, "某人",
        "没给 label 就用 value"
    );
    assert_eq!(track.label, "某首歌");

    let again = create_block(
        State(state.clone()),
        account.clone(),
        new_rule(BlockKind::Artist, "某人"),
    )
    .await
    .expect("重复建该成功")
    .0;
    assert_eq!(again.id, artist.id, "同一条规则只有一行");

    let listed = list_blocks(
        State(state.clone()),
        account.clone(),
        Query(BlockCapabilities::default()),
    )
    .await
    .expect("列规则该成功")
    .0;
    assert_eq!(listed.rules.len(), 3);

    let status = delete_block(
        State(state.clone()),
        account.clone(),
        Path(artist.id.clone()),
    )
    .await
    .expect("删规则该成功");
    assert_eq!(status, StatusCode::NO_CONTENT);

    let listed = list_blocks(
        State(state),
        account,
        Query(BlockCapabilities::default()),
    )
    .await
    .expect("列规则该成功")
    .0;
    assert_eq!(
        listed
            .rules
            .iter()
            .map(|rule| rule.kind)
            .collect::<Vec<_>>(),
        vec![BlockKind::Tag, BlockKind::Track]
    );
}

/// 空值拒绝;删别人的规则是 404,不是 403。
#[tokio::test]
async fn empty_value_is_rejected_and_foreign_rule_is_not_found()
 {
    let case = "blocks_guard";
    let pool = testing::pool().await;
    let owner = testing::fresh_account(&pool, case).await;
    let other =
        testing::fresh_account(&pool, "blocks_guard_other")
            .await;
    let state = testing::state(
        pool.clone(),
        testing::serve(FakeUpstream::default()).await,
    );

    let failure = create_block(
        State(state.clone()),
        owner.clone(),
        new_rule(BlockKind::Artist, "   "),
    )
    .await
    .expect_err("空值该被拒");
    assert_eq!(failure.0, StatusCode::BAD_REQUEST);

    let rule = create_block(
        State(state.clone()),
        owner,
        new_rule(BlockKind::Artist, "某人"),
    )
    .await
    .expect("建规则该成功")
    .0;
    let failure =
        delete_block(State(state), other, Path(rule.id))
            .await
            .expect_err("删别人的该失败");
    assert_eq!(failure.0, StatusCode::NOT_FOUND);
}

/// 日推里两首歌:一首是被屏蔽的歌手唱的,一首打了被屏蔽的标签,第三首干净。
fn three_tracks(
    case: &str,
) -> Vec<server::bangdream::proto::Track> {
    let mut by_blocked = testing::upstream_track(
        &testing::track_id(case, 1),
        "被屏蔽歌手的歌",
    );
    by_blocked.artists[0].name = "屏蔽我".to_owned();
    vec![
        by_blocked,
        testing::upstream_track(
            &testing::track_id(case, 2),
            "打了吵标签",
        ),
        testing::upstream_track(
            &testing::track_id(case, 3),
            "干净",
        ),
    ]
}

fn ids(tracks: &[contract::TrackDto]) -> Vec<String> {
    tracks.iter().map(|track| track.id.clone()).collect()
}

/// 歌手规则与标签规则在日推与搜索的出口都生效,并报出藏了几首;
/// 删掉规则,歌回来、隐藏数归零。
#[tokio::test]
async fn rules_hide_tracks_at_every_outlet_until_removed() {
    use crate::routes::catalog::search::{
        SearchQuery, daily, search_tracks,
    };

    let case = "blocks_outlets";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake = FakeUpstream {
        listed: three_tracks(case),
        ..FakeUpstream::default()
    };
    let state = testing::state(
        pool.clone(),
        testing::serve(fake).await,
    );

    let mut conn = pool.acquire().await.expect("取连接");
    let tag = server::store::tags::create(
        &mut conn, account.id, "吵",
    )
    .await
    .expect("建标签");
    server::store::tags::tag_track(
        &mut conn,
        account.id,
        tag.id,
        "netease",
        &testing::track_id(case, 2),
    )
    .await
    .expect("打标签");
    drop(conn);

    let artist_rule = create_block(
        State(state.clone()),
        account.clone(),
        new_rule(BlockKind::Artist, "屏蔽我"),
    )
    .await
    .expect("建歌手规则")
    .0;
    let tag_rule = create_block(
        State(state.clone()),
        account.clone(),
        new_rule(BlockKind::Tag, "吵"),
    )
    .await
    .expect("建标签规则")
    .0;

    let clean = vec![testing::track_id(case, 3)];
    let today =
        daily(State(state.clone()), account.clone())
            .await
            .expect("日推")
            .0;
    assert_eq!(ids(&today.tracks), clean);

    let search = || {
        search_tracks(
            State(state.clone()),
            account.clone(),
            axum::extract::Query(SearchQuery {
                q: "随便".to_owned(),
                limit: None,
                offset: None,
            }),
        )
    };
    let found = search().await.expect("搜索").0;
    assert_eq!(ids(&found.tracks), clean);
    assert_eq!(found.hidden, 2);

    for rule in [artist_rule, tag_rule] {
        delete_block(
            State(state.clone()),
            account.clone(),
            Path(rule.id),
        )
        .await
        .expect("删规则");
    }
    let today =
        daily(State(state.clone()), account.clone())
            .await
            .expect("日推")
            .0;
    assert_eq!(
        ids(&today.tracks),
        clean,
        "日推在入列时就丢了屏蔽的,规则删了也不回来"
    );
    let found = search().await.expect("搜索").0;
    assert_eq!(found.tracks.len(), 3);
}

/// 屏蔽生效到底(ADR 0037):电台与日推里拿掉,预取撤掉,桶里那份删掉;红心留着,
/// 「我喜欢」里藏起来,规则删了就回来(用户 2026-11-10)。
#[tokio::test]
async fn blocking_a_track_clears_it_everywhere() {
    use std::sync::Arc;

    use server::objects::Objects as _;
    use server::store::archive as ledger;

    use crate::routes::play::archive::{Archive, quality};
    use crate::routes::testing::{
        MemoryObjects, track_id, upstream_track,
    };

    let case = "blocks_purge";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let objects = Arc::new(MemoryObjects::default());
    let state = AppState {
        archive: Some(Archive::new(objects.clone())),
        ..testing::state(
            pool.clone(),
            testing::serve(FakeUpstream::default()).await,
        )
    };
    let id = track_id(case, 1);
    let track = TrackRef {
        platform: "netease".to_owned(),
        track_id: id.clone(),
    };
    let mut conn = pool.acquire().await.unwrap();
    cache::put_details(
        &mut conn,
        &[server::bangdream::track_to_dto(upstream_track(
            &id, "吵",
        ))],
    )
    .await
    .unwrap();
    let refs = std::slice::from_ref(&track);
    liked::set(&mut conn, account.id, &track, true)
        .await
        .unwrap();
    radio::append(&mut conn, account.id, refs)
        .await
        .unwrap();
    daily::add(&mut conn, account.id, refs).await.unwrap();
    prefetch::enqueue(&mut conn, account.id, refs, "other")
        .await
        .unwrap();
    let key = format!("tracks/{id}.flac");
    objects
        .put(
            &key,
            server::objects::whole(vec![1u8, 2, 3]),
            3,
            "audio/flac",
        )
        .await
        .unwrap();
    ledger::record(
        &mut conn,
        &ledger::StoredTrack {
            platform: "netease".to_owned(),
            track_id: id.clone(),
            quality: quality(),
            object_key: key.clone(),
            format: "flac".to_owned(),
            bit_rate: 999_000,
            bytes: 3,
            tier: "lossless".to_owned(),
            bits_per_sample: None,
            sample_rate: None,
        },
    )
    .await
    .unwrap();
    let rule = blocks::create(
        &mut conn,
        account.id,
        BlockKind::Track,
        &id,
        None,
    )
    .await
    .unwrap();

    purge(&state, account.id).await.expect("清屏蔽");

    assert!(
        liked::refs(&mut conn, account.id)
            .await
            .unwrap()
            .contains(&track),
        "屏蔽不删红心"
    );
    let liked_now = || {
        let (state, account) =
            (state.clone(), account.clone());
        async move {
            crate::routes::library::likes::liked(
                State(state),
                account,
                axum::extract::Query(
                    crate::routes::library::SortQuery::default(),
                ),
            )
            .await
            .expect("读我喜欢")
            .0
        }
    };
    let hidden = liked_now().await;
    assert_eq!(
        (hidden.tracks.len(), hidden.hidden),
        (0, 1),
        "「我喜欢」里藏起来"
    );
    assert_eq!(
        radio::list_all(&mut conn, account.id)
            .await
            .unwrap(),
        Vec::new()
    );
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM prefetch_jobs WHERE track_id = $1",
    )
    .bind(&id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(jobs, 0);
    assert_eq!(objects.get(&key), None);

    blocks::delete(
        &mut conn,
        account.id,
        rule.id.parse().unwrap(),
    )
    .await
    .unwrap();
    let back = liked_now().await;
    assert_eq!(
        back.tracks
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>(),
        vec![id.clone()],
        "规则删了,红心原样回来"
    );
}
