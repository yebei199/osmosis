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
    create_block(
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

    let listed =
        list_blocks(State(state.clone()), account.clone())
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

    let listed = list_blocks(State(state), account)
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
