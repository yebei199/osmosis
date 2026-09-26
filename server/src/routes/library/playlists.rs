//! 歌单的读与写。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{PlaylistDto, PlaylistsDto, TracksDto};
use serde::Deserialize;

use server::bangdream::{
    self,
    proto::{GetPlaylistRequest, Platform},
};
use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::cache;
use server::store::liked;
use server::store::playlist::{self, TrackRef};

use crate::routes::catalog::catalog_cache::{
    cached_tracks, detail_tracks_of, fill_details,
    netease_name, store_first, track_refs_of,
};
use crate::routes::play::prefetch;
use crate::{AppState, conn, fail};

/// `GET /playlists` —— 我们自己的歌单:置顶的「我的喜欢」,其后是本地歌单。
///
/// 网易云歌单(含收藏的)不再列出(`docs/adr/0033`),这一条因此一次都不问上游。
pub(crate) async fn playlists(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<PlaylistsDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let liked_count = liked::count(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;
    let local = playlist::list(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(PlaylistsDto {
        playlists: playlist::merged(liked_count, local),
    }))
}

/// `GET /playlists/platform/{id}/tracks` —— 平台歌单的曲目。
///
/// 歌单页不再列平台歌单,但搜出来的歌单点进去仍走这条(`docs/adr/0033`)。
///
/// 上游只给全量标识不给曲目:平台返回的曲目列表会被截断,标识列表不会
/// (见 bang-dream 的 `docs/adr/0003`)。详情因此在这一层备齐。
pub(crate) async fn platform_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<Json<TracksDto>, Failure> {
    let playlist_id = id.clone();
    store_first(&state, &account, &id, |state, account| {
        fetch_platform_playlist(state, account, playlist_id)
    })
    .await
    .map(Json)
}

/// 平台歌单的回源路径:取成员关系,回填缓存。
async fn fetch_platform_playlist(
    state: AppState,
    account: Account,
    id: String,
) -> Result<TracksDto, Failure> {
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

    Ok(TracksDto {
        tracks,
        unavailable,
    })
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
/// 自家只存标识,曲目详情向平台缓存借。
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

/// `POST /playlists/{id}/tracks` —— 往本地歌单加曲目。加进来的排进预取队列(#147)。
pub(crate) async fn add_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<TracksBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;
    let refs = body.refs();

    playlist::add_tracks(&mut conn, account.id, id, &refs)
        .await
        .map_err(|err| error::map_error(&err))?;
    drop(conn);
    prefetch::enqueue(&state, account.id, &refs).await;

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

#[cfg(test)]
mod tests;
