//! 红心与订阅:全量标识、那一页曲目,以及两个开关。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{TrackIdsDto, TracksDto};
use serde::Deserialize;
use tonic::transport::Channel;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{
        GetAccountStatusRequest, GetPlaylistRequest,
        ListLikedTracksRequest, ListUserPlaylistsRequest,
        Platform, SetPlaylistSubscribedRequest,
        SetTrackLikedRequest,
        library_service_client::LibraryServiceClient,
    },
};
use server::cache;
use server::error::Failure;

use super::catalog_cache::{
    cached_tracks, detail_tracks_of, track_refs_of,
};
use crate::{AppState, fail};

/// `GET /recent` 的查询参数。
///
/// 只剩 limit 一个:歌单类的路由都不再切页了,它们要的是完整的一批。
/// 最近播放不同 —— 那是一条越来越长的流水,「最近多少条」是它的固有参数。
#[derive(Deserialize)]
pub(crate) struct PageQuery {
    pub(crate) limit: Option<usize>,
}

/// `GET /liked` —— 我喜欢的音乐,全量。
///
/// 三步:先问上游当前账号是谁,再拿它**全量**的红心标识列表,把缺详情的那些补齐。
/// 上游只给标识是刻意的(它的 `docs/adr/0003`):平台返回的曲目列表会被截断,
/// 标识列表不会。
///
/// user_id 不做缓存:重新扫码登录会换一个账号,缓存住的话红心列表会静默停在旧账号上。
/// 这是一次同机 gRPC,便宜得没必要省。
pub(crate) async fn liked_ids(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TrackIdsDto>, Failure> {
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

    // 没绑网易云是**状态**不是错误:那就是「一首红心都没有」,
    // 界面据此把所有心画成空的,而不是整页失败。
    if !netease_account.logged_in {
        return Ok(Json(TrackIdsDto {
            track_ids: Vec::new(),
        }));
    }

    let found = library
        .list_liked_tracks(bangdream::as_user(
            &account,
            ListLikedTracksRequest {
                platform: Platform::Netease as i32,
                user_id: netease_account.user_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(TrackIdsDto {
        track_ids: found.track_ids,
    }))
}

pub(crate) async fn liked(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TracksDto>, Failure> {
    let mut auth = state.upstream.auth.clone();
    let mut library = state.upstream.library.clone();

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

    // 未登录是**状态**不是错误,所以上游用 logged_in 而非错误码回答(它的 `docs/adr/0005`)。
    // 但对这个请求而言目的没达成 —— 返回空列表会被读成"一首喜欢的都没有",
    // 那是另一件事,必须区分开。
    if !netease_account.logged_in {
        return Err(fail(&tonic::Status::unauthenticated(
            "netease: 未登录",
        )));
    }

    // 走红心**歌单**而不是 /liked/ids 那条路:红心接口返回的是裸数字数组,
    // 结构上挂不住加入时间,而次序要按加入时间倒排(见 `docs/adr/0021`)。
    let liked_id = liked_playlist_id(
        &mut library,
        &account,
        &netease_account.user_id,
    )
    .await?;

    let detail = library
        .get_playlist(bangdream::as_user(
            &account,
            GetPlaylistRequest {
                platform: Platform::Netease as i32,
                playlist_id: liked_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // 不分页:红心是一整批,不是搜索结果。973 首里只看得到 50 首的话,
    // 剩下的 923 首没有任何入口 —— 界面上没有翻页,也不该有。
    let (tracks, unavailable) = cached_tracks(
        &state,
        &account,
        cache::LIKED_PLAYLIST_ID,
        &track_refs_of(&detail),
        &detail_tracks_of(&detail),
    )
    .await?;

    Ok(Json(TracksDto {
        tracks,
        unavailable,
    }))
}

/// 找出这个账号的红心歌单在平台上的 id。
///
/// 平台把红心也算作一个用户歌单,靠 `special_type` 认;上游只搬运这个值,
/// 判定归这边(见 `docs/adr/0022`)。找不到是**错误**而不是空列表 ——
/// 每个账号都有这个歌单,找不到说明上游给的列表不完整,那时回空会被读成
/// 「一首喜欢的都没有」。
pub(crate) async fn liked_playlist_id(
    library: &mut LibraryServiceClient<Channel>,
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

/// `PUT /liked/{track_id}` —— 给一首歌点红心。
pub(crate) async fn like_track(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_liked(&state, &account, track_id, true).await
}

/// `DELETE /liked/{track_id}` —— 取消红心。
pub(crate) async fn unlike_track(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_liked(&state, &account, track_id, false).await
}

/// 红心的开与关只差一个布尔值,两条路由因此共用这一段。
///
/// 「我喜欢的」就是平台的红心列表,不建本地副本(见 `docs/adr/0016`),
/// 所以这里只转发,自家库一个字都不写。
pub(crate) async fn set_liked(
    state: &AppState,
    account: &Account,
    track_id: String,
    liked: bool,
) -> Result<StatusCode, Failure> {
    let mut library = state.upstream.library.clone();

    library
        .set_track_liked(bangdream::as_user(
            account,
            SetTrackLikedRequest {
                platform: Platform::Netease as i32,
                track_id,
                liked,
            },
        ))
        .await
        .map_err(|status| fail(&status))?;

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
/// 它的对应操作是删除。
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
