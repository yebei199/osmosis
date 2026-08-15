//! 歌词。

use axum::{
    Json,
    extract::{Path, State},
};
use contract::LyricDto;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{GetLyricRequest, Platform},
};
use server::error::Failure;

use crate::{AppState, fail};

/// `GET /lyric/{track_id}`:取一首歌的行级歌词。
///
/// 与 [`play`] 的一处刻意不同:`lyric` 缺席**不算失败**,给空行表。
/// 纯音乐与上游未收录都会走到这里,而「这首歌没有歌词」是正常状态 ——
/// 报成错误的话,客户端会把它显示成一次故障。
pub(crate) async fn lyric(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<Json<LyricDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_lyric(bangdream::as_user(
            &account,
            GetLyricRequest {
                platform: Platform::Netease as i32,
                track_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(bangdream::lyric_to_dto(
        response.lyric.unwrap_or_default(),
    )))
}
