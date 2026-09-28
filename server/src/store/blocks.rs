//! 屏蔽规则(#161):按歌手 / 标签 / 单曲,命中的歌在所有列表里隐藏。
//!
//! 每个函数都收 `account_id` 并把它写进 WHERE,与 [`crate::store::tags`]
//! 同一条规矩。规则一个账号至多几十条,匹配在内存里做,不下推进 SQL。

use contract::{BlockKind, BlockRuleDto};
use sqlx::PgConnection;

use crate::error::AppError;

/// 建一条规则。同一条已存在就直接交回它,不报错也不建第二条。
pub async fn create(
    conn: &mut PgConnection,
    account_id: i64,
    kind: BlockKind,
    value: &str,
    label: Option<&str>,
) -> Result<BlockRuleDto, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::Invalid(
            "屏蔽的对象不能为空",
        ));
    }
    let label = label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or(value);

    let (id, label): (i64, String) = sqlx::query_as(
        "INSERT INTO block_rules (account_id, kind, value, label)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (account_id, kind, value) DO UPDATE SET label = block_rules.label
         RETURNING id, label",
    )
    .bind(account_id)
    .bind(kind_name(kind))
    .bind(value)
    .bind(label)
    .fetch_one(conn)
    .await?;

    Ok(BlockRuleDto {
        id: id.to_string(),
        kind,
        value: value.to_owned(),
        label,
    })
}

/// 这个账号的全部规则,先建的在前。
pub async fn list(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<BlockRuleDto>, AppError> {
    let rows: Vec<(i64, String, String, String)> =
        sqlx::query_as(
            "SELECT id, kind, value, label FROM block_rules
             WHERE account_id = $1
             ORDER BY id",
        )
        .bind(account_id)
        .fetch_all(conn)
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|(id, kind, value, label)| {
            Some(BlockRuleDto {
                id: id.to_string(),
                kind: parse_kind(&kind)?,
                value,
                label,
            })
        })
        .collect())
}

/// 删一条规则。不是自己的一律 [`AppError::NotFound`]。
pub async fn delete(
    conn: &mut PgConnection,
    account_id: i64,
    rule_id: i64,
) -> Result<(), AppError> {
    let done = sqlx::query(
        "DELETE FROM block_rules WHERE id = $2 AND account_id = $1",
    )
    .bind(account_id)
    .bind(rule_id)
    .execute(conn)
    .await?;

    if done.rows_affected() == 0 {
        Err(AppError::NotFound)
    } else {
        Ok(())
    }
}

fn kind_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Artist => "artist",
        BlockKind::Tag => "tag",
        BlockKind::Track => "track",
    }
}

fn parse_kind(stored: &str) -> Option<BlockKind> {
    match stored {
        "artist" => Some(BlockKind::Artist),
        "tag" => Some(BlockKind::Tag),
        "track" => Some(BlockKind::Track),
        _ => None,
    }
}
