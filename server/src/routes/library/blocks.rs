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
use server::error::AppError;
use server::error::Failure;
use server::store::account::Account;
use server::store::playlist::TrackRef;
use server::store::{
    blocks, cache, daily, facets, liked, prefetch, radio,
};
use sqlx::PgConnection;

use crate::routes::catalog::radio::tell_radio_changed;
use crate::routes::play::archive;
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

    let purging = state.clone();
    let account_id = account.id;
    tokio::spawn(async move {
        if let Err(err) = purge(&purging, account_id).await
        {
            tracing::warn!(%err, "屏蔽的歌没清干净");
        }
    });
    Ok(Json(created))
}

/// `tracks` 里命中这个账号屏蔽规则的那些。缓存里没详情的判不了,不算命中。
pub(crate) async fn hits(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &[TrackRef],
) -> Result<Vec<TrackRef>, AppError> {
    let rules = blocks::list(conn, account_id).await?;
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let mut hit = Vec::new();
    let ids: Vec<String> = tracks
        .iter()
        .map(|track| track.track_id.clone())
        .collect();
    let Some(platform) =
        tracks.first().map(|t| &t.platform)
    else {
        return Ok(hit);
    };
    let mut details =
        cache::details_of(conn, platform, &ids).await?;
    facets::fill(conn, account_id, &mut details).await?;
    let all: Vec<String> =
        details.iter().map(|t| t.id.clone()).collect();
    blocks::drop_hits(&rules, &mut details);
    for id in all {
        if !details.iter().any(|track| track.id == id) {
            hit.push(TrackRef {
                platform: platform.clone(),
                track_id: id,
            });
        }
    }
    Ok(hit)
}

/// 屏蔽在服务端生效到底(ADR 0037):命中的解除红心、从电台与日推拿掉、不再排预取、
/// 桶里那份删掉。规则解除后这些歌不会自己回来。
pub(crate) async fn purge(
    state: &AppState,
    account_id: i64,
) -> Result<(), String> {
    let mut conn = state
        .pool
        .acquire()
        .await
        .map_err(|err| err.to_string())?;
    let conn = &mut *conn;
    let text = |err: AppError| format!("{err:?}");

    let mut seen = liked::refs(conn, account_id)
        .await
        .map_err(text)?;
    let lists = radio::list_all(conn, account_id)
        .await
        .map_err(text)?;
    seen.extend(lists);
    seen.sort_by(|a, b| a.track_id.cmp(&b.track_id));
    seen.dedup();

    let blocked = hits(conn, account_id, &seen)
        .await
        .map_err(text)?;
    for track in &blocked {
        liked::set(conn, account_id, track, false)
            .await
            .map_err(text)?;
        radio::remove(conn, account_id, track)
            .await
            .map_err(text)?;
        daily::remove(conn, account_id, track)
            .await
            .map_err(text)?;
        prefetch::cancel(conn, track)
            .await
            .map_err(text)?;
        archive::drop_blocked(state, conn, track).await?;
    }
    if !blocked.is_empty() {
        tell_radio_changed(state, account_id);
    }
    Ok(())
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
