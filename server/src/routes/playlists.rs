//! 歌单的读与写。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{PlaylistDto, PlaylistsDto, TracksDto};
use serde::Deserialize;
use tonic::transport::Channel;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{
        GetAccountStatusRequest, GetPlaylistRequest,
        ListLikedTracksRequest, ListUserPlaylistsRequest,
        Platform,
        library_service_client::LibraryServiceClient,
    },
};
use server::error::Failure;
use server::playlist::{self, TrackRef};
use server::{cache, error};

use super::catalog_cache::{
    cached_tracks, detail_tracks_of, fill_details,
    netease_name, track_refs_of,
};
use crate::{AppState, conn, fail};

/// `GET /playlists` —— 两个来源合成的一张歌单列表。
///
/// 平台歌单直读上游、不镜像;本地歌单读自家的库;「我喜欢的」置顶,它就是
/// 平台的红心列表(见 `docs/adr/0016`)。
///
/// 上游要不到平台歌单时**不整个失败**:本地那半与红心仍然有用,把它们一起
/// 扣下等于让网易云的一次抖动把用户自己的歌单也弄没了。
pub(crate) async fn playlists(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<PlaylistsDto>, Failure> {
    let mut auth = state.upstream.auth;
    let mut library = state.upstream.library;

    let netease_account = auth
        .get_account_status(bangdream::as_user(
            &account,
            GetAccountStatusRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    let (platform, liked_count) =
        if netease_account.logged_in {
            platform_playlists(
                &mut library,
                &account,
                &netease_account.user_id,
            )
            .await
        } else {
            // 没绑网易云是**状态**不是错误:本地歌单照常给,列表里就是少了平台那部分
            (Vec::new(), 0)
        };

    let mut conn = conn(&state.pool).await?;
    let local = playlist::list(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(PlaylistsDto {
        playlists: playlist::merged(
            liked_count,
            platform,
            local,
        ),
    }))
}

/// 取平台那半:歌单列表与红心数。任一步失败都只记一笔日志、当作空 ——
/// 见 [`playlists`] 顶上那条理由。
pub(crate) async fn platform_playlists(
    library: &mut LibraryServiceClient<Channel>,
    account: &Account,
    netease_user_id: &str,
) -> (Vec<PlaylistDto>, i32) {
    let lists = match library
        .list_user_playlists(bangdream::as_user(
            account,
            ListUserPlaylistsRequest {
                platform: Platform::Netease as i32,
                user_id: netease_user_id.to_owned(),
                limit: 0,
                offset: 0,
            },
        ))
        .await
    {
        Ok(response) => response.into_inner().playlists,
        Err(status) => {
            tracing::warn!(%status, "取平台歌单失败,只给本地那半");
            Vec::new()
        }
    };

    let liked_count = match library
        .list_liked_tracks(bangdream::as_user(
            account,
            ListLikedTracksRequest {
                platform: Platform::Netease as i32,
                user_id: netease_user_id.to_owned(),
            },
        ))
        .await
    {
        Ok(response) => response
            .into_inner()
            .track_ids
            .len()
            .try_into()
            .unwrap_or(i32::MAX),
        Err(status) => {
            tracing::warn!(%status, "取红心列表失败,数目按 0 显示");
            0
        }
    };

    (
        bangdream::platform_playlists_to_dto(lists),
        liked_count,
    )
}

/// `GET /playlists/platform/{id}/tracks` —— 平台歌单的曲目。
///
/// 上游只给全量标识不给曲目:平台返回的曲目列表会被截断,标识列表不会
/// (见 bang-dream 的 `docs/adr/0003`)。详情因此在这一层备齐,与 [`liked`] 同一个套路。
pub(crate) async fn platform_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<Json<TracksDto>, Failure> {
    let mut library = state.upstream.library.clone();
    let detail = library
        .get_playlist(bangdream::as_user(
            &account,
            GetPlaylistRequest {
                platform: Platform::Netease as i32,
                playlist_id: id.clone(),
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    let (tracks, unavailable) = cached_tracks(
        &state,
        &account,
        &id,
        &track_refs_of(&detail),
        &detail_tracks_of(&detail),
    )
    .await?;

    Ok(Json(TracksDto {
        tracks,
        unavailable,
    }))
}

/// `POST /playlists` 的请求体。
#[derive(Deserialize)]
pub(crate) struct NameBody {
    name: String,
}

/// `POST /playlists` —— 建一个本地歌单。
pub(crate) async fn create_playlist(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<NameBody>,
) -> Result<Json<PlaylistDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let created =
        playlist::create(&mut conn, account.id, &body.name)
            .await
            .map_err(|err| error::map_error(&err))?;

    Ok(Json(created.to_dto()))
}

/// `PATCH /playlists/{id}` —— 给本地歌单改名。
pub(crate) async fn rename_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<NameBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::rename(&mut conn, account.id, id, &body.name)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /playlists/{id}` —— 删掉本地歌单。
pub(crate) async fn delete_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::delete(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /playlists/{id}/tracks` —— 本地歌单的曲目,详情由上游补全。
///
/// 与 [`liked`] 同一个套路:自家只存标识,曲目的真相在平台。
pub(crate) async fn playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Result<Json<TracksDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let refs = playlist::tracks(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    // 目前只有网易云一个平台。多平台之后这里要按 platform 分组各问各的 ——
    // 留到真有第二个平台时再改。
    let ids: Vec<String> = refs
        .iter()
        .map(|track| track.track_id.clone())
        .collect();

    // 只借详情那一半:本地歌单的成员关系真相在自家表里,不进缓存。
    // 进了的话,它的整数 id 会和平台歌单的字符串 id 撞在同一列上。
    fill_details(&state, &account, &mut conn, &ids).await?;

    let tracks =
        cache::details_of(&mut conn, &netease_name(), &ids)
            .await
            .map_err(|err| error::map_error(&err))?;

    // 这条路不经过缓存的剔除,没有"平台给不出详情"这回事
    Ok(Json(TracksDto {
        tracks,
        unavailable: 0,
    }))
}

/// 增删曲目的请求体。
#[derive(Deserialize)]
pub(crate) struct TracksBody {
    /// 曲目标识。身份是 `(平台, 平台内 id)`,所以平台不能省。
    tracks: Vec<TrackRefDto>,
}

#[derive(Deserialize)]
pub(crate) struct TrackRefDto {
    platform: String,
    id: String,
}

impl TracksBody {
    fn refs(&self) -> Vec<TrackRef> {
        self.tracks
            .iter()
            .map(|track| TrackRef {
                platform: track.platform.clone(),
                track_id: track.id.clone(),
            })
            .collect()
    }
}

/// `POST /playlists/{id}/tracks` —— 往本地歌单加曲目。
pub(crate) async fn add_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<TracksBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::add_tracks(
        &mut conn,
        account.id,
        id,
        &body.refs(),
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /playlists/{id}/tracks` —— 从本地歌单移掉曲目。
pub(crate) async fn remove_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<TracksBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::remove_tracks(
        &mut conn,
        account.id,
        id,
        &body.refs(),
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}
