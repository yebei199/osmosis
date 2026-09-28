//! 「我的库」:红心、本地歌单、播放历史。
//!
//! 真相在自家 Postgres([`crate::store`]),曲目详情向 [`super::catalog`] 那侧借。

pub(crate) mod blocks;
pub(crate) mod feedback;
pub(crate) mod history;
pub(crate) mod likes;
pub(crate) mod playlists;
pub(crate) mod tags;

use contract::TracksDto;
use server::error::AppError;
use server::store::{blocks as block_rules, facets};
use sqlx::PgConnection;

use crate::AppState;

/// 歌单类接口交出去之前的最后一道:填上账号级聚合(#160),再滤掉命中
/// 屏蔽规则的、把滤掉几首记进 `hidden`(#161)。所有返回曲目列表的路由都走它,
/// 过滤因此只在服务端出口做一次,各端一致。
///
/// 填不上只写日志,列表照样交出去:没有聚合只是分不了组,不该连歌都看不到。
pub(crate) async fn for_account(
    state: &AppState,
    account_id: i64,
    mut found: TracksDto,
) -> TracksDto {
    let shaped = match state.pool.acquire().await {
        Ok(mut conn) => {
            shape(&mut conn, account_id, &mut found)
                .await
                .map_err(|err| format!("{err:?}"))
        }
        Err(err) => Err(err.to_string()),
    };
    if let Err(err) = shaped {
        tracing::warn!(%err, "歌单聚合或屏蔽过滤没做成");
    }
    found
}

/// 标签规则认的是聚合里的标签,所以先填聚合、再滤。
async fn shape(
    conn: &mut PgConnection,
    account_id: i64,
    found: &mut TracksDto,
) -> Result<(), AppError> {
    facets::fill(conn, account_id, &mut found.tracks)
        .await?;
    let rules = block_rules::list(conn, account_id).await?;
    found.hidden +=
        block_rules::drop_hits(&rules, &mut found.tracks);
    Ok(())
}
