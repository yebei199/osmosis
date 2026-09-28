//! 账号级自定义标签的读与写(#158)。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{TagDto, TagsDto};
use serde::Deserialize;

use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::tags;

use crate::{AppState, conn};

/// `POST /tags` 与 `PATCH /tags/{id}` 共用的请求体。
#[derive(Deserialize)]
pub(crate) struct NameBody {
    name: String,
}

fn parse_id(id: &str) -> Result<i64, Failure> {
    id.parse().map_err(|_| {
        error::map_error(&server::error::AppError::NotFound)
    })
}

/// `GET /tags` —— 这个账号的全部标签。
pub(crate) async fn list_tags(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TagsDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let found = tags::list(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(TagsDto {
        tags: found.iter().map(tags::Tag::to_dto).collect(),
    }))
}

/// `POST /tags` —— 建一个标签(同名的直接复用)。
pub(crate) async fn create_tag(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<NameBody>,
) -> Result<Json<TagDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let created =
        tags::create(&mut conn, account.id, &body.name)
            .await
            .map_err(|err| error::map_error(&err))?;

    Ok(Json(created.to_dto()))
}

/// `PATCH /tags/{id}` —— 给标签改名。
pub(crate) async fn rename_tag(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
    Json(body): Json<NameBody>,
) -> Result<StatusCode, Failure> {
    let id = parse_id(&id)?;
    let mut conn = conn(&state.pool).await?;

    tags::rename(&mut conn, account.id, id, &body.name)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /tags/{id}` —— 删掉标签,`track_tags` 里的关联级联清空。
pub(crate) async fn delete_tag(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let id = parse_id(&id)?;
    let mut conn = conn(&state.pool).await?;

    tags::delete(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /tracks/{platform}/{id}/tags` —— 一首歌打了哪些标签。
pub(crate) async fn track_tags(
    State(state): State<AppState>,
    account: Account,
    Path((platform, track_id)): Path<(String, String)>,
) -> Result<Json<TagsDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let found = tags::tags_of_track(
        &mut conn, account.id, &platform, &track_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(Json(TagsDto {
        tags: found.iter().map(tags::Tag::to_dto).collect(),
    }))
}

/// `PUT /tags/{id}/tracks/{platform}/{track_id}` —— 给一首歌打上某个标签。
pub(crate) async fn tag_track(
    State(state): State<AppState>,
    account: Account,
    Path((id, platform, track_id)): Path<(
        String,
        String,
        String,
    )>,
) -> Result<StatusCode, Failure> {
    let id = parse_id(&id)?;
    let mut conn = conn(&state.pool).await?;

    tags::tag_track(
        &mut conn, account.id, id, &platform, &track_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /tags/{id}/tracks/{platform}/{track_id}` —— 从一首歌上摘掉某个标签。
pub(crate) async fn untag_track(
    State(state): State<AppState>,
    account: Account,
    Path((id, platform, track_id)): Path<(
        String,
        String,
        String,
    )>,
) -> Result<StatusCode, Failure> {
    let id = parse_id(&id)?;
    let mut conn = conn(&state.pool).await?;

    tags::untag_track(
        &mut conn, account.id, id, &platform, &track_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
