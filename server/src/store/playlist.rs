//! 本地歌单:真相在自家 Postgres。
//!
//! 系统自带的「我的喜欢」也住在这两张表里,由 [`crate::store::liked`] 管;这里的
//! 列表、改名、删除都认不出它(`system IS NOT NULL`),它由 [`merged`] 单独置顶。
//!
//! 每个函数都收 `account_id` 并把它写进 WHERE:归属检查不是单独一步,
//! 而是查询的一部分 —— 分成两步就总有一天会漏掉第一步。

use contract::{PlaylistDto, PlaylistSource};
use sqlx::PgConnection;

use crate::error::AppError;

/// 一个本地歌单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPlaylist {
    pub id: i64,
    pub name: String,
    pub track_count: i32,
}

impl LocalPlaylist {
    /// 翻成契约里的形状,`source` 一律是 `Local` —— 能走到这里的都是本地歌单。
    pub fn to_dto(&self) -> PlaylistDto {
        PlaylistDto {
            source: PlaylistSource::Local,
            id: self.id.to_string(),
            name: self.name.clone(),
            // 本地歌单还没有封面。真要有的话该取头几首的封面拼一张,那是界面的事。
            cover: None,
            track_count: self.track_count,
        }
    }
}

/// 一首歌的身份:`(平台, 平台内 id)`,缺一不可。
///
/// 不做跨平台匹配,所以同一首歌在两个平台上是两条(见 bang-dream 的 `docs/adr/0003`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRef {
    pub platform: String,
    pub track_id: String,
}

/// 建一个本地歌单。
pub async fn create(
    conn: &mut PgConnection,
    account_id: i64,
    name: &str,
) -> Result<LocalPlaylist, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("歌单名不能为空"));
    }

    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO local_playlists (account_id, name)
         VALUES ($1, $2) RETURNING id",
    )
    .bind(account_id)
    .bind(name)
    .fetch_one(conn)
    .await?;

    Ok(LocalPlaylist {
        id,
        name: name.to_owned(),
        track_count: 0,
    })
}

/// 列出这个账号的本地歌单,新建的在后。系统歌单(「我的喜欢」)不在其中,
/// 它由 [`merged`] 单独置顶。
pub async fn list(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<LocalPlaylist>, AppError> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT p.id, p.name, count(t.track_id)
         FROM local_playlists p
         LEFT JOIN local_playlist_tracks t ON t.playlist_id = p.id
         WHERE p.account_id = $1 AND p.system IS NULL
         GROUP BY p.id
         ORDER BY p.id",
    )
    .bind(account_id)
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, count)| LocalPlaylist {
            id,
            name,
            // 一个人的歌单不会有二十亿首,截断是纯防御
            track_count: count
                .try_into()
                .unwrap_or(i32::MAX),
        })
        .collect())
}

/// 改名。不是自己的歌单、以及系统歌单,一律 [`AppError::NotFound`]。
pub async fn rename(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: i64,
    name: &str,
) -> Result<(), AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("歌单名不能为空"));
    }

    let done = sqlx::query(
        "UPDATE local_playlists SET name = $3
         WHERE id = $2 AND account_id = $1 AND system IS NULL",
    )
    .bind(account_id)
    .bind(playlist_id)
    .bind(name)
    .execute(conn)
    .await?;

    found(done.rows_affected())
}

/// 删除。曲目关联由外键的 ON DELETE CASCADE 一并带走。系统歌单删不掉,
/// 与别人的歌单一样按不存在回答。
pub async fn delete(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: i64,
) -> Result<(), AppError> {
    let done = sqlx::query(
        "DELETE FROM local_playlists
         WHERE id = $2 AND account_id = $1 AND system IS NULL",
    )
    .bind(account_id)
    .bind(playlist_id)
    .execute(conn)
    .await?;

    found(done.rows_affected())
}

/// 取歌单里的曲目标识,按加入顺序。
///
/// 只给标识不给详情 —— 与 bang-dream 对歌单的做法一致:详情的真相在平台,
/// 调用方拿着标识去 `GetTracks` 补全。
pub async fn tracks(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: i64,
) -> Result<Vec<TrackRef>, AppError> {
    own(&mut *conn, account_id, playlist_id).await?;

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT platform, track_id FROM local_playlist_tracks
         WHERE playlist_id = $1
         ORDER BY position",
    )
    .bind(playlist_id)
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

/// 往歌单里加曲目,按给定顺序排在末尾。
///
/// 已经在里面的跳过而**不报错**:用户连点两下「加入」,两次的意图是同一个。
pub async fn add_tracks(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: i64,
    tracks: &[TrackRef],
) -> Result<(), AppError> {
    own(&mut *conn, account_id, playlist_id).await?;

    let (mut position,): (i64,) = sqlx::query_as(
        "SELECT coalesce(max(position), -1) + 1
         FROM local_playlist_tracks WHERE playlist_id = $1",
    )
    .bind(playlist_id)
    .fetch_one(&mut *conn)
    .await?;

    for track in tracks {
        sqlx::query(
            "INSERT INTO local_playlist_tracks
                 (playlist_id, platform, track_id, position)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
        )
        .bind(playlist_id)
        .bind(&track.platform)
        .bind(&track.track_id)
        .bind(position)
        .execute(&mut *conn)
        .await?;

        position += 1;
    }

    Ok(())
}

/// 从歌单里移掉曲目。其余曲目的位置不动,因此顺序不变。
pub async fn remove_tracks(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: i64,
    tracks: &[TrackRef],
) -> Result<(), AppError> {
    own(&mut *conn, account_id, playlist_id).await?;

    for track in tracks {
        sqlx::query(
            "DELETE FROM local_playlist_tracks
             WHERE playlist_id = $1 AND platform = $2 AND track_id = $3",
        )
        .bind(playlist_id)
        .bind(&track.platform)
        .bind(&track.track_id)
        .execute(&mut *conn)
        .await?;
    }

    Ok(())
}

/// 歌单页的那一张列表:「我的喜欢」置顶,其后是本地歌单。
///
/// 只有我们自己的歌单,网易云歌单不再列出(`docs/adr/0033`)。「我的喜欢」是
/// 系统歌单,由 `store::liked` 管,契约里用 `Liked` 这个来源标出来:客户端据此
/// 不给它删除与改名键,并按 `/liked` 读它的曲目。
pub fn merged(
    liked_count: i32,
    local: Vec<LocalPlaylist>,
) -> Vec<PlaylistDto> {
    let liked = PlaylistDto {
        source: PlaylistSource::Liked,
        // 客户端按来源走 /liked,不用这个 id
        id: String::new(),
        name: "我的喜欢".to_owned(),
        cover: None,
        track_count: liked_count,
    };

    std::iter::once(liked)
        .chain(local.iter().map(LocalPlaylist::to_dto))
        .collect()
}

/// 确认这个歌单归这个账号,不归就是"不存在"。
///
/// 不回 403:那等于确认了这个 id 存在,把别人有多少个歌单这件事泄露出去。
async fn own(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: i64,
) -> Result<(), AppError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM local_playlists WHERE id = $2 AND account_id = $1",
    )
    .bind(account_id)
    .bind(playlist_id)
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
