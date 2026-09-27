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
    let state =
        testing::state(pool, testing::unreachable_upstream());
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
        axum::Json(contract::SetFeedbackDto { verdict: -1 }),
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
    let state =
        testing::state(pool, testing::unreachable_upstream());

    let status = clear_feedback(
        State(state),
        account,
        Path(track_id(case, 1)),
    )
    .await
    .expect("取消不存在的行不该报错");
    assert_eq!(status, StatusCode::NO_CONTENT);
}
