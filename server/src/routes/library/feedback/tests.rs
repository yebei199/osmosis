//! 赞踩路由的测试(#157)。不问网易云 —— 这张表独立于红心与目录。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use similar_asserts::assert_eq;

use crate::routes::testing::{self, track_id};

use super::{clear_feedback, get_feedback, set_feedback};

/// 点赞 → 再点踩(覆盖成 -1)→ 再点一次踩(取消,行消失)。
/// 与 issue #157 的验收步骤同形。
#[tokio::test]
async fn like_then_dislike_then_cancel() {
    let case = "fb_toggle";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );
    let song = track_id(case, 1);

    set_feedback(
        State(state.clone()),
        account.clone(),
        Path(song.clone()),
        axum::Json(contract::SetFeedbackDto { verdict: 1 }),
    )
    .await
    .expect("点赞该成功");
    assert_eq!(
        get_feedback(
            State(state.clone()),
            account.clone(),
            Path(song.clone()),
        )
        .await
        .expect("该读得到")
        .0
        .verdict,
        Some(1)
    );

    set_feedback(
        State(state.clone()),
        account.clone(),
        Path(song.clone()),
        axum::Json(contract::SetFeedbackDto {
            verdict: -1,
        }),
    )
    .await
    .expect("点踩该覆盖成 -1");
    assert_eq!(
        get_feedback(
            State(state.clone()),
            account.clone(),
            Path(song.clone()),
        )
        .await
        .expect("该读得到")
        .0
        .verdict,
        Some(-1)
    );

    let status = clear_feedback(
        State(state.clone()),
        account.clone(),
        Path(song.clone()),
    )
    .await
    .expect("取消该成功");
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        get_feedback(State(state), account, Path(song))
            .await
            .expect("该读得到")
            .0
            .verdict,
        None
    );
}

/// 取消一首本来就没表过态的歌不报错 —— 与红心同一个理由(两次的意图是同一个)。
#[tokio::test]
async fn cancelling_untouched_track_is_a_noop() {
    let case = "fb_noop_cancel";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );

    let status = clear_feedback(
        State(state),
        account,
        Path(track_id(case, 1)),
    )
    .await
    .expect("取消不存在的行不该报错");
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 特别喜欢(ADR 0042):设得上、读得回;再点赞就覆盖成 1,与赞踩同一套覆盖语义。
#[tokio::test]
async fn love_is_a_level_that_other_levels_replace() {
    let case = "fb_love";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );
    let song = track_id(case, 1);
    let read = |state: crate::AppState| {
        let account = account.clone();
        let song = song.clone();
        async move {
            get_feedback(State(state), account, Path(song))
                .await
                .expect("该读得到")
                .0
                .verdict
        }
    };

    set_feedback(
        State(state.clone()),
        account.clone(),
        Path(song.clone()),
        axum::Json(contract::SetFeedbackDto { verdict: 2 }),
    )
    .await
    .expect("特别喜欢该成功");
    assert_eq!(read(state.clone()).await, Some(2));

    set_feedback(
        State(state.clone()),
        account.clone(),
        Path(song.clone()),
        axum::Json(contract::SetFeedbackDto { verdict: 1 }),
    )
    .await
    .expect("改成赞该成功");
    assert_eq!(read(state).await, Some(1));
}

/// 值域之外的(0、3)拒掉:未表态走 `DELETE`,不靠一个特殊值。
#[tokio::test]
async fn verdicts_outside_the_levels_are_rejected() {
    let case = "fb_out_of_range";
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );

    for verdict in [0, 3, -2] {
        let refused = set_feedback(
            State(state.clone()),
            account.clone(),
            Path(track_id(case, 1)),
            axum::Json(contract::SetFeedbackDto {
                verdict,
            }),
        )
        .await;
        assert!(refused.is_err(), "{verdict} 不该设得上");
    }
}
