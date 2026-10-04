//! 屏蔽规则的读与写(#161)。

use contract::{
    BlockKind, BlockRuleDto, BlockRulesDto, NewBlockRuleDto,
};

use crate::url::{block_url, blocks_url};
use crate::{ApiError, platform};

/// `GET /blocks` —— 这个账号的全部屏蔽规则。
pub async fn blocks() -> Result<BlockRulesDto, ApiError> {
    platform::get_json(format!(
        "{}?song_rules=true",
        blocks_url()
    ))
    .await
}

/// `POST /blocks` —— 建一条屏蔽规则(同一条已存在就交回它)。
pub async fn create_block(
    kind: BlockKind,
    value: &str,
    label: &str,
) -> Result<BlockRuleDto, ApiError> {
    create_dislike(NewBlockRuleDto {
        disliked_track: None,
        kind,
        value: value.to_owned(),
        label: Some(label.to_owned()),
    })
    .await
}

/// `DELETE /blocks/{id}` —— 删掉一条规则。
pub async fn delete_block(
    id: &str,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        reqwest::Method::DELETE,
        block_url(id),
        None,
    )
    .await
}

/// 保存理由；只有太难听了携带 disliked_track，与规则同事务提交。
pub async fn create_dislike(
    body: NewBlockRuleDto,
) -> Result<BlockRuleDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        blocks_url(),
        Some(body),
    )
    .await
}
