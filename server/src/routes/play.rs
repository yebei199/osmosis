//! 播放与下载:同一条上游源的两种交付方式。
//!
//! [`play`] 交出一条客户端自己去取的临时直链;[`download`](download::download)
//! 把字节从上游拉过来、必要时转成 mp3 再交出去。两者共用 [`PLAY_QUALITY`] ——
//! 各自取各自的档位的话,「听到的」和「存下的」会是两个版本。

use std::time::Instant;

use axum::{
    Json,
    extract::{Path, State},
};
use contract::PlaySourceDto;

use server::bangdream::{
    self,
    proto::{GetPlaySourceRequest, Platform, QualityLevel},
};
use server::error::Failure;
use server::store::account::Account;

use crate::{AppState, fail};

pub(crate) mod archive;
pub(crate) mod download;
pub(crate) mod links;

#[cfg(test)]
mod tests;

/// 取播放地址时请求的音质档位。
///
// ponytail: 先写死。做到音质选择时再提成查询参数 —— 现在没有任何界面能选它。
pub(crate) const PLAY_QUALITY: QualityLevel =
    QualityLevel::High;

/// `GET /play/{track_id}` —— 取一条临时直链。
///
/// 存过的歌给对象存储的签名链接,不再找网易云(#126);没存过、或对象存储
/// 此刻不可用,就向上游要。上游的直链带签名会过期,只在剩余有效期还够放完
/// 整首时复用上一次拿到的那条(#139,见 [`links`])。
pub(crate) async fn play(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<Json<PlaySourceDto>, Failure> {
    if let Some(stored) =
        archive::stored_source(&state, &track_id).await
    {
        return Ok(Json(stored));
    }

    let key = (account.id, track_id, PLAY_QUALITY as i32);
    if let Some(cached) =
        state.links.get(&key, Instant::now())
    {
        return Ok(Json(cached));
    }

    let issued_at = Instant::now();
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_play_source(bangdream::as_user(
            &account,
            GetPlaySourceRequest {
                platform: Platform::Netease as i32,
                track_id: key.1.clone(),
                level: PLAY_QUALITY as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // source 缺席意味着上游认为拿到了、却没给内容 —— 当成上游失败,不静默返回空。
    let source = response.source.ok_or_else(|| {
        fail(&tonic::Status::internal("上游没有返回播放源"))
    })?;

    let dto = bangdream::play_source_to_dto(source.clone());
    state.links.record(
        key,
        &source,
        dto.clone(),
        issued_at,
    );

    Ok(Json(dto))
}
