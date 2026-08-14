//! 播放历史的上报与回读。

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use contract::{
    PlayedDto, StatsDto, TopArtistDto, TracksDto,
};

use server::account::Account;
use server::bangdream::{
    self,
    proto::{GetTracksRequest, Platform},
};
use server::error;
use server::error::Failure;
use server::history;
use server::playlist::TrackRef;

use super::likes::PageQuery;
use crate::{AppState, conn, fail};

/// 「最近播放」默认给多少首。
///
// ponytail: 一屏够看就行,客户端要更多可以自己传 limit。
pub(crate) const DEFAULT_RECENT_LIMIT: i64 = 50;

/// `POST /played` —— 报告一次起播。
///
/// 客户端在**声音真的出来之后**才发,不是按下播放键就发:取直链可能失败,
/// 那时并没有发生一次播放。
pub(crate) async fn record_play(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<PlayedDto>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    history::record(
        &mut conn,
        account.id,
        &TrackRef {
            platform: body.platform,
            track_id: body.track_id,
        },
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /recent` —— 最近播放,曲目详情由上游补全。
///
/// 与 [`liked`]、[`playlist_tracks`] 同一个套路:自家只存标识。
pub(crate) async fn recent(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<PageQuery>,
) -> Result<Json<TracksDto>, Failure> {
    // 大得离谱的 limit 退回默认值,而不是报错:那是客户端的笔误,不是攻击
    let limit = query
        .limit
        .and_then(|limit| i64::try_from(limit).ok())
        .unwrap_or(DEFAULT_RECENT_LIMIT);

    let mut conn = conn(&state.pool).await?;
    let refs =
        history::recent(&mut conn, account.id, limit)
            .await
            .map_err(|err| error::map_error(&err))?;

    if refs.is_empty() {
        return Ok(Json(TracksDto {
            tracks: Vec::new(),
            unavailable: 0,
        }));
    }

    // 目前只有网易云一个平台,与 playlist_tracks 同一处待办
    let ids: Vec<String> = refs
        .into_iter()
        .map(|track| track.track_id)
        .collect();

    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_tracks(bangdream::as_user(
            &account,
            GetTracksRequest {
                platform: Platform::Netease as i32,
                track_ids: ids,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(TracksDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
        unavailable: 0,
    }))
}

/// `GET /stats`:收听统计,从播放事件流查询时聚合(见 `history` 模块)。
pub(crate) async fn stats(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<StatsDto>, Failure> {
    /// 常听歌手取几个。设计稿的画像卡是五条(docs/design.md「页面清单」)。
    const TOP_ARTISTS: i64 = 5;

    let mut conn = conn(&state.pool).await?;
    let listening = history::stats(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;
    let top = history::top_artists(
        &mut conn,
        account.id,
        TOP_ARTISTS,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    // i64 → u32:负数不可能(count 出来的),溢出等于四十亿次起播,截断即可。
    let clamp =
        |n: i64| u32::try_from(n).unwrap_or(u32::MAX);

    Ok(Json(StatsDto {
        username: account.username,
        month_plays: clamp(listening.month_plays),
        distinct_tracks: clamp(listening.distinct_tracks),
        streak_days: clamp(listening.streak_days),
        top_artists: top
            .into_iter()
            .map(|(name, plays)| TopArtistDto {
                name,
                plays: clamp(plays),
            })
            .collect(),
    }))
}
