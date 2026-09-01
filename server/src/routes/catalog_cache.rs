//! 平台曲目的缓存回填:先读缓存,缺的按批向上游补,补完写回。
//!
//! 红心与歌单两条路都要用它,所以它不跟着任何一条走。

use contract::TrackDto;
use std::collections::HashSet;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{
        GetPlaylistResponse, GetTracksRequest, Platform,
    },
};
use server::error::Failure;
use server::{cache, error};

use crate::{AppState, conn, fail};

/// 一次向上游要多少首曲目详情。
///
/// 973 首的歌单一次要不完 —— 上游把这些 id 拼进一个请求体发给平台,而平台对
/// 请求大小有自己的想法。分批只在**冷启动**发生:详情缓存下来之后,常态是
/// 一批都不用要。
pub(crate) const DETAIL_BATCH: usize = 200;

/// 把一个平台歌单的曲目备齐,并按平台给的次序读出来。
///
/// 只向平台要**缺详情**的那些:歌单的成员关系天天变(点一次红心就变一次),
/// 而曲目详情几乎不变,且跨歌单共用 —— 收藏的歌单里的歌大半已经在红心里了。
/// 每次都全量重取的话,这个缓存等于没有(见 `docs/adr/0018`)。
///
/// 平台不肯给详情的 id(下架、无权限)会被剔出成员关系:留着它只会在读回时
/// 的 JOIN 里消失,那时歌单少一首而没有任何人报错。
pub(crate) async fn cached_tracks(
    state: &AppState,
    account: &Account,
    playlist_id: &str,
    refs: &[cache::TrackRef],
    detail_tracks: &[TrackDto],
) -> Result<(Vec<TrackDto>, usize), Failure> {
    let mut conn = conn(&state.pool).await?;
    let platform = netease_name();

    // 歌单详情随手带回来的那一批先入库,它们不必再问平台要一遍。
    // 平台把这一批截断时,只有差额才走补拉。
    cache::put_details(&mut conn, detail_tracks)
        .await
        .map_err(|err| error::map_error(&err))?;
    let missing =
        bangdream::refs_missing_from(detail_tracks, refs);

    let unavailable =
        fill_details(state, account, &mut conn, &missing)
            .await?;
    // 剔掉平台给不出详情的那些,并记下剔了几条 —— 静默变短的歌单没人报得出来
    let (known, dropped) =
        bangdream::keep_available(refs, &unavailable);
    if dropped > 0 {
        tracing::warn!(
            playlist_id,
            dropped,
            "平台给不出详情,这些曲目没能进成员关系"
        );
    }

    cache::set_membership(
        &mut conn,
        account.id,
        playlist_id,
        &platform,
        &known,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    let tracks = cache::tracks_of(
        &mut conn,
        account.id,
        playlist_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok((tracks, dropped))
}

/// 把这些 id 里还缺的详情向平台要回来存下,返回平台**仍然给不出**的那些。
///
/// 分成两步问「谁还缺」是有意的:第一次问的是「要不要发请求」,第二次问的是
/// 「发完了还差谁」。合成一次的话,平台跳过的那些(下架、无权限)与从没问过的
/// 那些混在一起,分不出来。
pub(crate) async fn fill_details(
    state: &AppState,
    account: &Account,
    conn: &mut sqlx::PgConnection,
    ids: &[String],
) -> Result<HashSet<String>, Failure> {
    let platform = netease_name();

    let missing =
        cache::missing_details(conn, &platform, ids)
            .await
            .map_err(|err| error::map_error(&err))?;

    let mut catalog = state.upstream.catalog.clone();
    for chunk in missing.chunks(DETAIL_BATCH) {
        let response = catalog
            .get_tracks(bangdream::as_user(
                account,
                GetTracksRequest {
                    platform: Platform::Netease as i32,
                    track_ids: chunk.to_vec(),
                },
            ))
            .await
            .map_err(|status| fail(&status))?
            .into_inner();

        let tracks: Vec<TrackDto> = response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect();

        cache::put_details(conn, &tracks)
            .await
            .map_err(|err| error::map_error(&err))?;
    }

    Ok(cache::missing_details(conn, &platform, ids)
        .await
        .map_err(|err| error::map_error(&err))?
        .into_iter()
        .collect())
}

/// 缓存里代表网易云的那个字符串。
///
/// 走 `track_to_dto` 用的同一个函数 —— 另写一份的话,prost 生成的
/// `as_str_name()` 给的是 `PLATFORM_NETEASE`,与存进去的 `netease` 对不上,
/// 而那是运行期才炸的外键错误,编译器一声不吭。
pub(crate) fn netease_name() -> String {
    bangdream::platform_name(Platform::Netease as i32)
}

/// 歌单详情随手带回来的那一批曲目详情。
///
/// **会被平台截断**,所以它只是省往返的顺风车,不是全量 —— 歌单有多长的判据
/// 永远是 `track_refs`。差额由 `bangdream::refs_missing_from` 挑出来补拉。
pub(crate) fn detail_tracks_of(
    detail: &GetPlaylistResponse,
) -> Vec<TrackDto> {
    detail
        .tracks
        .iter()
        .cloned()
        .map(bangdream::track_to_dto)
        .collect()
}

/// 歌单详情里的成员关系。
pub(crate) fn track_refs_of(
    detail: &GetPlaylistResponse,
) -> Vec<cache::TrackRef> {
    detail
        .track_refs
        .iter()
        .map(|track| {
            cache::TrackRef::new(
                &track.id,
                Some(track.added_at_ms),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
