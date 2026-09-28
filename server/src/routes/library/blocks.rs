//! 屏蔽规则的读与写(#161)。

use axum::{
    Json,
    extract::{Path, State},
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

/// `GET /blocks` —— 这个账号的全部屏蔽规则。
pub(crate) async fn list_blocks(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<BlockRulesDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let rules = blocks::list(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(BlockRulesDto { rules }))
}

/// `POST /blocks` —— 建一条屏蔽规则(同一条已存在就交回它)。
pub(crate) async fn create_block(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<NewBlockRuleDto>,
) -> Result<Json<BlockRuleDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let created = blocks::create(
        &mut conn,
        account.id,
        body.kind,
        &body.value,
        body.label.as_deref(),
    )
    .await
    .map_err(|err| error::map_error(&err))?;

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
