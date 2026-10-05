//! 屏蔽规则的读与写(#161)。

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use contract::{
    BlockRuleDto, BlockRulesDto, NewBlockRuleDto,
};

use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::blocks;

use crate::{AppState, conn};

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct BlockCapabilities {
    #[serde(default)]
    song_rules: bool,
}

/// `GET /blocks` —— 这个账号的全部屏蔽规则。
pub(crate) async fn list_blocks(
    State(state): State<AppState>,
    account: Account,
    Query(capabilities): Query<BlockCapabilities>,
) -> Result<Json<BlockRulesDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let mut rules = blocks::list(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    if !capabilities.song_rules {
        rules.retain(|rule| {
            matches!(
                rule.kind,
                contract::BlockKind::Artist
                    | contract::BlockKind::Tag
                    | contract::BlockKind::Track
            )
        });
        for rule in &mut rules {
            if rule.kind == contract::BlockKind::Artist
                && let Ok(artist) =
                    serde_json::from_str::<
                        contract::ArtistIdentityDto,
                    >(&rule.value)
            {
                rule.value = artist.name;
            }
        }
    }
    Ok(Json(BlockRulesDto { rules }))
}

/// `POST /blocks` —— 建一条屏蔽规则(同一条已存在就交回它)。
pub(crate) async fn create_block(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<NewBlockRuleDto>,
) -> Result<Json<BlockRuleDto>, Failure> {
    if let Some(track) = &body.disliked_track
        && (body.kind != contract::BlockKind::Song
            || track.platform.trim().is_empty()
            || track.track_id.trim().is_empty())
    {
        return Err(error::map_error(
            &server::error::AppError::Invalid(
                "点踩必须对应歌曲规则与有效曲目",
            ),
        ));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|err| error::map_error(&err.into()))?;
    let created = blocks::create(
        &mut tx,
        account.id,
        body.kind,
        &body.value,
        body.label.as_deref(),
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    if let Some(track) = body.disliked_track {
        server::store::feedback::set(
            &mut tx,
            account.id,
            &server::store::playlist::TrackRef {
                platform: track.platform,
                track_id: track.track_id,
            },
            -1,
        )
        .await
        .map_err(|err| error::map_error(&err))?;
    }
    tx.commit()
        .await
        .map_err(|err| error::map_error(&err.into()))?;

    Ok(Json(created))
}

/// `DELETE /blocks/{id}` —— 删掉一条规则,被它藏起来的歌回来。
pub(crate) async fn delete_block(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let id: i64 = id.parse().map_err(|_| {
        error::map_error(&server::error::AppError::NotFound)
    })?;
    let mut conn = conn(&state.pool).await?;

    blocks::delete(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
