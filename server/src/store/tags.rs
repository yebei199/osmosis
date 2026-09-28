//! 账号级自定义标签(#158):真相在自家 Postgres。
//!
//! 每个函数都收 `account_id` 并把它写进 WHERE:归属检查不是单独一步,
//! 而是查询的一部分,与 [`crate::store::playlist`] 同一条规矩。

use contract::{TagDto, TagSource};
use sqlx::PgConnection;

use crate::error::AppError;

/// 一个标签。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub source: TagSource,
}

impl Tag {
    pub fn to_dto(&self) -> TagDto {
        TagDto {
            id: self.id.to_string(),
            name: self.name.clone(),
            source: self.source,
        }
    }
}

/// 建一个标签,手打的一律是 [`TagSource::Manual`]。
///
/// 同名标签已存在就直接复用它,不报错、也不建第二条 ——
/// 用户在两首不同的歌上打同一个新名字,两次的意图是同一个标签。
pub async fn create(
    conn: &mut PgConnection,
    account_id: i64,
    name: &str,
) -> Result<Tag, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("标签名不能为空"));
    }

    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO tags (account_id, name, source)
         VALUES ($1, $2, 'manual')
         ON CONFLICT (account_id, name) DO UPDATE SET name = tags.name
         RETURNING id",
    )
    .bind(account_id)
    .bind(name)
    .fetch_one(conn)
    .await?;

    Ok(Tag {
        id,
        name: name.to_owned(),
        source: TagSource::Manual,
    })
}

/// 列出这个账号的所有标签。
pub async fn list(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<Tag>, AppError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, name, source FROM tags
         WHERE account_id = $1
         ORDER BY id",
    )
    .bind(account_id)
    .fetch_all(conn)
    .await?;

    rows.into_iter().map(row_to_tag).collect()
}

/// 改名。不是自己的标签一律 [`AppError::NotFound`]。
pub async fn rename(
    conn: &mut PgConnection,
    account_id: i64,
    tag_id: i64,
    name: &str,
) -> Result<(), AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("标签名不能为空"));
    }

    let done = sqlx::query(
        "UPDATE tags SET name = $3
         WHERE id = $2 AND account_id = $1",
    )
    .bind(account_id)
    .bind(tag_id)
    .bind(name)
    .execute(conn)
    .await?;

    found(done.rows_affected())
}

/// 删除。`track_tags` 里的关联由外键的 ON DELETE CASCADE 一并清空。
pub async fn delete(
    conn: &mut PgConnection,
    account_id: i64,
    tag_id: i64,
) -> Result<(), AppError> {
    let done = sqlx::query(
        "DELETE FROM tags WHERE id = $2 AND account_id = $1",
    )
    .bind(account_id)
    .bind(tag_id)
    .execute(conn)
    .await?;

    found(done.rows_affected())
}

/// 一首歌打了哪些标签。
pub async fn tags_of_track(
    conn: &mut PgConnection,
    account_id: i64,
    platform: &str,
    track_id: &str,
) -> Result<Vec<Tag>, AppError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT t.id, t.name, t.source
         FROM track_tags tt
         JOIN tags t ON t.id = tt.tag_id
         WHERE tt.account_id = $1 AND tt.platform = $2 AND tt.track_id = $3
         ORDER BY t.id",
    )
    .bind(account_id)
    .bind(platform)
    .bind(track_id)
    .fetch_all(conn)
    .await?;

    rows.into_iter().map(row_to_tag).collect()
}

/// 给一首歌打上一个标签。标签不是自己的、或不存在,一律 [`AppError::NotFound`]。
/// 已经打过的再打一次直接幂等。
pub async fn tag_track(
    conn: &mut PgConnection,
    account_id: i64,
    tag_id: i64,
    platform: &str,
    track_id: &str,
) -> Result<(), AppError> {
    own(&mut *conn, account_id, tag_id).await?;

    sqlx::query(
        "INSERT INTO track_tags (account_id, platform, track_id, tag_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(platform)
    .bind(track_id)
    .bind(tag_id)
    .execute(conn)
    .await?;

    Ok(())
}

/// 从一首歌上摘掉一个标签。标签不存在也当作已经摘掉,不报错 ——
/// 与打标签同一条幂等规矩。
pub async fn untag_track(
    conn: &mut PgConnection,
    account_id: i64,
    tag_id: i64,
    platform: &str,
    track_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM track_tags
         WHERE account_id = $1 AND tag_id = $2 AND platform = $3 AND track_id = $4",
    )
    .bind(account_id)
    .bind(tag_id)
    .bind(platform)
    .bind(track_id)
    .execute(conn)
    .await?;

    Ok(())
}

fn row_to_tag(
    (id, name, source): (i64, String, String),
) -> Result<Tag, AppError> {
    let source = match source.as_str() {
        "manual" => TagSource::Manual,
        "model" => TagSource::Model,
        _ => TagSource::Manual,
    };
    Ok(Tag { id, name, source })
}

/// 确认这个标签归这个账号,不归就是"不存在"。不回 403,道理与
/// [`crate::store::playlist`] 的 `own` 一致。
async fn own(
    conn: &mut PgConnection,
    account_id: i64,
    tag_id: i64,
) -> Result<(), AppError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM tags WHERE id = $2 AND account_id = $1",
    )
    .bind(account_id)
    .bind(tag_id)
    .fetch_optional(conn)
    .await?;

    row.map(|_| ()).ok_or(AppError::NotFound)
}

/// 写操作影响了 0 行就是"不存在"。
fn found(rows: u64) -> Result<(), AppError> {
    if rows == 0 {
        Err(AppError::NotFound)
    } else {
        Ok(())
    }
}
