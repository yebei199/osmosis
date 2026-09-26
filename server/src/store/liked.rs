//! 「我的喜欢」:系统自带的本地歌单,红心的真相(#146,`docs/adr/0033`)。
//!
//! 建在本地歌单那两张表上,靠 `local_playlists.system = 'liked'` 认。每个账号
//! 一个,第一次用到时建;普通歌单的列表、改名、删除都认不出它(见 [`super::playlist`])。
//!
//! 网易云的红心只在 [`import`] 时出场:导一次,之后以这里为准,不写回平台。

use sqlx::{Connection, PgConnection};

use crate::error::AppError;
use crate::store::cache;
use crate::store::playlist::TrackRef;

/// `local_playlists.system` 里「我的喜欢」的取值。
pub const SYSTEM: &str = "liked";

/// 这个账号的「我的喜欢」的 id,没有就建一个。
///
/// 两次并发的「没有就建」撞在迁移 0011 那条部分唯一索引上,后到的什么都不插,
/// 随后的 SELECT 读到先到的那一个。
pub async fn ensure(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<i64, AppError> {
    sqlx::query(
        "INSERT INTO local_playlists (account_id, name, system)
         VALUES ($1, '我的喜欢', $2)
         ON CONFLICT (account_id, system) WHERE system IS NOT NULL
         DO NOTHING",
    )
    .bind(account_id)
    .bind(SYSTEM)
    .execute(&mut *conn)
    .await?;

    let (id,): (i64,) = sqlx::query_as(
        "SELECT id FROM local_playlists
         WHERE account_id = $1 AND system = $2",
    )
    .bind(account_id)
    .bind(SYSTEM)
    .fetch_one(conn)
    .await?;

    Ok(id)
}

/// 这个账号的「我的喜欢」建过没有。没建过的第一次用到时从平台导入一次(#147)。
pub async fn exists(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM local_playlists
             WHERE account_id = $1 AND system = $2
         )",
    )
    .bind(account_id)
    .bind(SYSTEM)
    .fetch_one(conn)
    .await?)
}

/// 全部红心,最近加入的在最前(`docs/adr/0021`)。
///
/// 加入时刻相同或没有的,按 position 倒排 —— 导入时平台排在前面的拿到的
/// position 大,于是它们仍按平台给的先后出来。
pub async fn refs(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<TrackRef>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.platform, t.track_id
         FROM local_playlist_tracks t
         JOIN local_playlists p ON p.id = t.playlist_id
         WHERE p.account_id = $1 AND p.system = $2
         ORDER BY t.added_at DESC NULLS LAST, t.position DESC",
    )
    .bind(account_id)
    .bind(SYSTEM)
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(platform, track_id)| TrackRef {
            platform,
            track_id,
        })
        .collect())
}

/// 红心有几首。歌单页那一行的数目。
pub async fn count(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<i32, AppError> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*)
         FROM local_playlist_tracks t
         JOIN local_playlists p ON p.id = t.playlist_id
         WHERE p.account_id = $1 AND p.system = $2",
    )
    .bind(account_id)
    .bind(SYSTEM)
    .fetch_one(conn)
    .await?;

    Ok(n.try_into().unwrap_or(i32::MAX))
}

/// 点心或取消。重复点心、取消一首没有的,都不报错:两次的意图是同一个。
pub async fn set(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
    liked: bool,
) -> Result<(), AppError> {
    let playlist_id =
        ensure(&mut *conn, account_id).await?;

    let statement = if liked {
        "INSERT INTO local_playlist_tracks
             (playlist_id, platform, track_id, position, added_at)
         SELECT $1, $2, $3, coalesce(max(position), -1) + 1, now()
         FROM local_playlist_tracks WHERE playlist_id = $1
         ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM local_playlist_tracks
         WHERE playlist_id = $1 AND platform = $2 AND track_id = $3"
    };
    sqlx::query(statement)
        .bind(playlist_id)
        .bind(&track.platform)
        .bind(&track.track_id)
        .execute(conn)
        .await?;

    Ok(())
}

/// 把平台的红心并进来,返回新加了几首。只补没有的,不删这边已有的。
///
/// `refs` 是平台给的次序(最近加的在前),带着平台的加入时刻。倒着插:
/// 平台排在前面的拿到更大的 position,与 [`refs`] 的倒排对上。
/// 整批一个事务,中途失败不会留下半份。
pub async fn import(
    conn: &mut PgConnection,
    account_id: i64,
    platform: &str,
    refs: &[cache::TrackRef],
) -> Result<u64, AppError> {
    let mut tx = conn.begin().await?;
    let playlist_id = ensure(&mut tx, account_id).await?;

    let (mut position,): (i64,) = sqlx::query_as(
        "SELECT coalesce(max(position), -1) + 1
         FROM local_playlist_tracks WHERE playlist_id = $1",
    )
    .bind(playlist_id)
    .fetch_one(&mut *tx)
    .await?;

    let mut added = 0;
    for track in refs.iter().rev() {
        added += sqlx::query(
            "INSERT INTO local_playlist_tracks
                 (playlist_id, platform, track_id, position, added_at)
             VALUES ($1, $2, $3, $4,
                     to_timestamp($5::bigint / 1000.0))
             ON CONFLICT DO NOTHING",
        )
        .bind(playlist_id)
        .bind(platform)
        .bind(&track.id)
        .bind(position)
        .bind(track.added_at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        position += 1;
    }

    tx.commit().await?;
    Ok(added)
}
