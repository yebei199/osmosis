//! 标签路由的测试(#158)。

use axum::extract::Path;
use axum::http::StatusCode;

use server::store::tags;

use crate::routes::testing::{self, FakeUpstream};

use super::*;

/// 建标签、打到两首歌上,查库两行;删标签,`track_tags` 级联清空。
///
/// 这是验收步骤里那条脚本的等价 Rust 版:比 curl 一遍更接近「以后改坏了会红」。
#[tokio::test]
async fn create_tag_track_two_songs_then_delete_cascades() {
    let case = "tag_two_songs";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool.clone(),
        testing::serve(FakeUpstream::default()).await,
    );

    let tag = create_tag(
        axum::extract::State(state.clone()),
        account.clone(),
        axum::Json(NameBody {
            name: "深夜循环".to_owned(),
        }),
    )
    .await
    .expect("建标签该成功")
    .0;

    tag_track(
        axum::extract::State(state.clone()),
        account.clone(),
        Path((
            tag.id.clone(),
            "netease".to_owned(),
            "111".to_owned(),
        )),
    )
    .await
    .expect("打标签该成功");
    tag_track(
        axum::extract::State(state.clone()),
        account.clone(),
        Path((
            tag.id.clone(),
            "netease".to_owned(),
            "222".to_owned(),
        )),
    )
    .await
    .expect("打标签该成功");

    let mut conn = pool.acquire().await.unwrap();
    let (rows,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM track_tags WHERE tag_id = $1",
    )
    .bind(tag.id.parse::<i64>().unwrap())
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(rows, 2, "两首歌各一行");

    let first_tags = track_tags(
        axum::extract::State(state.clone()),
        account.clone(),
        Path(("netease".to_owned(), "111".to_owned())),
    )
    .await
    .expect("查这首歌的标签该成功")
    .0;
    assert_eq!(
        first_tags
            .tags
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>(),
        vec![tag.id.clone()],
    );

    let status = delete_tag(
        axum::extract::State(state.clone()),
        account.clone(),
        Path(tag.id.clone()),
    )
    .await
    .expect("删标签该成功");
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (rows,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM track_tags WHERE tag_id = $1",
    )
    .bind(tag.id.parse::<i64>().unwrap())
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(rows, 0, "删标签要把两条关联一并带走");
}

/// 摘掉标签是幂等的:摘两遍不报错。
#[tokio::test]
async fn untagging_twice_is_a_no_op() {
    let case = "tag_untag_idempotent";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool,
        testing::serve(FakeUpstream::default()).await,
    );

    let tag = create_tag(
        axum::extract::State(state.clone()),
        account.clone(),
        axum::Json(NameBody {
            name: "轻音乐".to_owned(),
        }),
    )
    .await
    .unwrap()
    .0;

    untag_track(
        axum::extract::State(state.clone()),
        account.clone(),
        Path((
            tag.id.clone(),
            "netease".to_owned(),
            "333".to_owned(),
        )),
    )
    .await
    .expect("没打过也该能摘,幂等");
    untag_track(
        axum::extract::State(state.clone()),
        account,
        Path((
            tag.id,
            "netease".to_owned(),
            "333".to_owned(),
        )),
    )
    .await
    .expect("摘两遍也不该报错");
}

/// 建标签给相同的名字,直接复用已有的那个,不建第二条。
#[tokio::test]
async fn creating_a_tag_with_an_existing_name_reuses_it() {
    let case = "tag_reuse_name";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let mut conn = pool.acquire().await.unwrap();

    let first = tags::create(&mut conn, account.id, "国风")
        .await
        .unwrap();
    let second =
        tags::create(&mut conn, account.id, "国风")
            .await
            .unwrap();

    assert_eq!(first.id, second.id);

    let all =
        tags::list(&mut conn, account.id).await.unwrap();
    assert_eq!(
        all.iter().filter(|t| t.name == "国风").count(),
        1,
        "重名不该建出第二条"
    );
}
