//! 搜索与发现:曲目、歌手、歌单,以及每日推荐。

use axum::{
    Json,
    extract::{Path, Query, State},
};
use contract::{
    ArtistSearchDto, PlaylistSearchDto, SearchDto,
    TracksDto,
};
use serde::Deserialize;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{
        GetArtistRequest, GetDailyRecommendationsRequest,
        Platform, SearchArtistsRequest,
        SearchPlaylistsRequest, SearchTracksRequest,
    },
};
use server::error::Failure;

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
    let mut catalog = state.upstream.catalog;
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

    Ok(Json(SearchDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
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

    Ok(Json(TracksDto {
        tracks: response
            .hot_tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
        unavailable: 0,
    }))
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
/// 上游直接给完整曲目,不像 [`liked`] 那样只给标识。
pub(crate) async fn daily(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TracksDto>, Failure> {
    let mut discover = state.upstream.discover;
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

    Ok(Json(TracksDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
        unavailable: 0,
    }))
}
