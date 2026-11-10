//! 播放历史的上报与回读。

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use contract::{
    ListenedDto, PlayedAckDto, PlayedDto, StatsDto,
    TopArtistDto, TrackIdsDto, TracksDto,
};

use server::bangdream::{
    self,
    proto::{GetTracksRequest, Platform},
};
use server::error;
use server::error::Failure;
use server::heard;
use server::store::account::Account;
use server::store::history::{self, PlayRef};
use server::store::playlist::TrackRef;

use super::for_account;
use super::likes::PageQuery;
use crate::routes::catalog::radio::spawn_top_up;
use crate::routes::catalog::search::remember_details;
use crate::routes::play::archive;
use crate::{AppState, conn, fail};

/// 「最近播放」默认给多少首。
///
// ponytail: 一屏够看就行,客户端要更多可以自己传 limit。
pub(crate) const DEFAULT_RECENT_LIMIT: i64 = 50;

/// `POST /played` —— 报告一次起播。响应体带这一行的 id,补记听了多久时要用。
///
/// 客户端在**声音真的出来之后**才发,不是按下播放键就发:取直链可能失败,
/// 那时并没有发生一次播放。
pub(crate) async fn record_play(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<PlayedDto>,
) -> Result<Json<PlayedAckDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let track = TrackRef {
        platform: body.platform,
        track_id: body.track_id,
    };

    let id = history::record(&mut conn, account.id, &track)
        .await
        .map_err(|err| error::map_error(&err))?;

    // 起播不算听过:电台与日推的「已听过」等补记听了多久时再判(见 `server::heard`)。
    // 声音已经出来了,这首值得存:后台去,不等它(#126)
    archive::spawn_keep(&state, account, track);

    Ok(Json(PlayedAckDto { id }))
}

/// `PATCH /played/{id}/listened` —— 补记这一次播放听了多久(#157),听够了就算听过。
///
/// 切歌、播完、停止时客户端各发一次;进程被杀时这条永远不会发生,那一行
/// 的 `listened_ms`/`duration_ms` 留 NULL,算不出完播也算不出跳过,也不算听过,接受。
/// 电台或日推里的一首因此听过了,各台把它挪进「已听过」(#186),存量少了就补。
pub(crate) async fn report_listened(
    State(state): State<AppState>,
    account: Account,
    Path(play_event_id): Path<i64>,
    Json(body): Json<ListenedDto>,
) -> Result<StatusCode, Failure> {
    let listed = heard::settle(
        &state.pool,
        &state.roster,
        account.id,
        &PlayRef::Event(play_event_id),
        body.listened_ms,
        body.duration_ms,
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    if listed {
        spawn_top_up(&state, account);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /heard` —— 给的这几首里,这个账号听过的那些(用户 2026-11-10 的口径)。
///
/// 日推、电台起播的那一批放着放着,里面的歌可能在别的设备上听过了;自动续播
/// 到它之前问一句,听过的跳过。只有网易云一个平台,与 `/liked/ids` 同一处待办。
pub(crate) async fn heard_among(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<TrackIdsDto>,
) -> Result<Json<TrackIdsDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let track_ids = history::heard_among(
        &mut conn,
        account.id,
        "netease",
        &body.track_ids,
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    Ok(Json(TrackIdsDto { track_ids }))
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
            hidden: 0,
        }));
    }

    // 目前只有网易云一个平台,与 playlist_tracks 同一处待办
    let ids: Vec<String> = refs
        .into_iter()
        .map(|track| track.track_id)
        .collect();

    let mut catalog = state.upstream.catalog.clone();
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

    let tracks: Vec<_> = response
        .tracks
        .into_iter()
        .map(bangdream::track_to_dto)
        .collect();
    // 专辑与歌词标记挂在详情那一行上,最近播放也得写(#160)
    remember_details(&state, &tracks).await;

    Ok(Json(
        for_account(
            &state,
            account.id,
            TracksDto {
                tracks,
                unavailable: 0,
                hidden: 0,
            },
        )
        .await,
    ))
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
