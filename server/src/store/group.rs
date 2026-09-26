//! 组的全局播放状态落库(#142,`migrations/0009_play_groups.sql`)。
//!
//! 只做行与 [`Group`] 之间的翻译。规则在 `crate::syncplay::group::timeline`,
//! 什么时候读、锁、写在 `crate::syncplay::group`。

use contract::LoopModeDto;
use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::queue::Tx;
use crate::syncplay::group::timeline::{Group, Now};

/// 一行的原样形状。
type Row = (
    i64,
    Vec<String>,
    Vec<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    bool,
    i64,
    i64,
    bool,
    String,
    Vec<i64>,
);

const COLUMNS: &str = "version, members, outputs, queue_id, revision,
     entry_id, playing, position_us, anchor_wall_us, shuffled,
     loop_mode, play_order";

/// 读一个账号的组。从来没有过组时是 `None`。
pub async fn load(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Option<Group>, AppError> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM play_groups WHERE account_id = $1"
    ))
    .bind(account_id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(from_row))
}

/// 读并锁住一个账号的组,直到事务结束。没有行就先插一行空的再锁 ——
/// 两条并发的意图因此总在同一行上排队,不会各建一个组。
pub async fn lock(
    tx: &mut Tx<'_>,
    account_id: i64,
) -> Result<Group, AppError> {
    sqlx::query(
        "INSERT INTO play_groups (account_id, version)
         VALUES ($1, 0) ON CONFLICT (account_id) DO NOTHING",
    )
    .bind(account_id)
    .execute(&mut **tx)
    .await?;
    let row: Row = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM play_groups
         WHERE account_id = $1 FOR UPDATE"
    ))
    .bind(account_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(from_row(row))
}

/// 写回。`boundary` 是这一首放完的挂钟时刻(只在播放时有),续播任务按它挑组。
pub async fn save(
    tx: &mut Tx<'_>,
    account_id: i64,
    group: &Group,
    boundary: Option<i64>,
) -> Result<(), AppError> {
    let now = group.now.as_ref();
    sqlx::query(
        "UPDATE play_groups SET
             version = $2, members = $3, outputs = $4,
             queue_id = $5, revision = $6, entry_id = $7,
             playing = $8, position_us = $9, anchor_wall_us = $10,
             boundary_wall_us = $11, shuffled = $12,
             loop_mode = $13, play_order = $14
         WHERE account_id = $1",
    )
    .bind(account_id)
    .bind(group.version)
    .bind(&group.members)
    .bind(&group.outputs)
    .bind(now.map(|now| now.queue_id))
    .bind(now.map(|now| now.revision))
    .bind(now.map(|now| now.entry_id))
    .bind(now.is_some_and(|now| now.playing))
    .bind(now.map_or(0, |now| now.position_us as i64))
    .bind(now.map_or(0, |now| now.anchor_wall_us))
    .bind(boundary)
    .bind(now.is_some_and(|now| now.shuffled))
    .bind(loop_text(
        now.map_or(LoopModeDto::Off, |now| now.loop_mode),
    ))
    .bind(now.map_or(Vec::new(), |now| now.order.clone()))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 在放、而这一首在挂钟 `at` 之前已经放完的那些组。
pub async fn due(
    conn: &mut PgConnection,
    at: i64,
) -> Result<Vec<i64>, AppError> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT account_id FROM play_groups
         WHERE playing AND boundary_wall_us <= $1",
    )
    .bind(at)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

fn from_row(row: Row) -> Group {
    let (
        version,
        members,
        outputs,
        queue_id,
        revision,
        entry_id,
        playing,
        position_us,
        anchor_wall_us,
        shuffled,
        loop_mode,
        order,
    ) = row;
    let now = match (queue_id, revision, entry_id) {
        (
            Some(queue_id),
            Some(revision),
            Some(entry_id),
        ) => Some(Now {
            queue_id,
            revision,
            entry_id,
            playing,
            position_us: position_us.max(0) as u64,
            anchor_wall_us,
            shuffled,
            loop_mode: loop_mode_of(&loop_mode),
            order,
        }),
        _ => None,
    };
    Group {
        version,
        members,
        outputs,
        now,
    }
}

fn loop_text(mode: LoopModeDto) -> &'static str {
    match mode {
        LoopModeDto::Off => "off",
        LoopModeDto::All => "all",
        LoopModeDto::One => "one",
    }
}

fn loop_mode_of(text: &str) -> LoopModeDto {
    match text {
        "all" => LoopModeDto::All,
        "one" => LoopModeDto::One,
        _ => LoopModeDto::Off,
    }
}
