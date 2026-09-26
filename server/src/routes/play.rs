//! 播放与下载:同一条上游源的两种交付方式。
//!
//! [`play`] 交出一条客户端自己去取的临时直链;[`download`](download::download)
//! 把字节从上游拉过来、必要时转成 mp3 再交出去。三条路各要各的档位
//! (`docs/adr/0034`):播放要最高,缓存要无损,下载要 320k。

use std::time::Instant;

use axum::{
    Json,
    extract::{Path, State},
};
use contract::PlaySourceDto;

use server::bangdream::{
    self,
    proto::{GetPlaySourceRequest, Platform},
};
use server::error::Failure;
use server::quality::{Tier, netease};
use server::store::account::Account;

use crate::{AppState, fail};

pub(crate) mod archive;
pub(crate) mod download;
pub(crate) mod links;

#[cfg(test)]
mod tests;

/// 现取时要的档位:音源能给的最好那一档(#147)。
const PLAY_TIER: Tier = Tier::HIGHEST;

/// `GET /play/{track_id}` —— 取一条临时直链。
///
/// 桶里有无损就给对象存储的签名链接,不再找网易云(#126、#147);没有、或对象
/// 存储此刻不可用,就向上游按「最高」现取。上游的直链带签名会过期,只在剩余
/// 有效期还够放完整首时复用上一次拿到的那条(#139,见 [`links`])。
/// 两条路都在响应里带上实际音质。
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

    let level = netease::level_of(PLAY_TIER) as i32;
    let key = (account.id, track_id, level);
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
                level,
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
