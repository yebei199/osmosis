//! 「我的库」:红心、本地歌单、播放历史。
//!
//! 真相在自家 Postgres([`crate::store`]),曲目详情向 [`super::catalog`] 那侧借。

pub(crate) mod feedback;
pub(crate) mod history;
pub(crate) mod likes;
pub(crate) mod playlists;
pub(crate) mod tags;

use contract::TracksDto;
use server::store::facets;

use crate::AppState;

/// 给歌单类接口的这一批填上账号级聚合(#160),分组与筛选靠它。
///
/// 填不上只写日志,列表照样交出去:没有聚合只是分不了组,不该连歌都看不到。
pub(crate) async fn with_facets(
    state: &AppState,
    account_id: i64,
    mut found: TracksDto,
) -> TracksDto {
    let filled = match state.pool.acquire().await {
        Ok(mut conn) => facets::fill(
            &mut conn,
            account_id,
            &mut found.tracks,
        )
        .await
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    if let Err(err) = filled {
        tracing::warn!(%err, "歌单聚合填不上");
    }
    found
}
