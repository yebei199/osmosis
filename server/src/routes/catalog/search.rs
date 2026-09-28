//! 搜索与发现:曲目、歌手、歌单,以及每日推荐。

use axum::{
    Json,
    extract::{Path, Query, State},
};
use contract::{
    ArtistSearchDto, PlaylistSearchDto, SearchDto,
    TrackDto, TracksDto,
};
use serde::Deserialize;

use server::bangdream::{
    self,
    proto::{
        GetArtistRequest, GetDailyRecommendationsRequest,
        Platform, SearchArtistsRequest,
        SearchPlaylistsRequest, SearchTracksRequest,
    },
};
use server::error::Failure;
use server::store::account::Account;
use server::store::cache;
use server::store::daily as daily_picks;
use server::store::playlist::TrackRef;

use crate::routes::library::with_facets;
use crate::routes::play::prefetch;
use crate::{AppState, fail};

/// 搜索默认返回条数。
pub(crate) const DEFAULT_SEARCH_LIMIT: i32 = 30;

/// 三条搜索路由共用的查询参数。
#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    /// 关键词。
    q: String,
    /// 每页条数,不给按 [`DEFAULT_SEARCH_LIMIT`]。
    limit: Option<i32>,
    /// 偏移量,不给从 0 开始。翻页由客户端自行推进。
    offset: Option<i32>,
}

/// `GET /search/tracks?q=紅蓮華` —— 搜歌。
pub(crate) async fn search_tracks(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchDto>, Failure> {
    let mut catalog = state.upstream.catalog.clone();
    let response = catalog
        .search_tracks(bangdream::as_user(
            &account,
            SearchTracksRequest {
                platform: Platform::Netease as i32,
                keyword: query.q,
                limit: query
                    .limit
                    .unwrap_or(DEFAULT_SEARCH_LIMIT),
                offset: query.offset.unwrap_or_default(),
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
    remember_details(&state, &tracks).await;

    Ok(Json(SearchDto {
        tracks,
        has_more: response.has_more,
    }))
}

/// `GET /search/artists?q=beyond` —— 搜歌手。
pub(crate) async fn search_artists(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ArtistSearchDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .search_artists(bangdream::as_user(
            &account,
            SearchArtistsRequest {
                platform: Platform::Netease as i32,
                keyword: query.q,
                limit: query
                    .limit
                    .unwrap_or(DEFAULT_SEARCH_LIMIT),
                offset: query.offset.unwrap_or_default(),
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(ArtistSearchDto {
        artists: response
            .artists
            .into_iter()
            .map(bangdream::artist_to_dto)
            .collect(),
        has_more: response.has_more,
    }))
}

/// `GET /artists/{id}/tracks` —— 某个歌手的热门曲目。
///
/// 搜索结果里的歌手点下去要能听到东西,否则那一页只是一串名字。
///
/// 上游一次给完整曲目,不像歌单那样只给标识 —— 因此不经过缓存:没有要补的详情,
/// 而这批歌是**平台此刻认为的热门**,存下来只会让它停在过去某一天。
pub(crate) async fn artist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<Json<TracksDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_artist(bangdream::as_user(
            &account,
            GetArtistRequest {
                platform: Platform::Netease as i32,
                artist_id: id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    let tracks: Vec<_> = response
        .hot_tracks
        .into_iter()
        .map(bangdream::track_to_dto)
        .collect();
    // 只写详情、不写成员关系:专辑与歌词标记挂在详情那一行上(#160)
    remember_details(&state, &tracks).await;

    Ok(Json(
        with_facets(
            &state,
            account.id,
            TracksDto {
                tracks,
                unavailable: 0,
            },
        )
        .await,
    ))
}

/// `GET /search/playlists?q=华语` —— 搜歌单。
///
/// 只搜平台的。本地歌单数量小、已经在客户端手上,过滤是界面的事,
/// 为它多跑一趟服务端没有意义。
pub(crate) async fn search_playlists(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SearchQuery>,
) -> Result<Json<PlaylistSearchDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .search_playlists(bangdream::as_user(
            &account,
            SearchPlaylistsRequest {
                platform: Platform::Netease as i32,
                keyword: query.q,
                limit: query
                    .limit
                    .unwrap_or(DEFAULT_SEARCH_LIMIT),
                offset: query.offset.unwrap_or_default(),
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(PlaylistSearchDto {
        // 搜索结果里不会有红心歌单(那是账号自己的),照直翻就行
        playlists: response
            .playlists
            .into_iter()
            .map(bangdream::playlist_to_dto)
            .collect(),
        has_more: response.has_more,
    }))
}

/// `GET /daily` —— 今日推荐。
///
/// 上游直接给完整曲目,不像 [`liked`] 那样只给标识。当天的这批排进预取队列
/// (#147):取到就是日推刷新了,已在桶里的入队时就跳过。详情顺手写进缓存(#156)。
pub(crate) async fn daily(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TracksDto>, Failure> {
    let mut discover = state.upstream.discover.clone();
    let response = discover
        .get_daily_recommendations(bangdream::as_user(
            &account,
            GetDailyRecommendationsRequest {
                platform: Platform::Netease as i32,
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
    let refs: Vec<TrackRef> = tracks
        .iter()
        .map(|track| TrackRef {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
        })
        .collect();
    remember_details(&state, &tracks).await;
    remember_daily(&state, account.id, &refs).await;
    prefetch::enqueue(&state, account.id, &refs).await;

    Ok(Json(
        with_facets(
            &state,
            account.id,
            TracksDto {
                tracks,
                unavailable: 0,
            },
        )
        .await,
    ))
}

/// 把列表里出现的曲目详情写进 `platform_tracks`(#156):分类视图要的专辑、
/// 歌词标记都挂在那一行上,只有歌单路径写的话,日推与搜索里的歌就没有。
/// 写不上只写日志,列表照样交出去。
pub(crate) async fn remember_details(
    state: &AppState,
    tracks: &[TrackDto],
) {
    let written = match state.pool.acquire().await {
        Ok(mut conn) => {
            cache::put_details(&mut conn, tracks)
                .await
                .map_err(|err| format!("{err:?}"))
        }
        Err(err) => Err(err.to_string()),
    };
    if let Err(err) = written {
        tracing::warn!(%err, "曲目详情写不进缓存");
    }
}

/// 记下这个账号当天的日推:保留规则要知道哪几首在里面(#147)。
/// 记不上只写日志,日推照样交出去。
async fn remember_daily(
    state: &AppState,
    account_id: i64,
    tracks: &[TrackRef],
) {
    let remembered = match state.pool.acquire().await {
        Ok(mut conn) => daily_picks::replace(
            &mut conn, account_id, tracks,
        )
        .await
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    if let Err(err) = remembered {
        tracing::warn!(%err, "记不下当天的日推");
    }
}

#[cfg(test)]
mod tests;
