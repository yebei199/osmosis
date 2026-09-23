//! 歌单的读与写。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{PlaylistDto, PlaylistsDto, TracksDto};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;
use tracing::Instrument;

use server::bangdream::{
    self, UpstreamChannel,
    proto::{
        GetAccountStatusRequest, GetPlaylistRequest,
        ListLikedTracksRequest, ListUserPlaylistsRequest,
        Platform,
        library_service_client::LibraryServiceClient,
    },
};
use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::cache;
use server::store::playlist::{self, TrackRef};

use crate::routes::catalog::catalog_cache::{
    FRESH_WAIT, FetchedAt, MAX_AGE, REFRESH_EVERY, age_of,
    cached_tracks, detail_tracks_of, fill_details,
    netease_name, store_first, track_refs_of,
};
use crate::{AppState, conn, fail};

/// `GET /playlists` —— 两个来源合成的一张歌单列表。
///
/// 平台歌单直读上游、不镜像;本地歌单读自家的库;「我喜欢的」置顶,它就是
/// 平台的红心列表(见 `docs/adr/0016`)。
///
/// 上游要不到平台歌单时**不整个失败**:本地那半与红心仍然有用,把它们一起
/// 扣下等于让网易云的一次抖动把用户自己的歌单也弄没了。平台那半因此先答
/// 内存里上一次的那份、后台回源(见 [`platform_half`]),本地那半照旧直读库。
pub(crate) async fn playlists(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<PlaylistsDto>, Failure> {
    let (platform, liked_count) =
        platform_half(&state, &account).await;

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

/// 平台那半:歌单列表与红心数。
type PlatformHalf = (Vec<PlaylistDto>, i32);

/// 每个账号平台那半最近一次完整回源的结果,和那一刻。
///
/// 只在内存里,与 `catalog_cache::Freshness` 同一个理由:落库要加迁移。重启后
/// 第一次打开最多等 [`FRESH_WAIT`],等不到就只给本地那半 —— 与上游失败时同一个
/// 降级,回源在后台跑完,下一次就有。
#[derive(Clone, Default)]
pub(crate) struct PlatformLists(
    Arc<Mutex<HashMap<i64, (FetchedAt, PlatformHalf)>>>,
);

impl PlatformLists {
    fn lock(
        &self,
    ) -> MutexGuard<
        '_,
        HashMap<i64, (FetchedAt, PlatformHalf)>,
    > {
        // 锁里只有 HashMap 的一次读写,不会在持锁时 panic;真毒化了也照用
        self.0.lock().unwrap_or_else(|err| err.into_inner())
    }

    pub(crate) fn get(
        &self,
        account_id: i64,
    ) -> Option<(FetchedAt, PlatformHalf)> {
        self.lock().get(&account_id).cloned()
    }

    fn record(&self, account_id: i64, half: PlatformHalf) {
        self.lock().insert(
            account_id,
            (Some(Instant::now()), half),
        );
    }

    /// 这边改了平台那半(收藏、点心):下一次打开当作过期,等得到就给新的。
    ///
    /// 不是删掉:删了的话生产上等不到回源时只剩本地那半,比旧的那份更糟。
    pub(crate) fn outdate(&self, account_id: i64) {
        if let Some((at, _)) =
            self.lock().get_mut(&account_id)
        {
            *at = None;
        }
    }

    /// 把记录往回拨 `by`,测试用来模拟「过了这么久」。
    #[cfg(test)]
    pub(crate) fn age(
        &self,
        account_id: i64,
        by: Duration,
    ) {
        if let Some((at, _)) =
            self.lock().get_mut(&account_id)
        {
            *at = at.and_then(|at| at.checked_sub(by));
        }
    }
}

/// 平台那半,按 `catalog_cache::store_first` 同一套时机取。
///
/// 与它的差别只在「没有」时:这里不是只能等,而是最多等 [`FRESH_WAIT`],
/// 等不到给空 —— 本地那半不该陪着上游一起超时。
async fn platform_half(
    state: &AppState,
    account: &Account,
) -> PlatformHalf {
    let cached = state.platform_lists.get(account.id);
    if let Some((at, half)) = &cached
        && age_of(*at) < REFRESH_EVERY
    {
        return half.clone();
    }

    // 独立任务:客户端等不及断开,这次回源照样跑完并记下
    let job = tokio::spawn(
        fetch_platform_half(state.clone(), account.clone())
            .in_current_span(),
    );
    match cached {
        Some((at, half)) if age_of(at) < MAX_AGE => half,
        cached => {
            match tokio::time::timeout(FRESH_WAIT, job)
                .await
            {
                Ok(Ok(Some(half))) => half,
                _ => cached
                    .map(|(_, half)| half)
                    .unwrap_or_default(),
            }
        }
    }
}

/// 平台那半的回源:问账号,再取歌单列表与红心数。完整拿到才记下。
///
/// 问账号失败给 `None`(本次只给本地那半,与上游失败同一个降级);列表或红心数
/// 有一样没取到,结果照用但不记下 —— 记下的话一次抖动会被当成新的那份用一整天。
async fn fetch_platform_half(
    state: AppState,
    account: Account,
) -> Option<PlatformHalf> {
    let mut auth = state.upstream.auth.clone();
    let mut library = state.upstream.library.clone();

    let netease_account = match auth
        .get_account_status(bangdream::as_user(
            &account,
            GetAccountStatusRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(status) => {
            tracing::warn!(%status, "问账号失败,只给本地那半");
            return None;
        }
    };

    // 没绑网易云是**状态**不是错误:本地歌单照常给,列表里就是少了平台那部分
    let (half, complete) = if netease_account.logged_in {
        platform_playlists(
            &mut library,
            &account,
            &netease_account.user_id,
        )
        .await
    } else {
        ((Vec::new(), 0), true)
    };
    if complete {
        state
            .platform_lists
            .record(account.id, half.clone());
    }

    Some(half)
}

/// 取平台那半:歌单列表与红心数,以及两样是不是都取到了。任一步失败都只记
/// 一笔日志、当作空 —— 见 [`playlists`] 顶上那条理由。
pub(crate) async fn platform_playlists(
    library: &mut LibraryServiceClient<UpstreamChannel>,
    account: &Account,
    netease_user_id: &str,
) -> (PlatformHalf, bool) {
    let mut complete = true;
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
            complete = false;
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
            complete = false;
            0
        }
    };

    (
        (
            bangdream::platform_playlists_to_dto(lists),
            liked_count,
        ),
        complete,
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

#[cfg(test)]
mod tests;
