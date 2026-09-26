//! 红心与订阅:「我的喜欢」的读写与导入,以及平台歌单的收藏开关。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{TrackIdsDto, TracksDto};
use serde::{Deserialize, Serialize};

use server::bangdream::{
    self, UpstreamChannel,
    proto::{
        GetAccountStatusRequest, GetPlaylistRequest,
        GetPlaylistResponse, ListUserPlaylistsRequest,
        Platform, SetPlaylistSubscribedRequest,
        library_service_client::LibraryServiceClient,
    },
};
use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::cache;
use server::store::liked;
use server::store::playlist::TrackRef;

use crate::routes::catalog::catalog_cache::{
    detail_tracks_of, fill_details, netease_name,
    track_refs_of,
};
use crate::routes::play::{archive, prefetch};
use crate::{AppState, conn, fail};

/// `GET /recent` 的查询参数。
///
/// 只剩 limit 一个:歌单类的路由都不再切页了,它们要的是完整的一批。
/// 最近播放不同 —— 那是一条越来越长的流水,「最近多少条」是它的固有参数。
#[derive(Deserialize)]
pub(crate) struct PageQuery {
    pub(crate) limit: Option<usize>,
}

/// `GET /liked/ids` —— 红心的全量标识,最近加入的在前。
///
/// 读自家的「我的喜欢」(`docs/adr/0033`),不问网易云:网易云没登录、连不上,
/// 界面上的心照样画得对。
pub(crate) async fn liked_ids(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TrackIdsDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let refs = liked::refs(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(TrackIdsDto {
        track_ids: refs
            .into_iter()
            .map(|track| track.track_id)
            .collect(),
    }))
}

/// `GET /liked` —— 「我的喜欢」,全量,最近加入的在前。
///
/// 成员关系读自家库;详情向缓存借,缓存里没有的才问平台(常态是一次都不问)。
/// 平台给不出详情的那首(下架、无权限)仍在「我的喜欢」里,只是这一次显示
/// 不出来,算进 `unavailable` —— 成员关系是我们的,不因为平台一时给不出就删。
pub(crate) async fn liked(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TracksDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let ids: Vec<String> =
        liked::refs(&mut conn, account.id)
            .await
            .map_err(|err| error::map_error(&err))?
            .into_iter()
            .map(|track| track.track_id)
            .collect();

    // 目前只有网易云一个平台,与本地歌单那条路同一个前提
    fill_details(&state, &account, &mut conn, &ids).await?;
    let tracks =
        cache::details_of(&mut conn, &netease_name(), &ids)
            .await
            .map_err(|err| error::map_error(&err))?;

    Ok(Json(TracksDto {
        unavailable: ids.len() - tracks.len(),
        tracks,
    }))
}

/// `POST /liked/import` 的响应:这次新加了几首,导完一共几首。
#[derive(Debug, Serialize)]
pub(crate) struct LikedImport {
    pub(crate) added: u64,
    pub(crate) total: i32,
}

/// `POST /liked/import` —— 把网易云的红心并进「我的喜欢」。
///
/// 用户定的:导一次,之后以我们的为准(#146)。这条既是那「一次」,也是之后
/// 手动的「再导一次」:只补新增、不删这边已有的,重跑不重复。界面上没有按钮,
/// 带登录 token 用 curl 打它。
///
/// 顺手把详情备进缓存,之后读「我的喜欢」就不必再问平台。平台给不出详情的
/// 照样导进来(见 [`liked`])。
pub(crate) async fn import_liked(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<LikedImport>, Failure> {
    let detail = netease_liked(&state, &account).await?;
    let refs = track_refs_of(&detail);
    let ids: Vec<String> =
        refs.iter().map(|track| track.id.clone()).collect();

    let mut conn = conn(&state.pool).await?;
    cache::put_details(
        &mut conn,
        &detail_tracks_of(&detail),
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    fill_details(&state, &account, &mut conn, &ids).await?;

    let added = liked::import(
        &mut conn,
        account.id,
        &netease_name(),
        &refs,
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    let total = liked::count(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;
    tracing::info!(added, total, "导入网易云红心");
    drop(conn);
    let imported: Vec<TrackRef> = ids
        .into_iter()
        .map(|track_id| TrackRef {
            platform: netease_name(),
            track_id,
        })
        .collect();
    prefetch::enqueue(&state, account.id, &imported).await;

    Ok(Json(LikedImport { added, total }))
}

/// 网易云红心歌单的详情:问账号 → 找红心歌单 → 取成员关系与加入时刻。
///
/// 走红心**歌单**而不是红心接口:后者返回裸数字数组,挂不住加入时刻
/// (`docs/adr/0021`)。
async fn netease_liked(
    state: &AppState,
    account: &Account,
) -> Result<GetPlaylistResponse, Failure> {
    let mut auth = state.upstream.auth.clone();
    let mut library = state.upstream.library.clone();

    let netease_account = auth
        .get_account_status(bangdream::as_user(
            account,
            GetAccountStatusRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // 未登录是**状态**不是错误(上游的 `docs/adr/0005`),但导入的目的没达成 ——
    // 当成「导进零首」的话,用户会以为网易云那边一首红心都没有
    if !netease_account.logged_in {
        return Err(fail(&tonic::Status::unauthenticated(
            "netease: 未登录",
        )));
    }

    let liked_id = liked_playlist_id(
        &mut library,
        account,
        &netease_account.user_id,
    )
    .await?;

    Ok(library
        .get_playlist(bangdream::as_user(
            account,
            GetPlaylistRequest {
                platform: Platform::Netease as i32,
                playlist_id: liked_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner())
}

/// 找出这个账号的红心歌单在平台上的 id。
///
/// 平台把红心也算作一个用户歌单,靠 `special_type` 认;上游只搬运这个值,
/// 判定归这边(见 `docs/adr/0022`)。找不到是**错误**而不是空列表 ——
/// 每个账号都有这个歌单,找不到说明上游给的列表不完整。
async fn liked_playlist_id(
    library: &mut LibraryServiceClient<UpstreamChannel>,
    account: &Account,
    netease_user_id: &str,
) -> Result<String, Failure> {
    let lists = library
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
        .map_err(|status| fail(&status))?
        .into_inner();

    bangdream::liked_playlist_id(&lists.playlists)
        .ok_or_else(|| {
            fail(&tonic::Status::not_found(
                "netease: 歌单列表里没有红心歌单",
            ))
        })
}

/// `PUT /liked/{track_id}` —— 点红心。这首排进预取队列(#147)。
pub(crate) async fn like_track(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    let track = TrackRef {
        platform: netease_name(),
        track_id: track_id.clone(),
    };
    let status =
        set_liked(&state, &account, track_id, true).await?;
    prefetch::enqueue(
        &state,
        account.id,
        std::slice::from_ref(&track),
    )
    .await;
    Ok(status)
}

/// `DELETE /liked/{track_id}` —— 取消红心。
///
/// 存进对象存储的那份从这一刻起重新数保留期(#126)。
pub(crate) async fn unlike_track(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    let status = set_liked(
        &state,
        &account,
        track_id.clone(),
        false,
    )
    .await?;
    archive::restart_clock(&state, &track_id).await;
    Ok(status)
}

/// 红心的开与关只差一个布尔值,两条路由因此共用这一段。
///
/// 只改自家的「我的喜欢」,不写回网易云(`docs/adr/0033`)。路径里只有平台内 id,
/// 目前唯一的平台是网易云。
pub(crate) async fn set_liked(
    state: &AppState,
    account: &Account,
    track_id: String,
    liked: bool,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;
    let track = TrackRef {
        platform: netease_name(),
        track_id,
    };
    liked::set(&mut conn, account.id, &track, liked)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /subscriptions/playlists/{playlist_id}` —— 收藏一个平台歌单。
pub(crate) async fn subscribe_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(playlist_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_subscribed(&state, &account, playlist_id, true)
        .await
}

/// `DELETE /subscriptions/playlists/{playlist_id}` —— 取消收藏。
pub(crate) async fn unsubscribe_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(playlist_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_subscribed(&state, &account, playlist_id, false)
        .await
}

/// 收藏的开与关同样只差一个布尔值。
///
/// 只对**平台**歌单有意义:本地歌单是自己建的,没有"收藏"这回事,
/// 它的对应操作是删除。收藏照旧写网易云;收藏的歌单不再出现在我们的
/// 歌单页上(`docs/adr/0033`)。
pub(crate) async fn set_subscribed(
    state: &AppState,
    account: &Account,
    playlist_id: String,
    subscribed: bool,
) -> Result<StatusCode, Failure> {
    let mut library = state.upstream.library.clone();

    library
        .set_playlist_subscribed(bangdream::as_user(
            account,
            SetPlaylistSubscribedRequest {
                platform: Platform::Netease as i32,
                playlist_id,
                subscribed,
            },
        ))
        .await
        .map_err(|status| fail(&status))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
