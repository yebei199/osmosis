//! 「听过」的唯一写入口:补记一次播放听了多久,听够了记成听过,并告诉各台。
//!
//! 独奏的补记(`PATCH /played/{id}/listened`)与组里服务端自己判的放完/切走
//! 都走 [`settle`]。各台的电台与日推列表靠 `RadioChanged` 重取;判据与落库在
//! [`crate::store::history::settle`],这里只多一件事:新听过的那首在电台或日推里,
//! 就通知。分开两处写的话,总有一条路忘了通知(组里那条曾经就是)。

use contract::ServerSignal;
use sqlx::PgPool;

use crate::error::AppError;
use crate::store::history::{self, PlayRef};
use crate::store::{daily, radio};
use crate::syncplay::signaling::{
    self, AccountId, SharedRoster,
};

/// 补记 `play` 听了 `listened_ms`(这首长 `duration_ms`)。
///
/// 返回这一下有没有让电台或日推里的一首变成听过 —— 调用方据此看要不要补歌。
pub async fn settle(
    pool: &PgPool,
    roster: &SharedRoster,
    account: AccountId,
    play: &PlayRef,
    listened_ms: i64,
    duration_ms: i64,
) -> Result<bool, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(track) = history::settle(
        &mut conn,
        account,
        play,
        listened_ms,
        duration_ms,
    )
    .await?
    else {
        return Ok(false);
    };
    let listed =
        radio::holds(&mut conn, account, &track).await?
            || daily::holds(&mut conn, account, &track)
                .await?;
    if listed {
        signaling::tell_account(
            roster,
            account,
            &ServerSignal::RadioChanged,
        );
    }
    Ok(listed)
}
