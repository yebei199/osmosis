//! 每个账号一份共享的电台歌单(#186):成员与加入次序。
//!
//! 详情不在这里,借 [`super::cache`];听没听过也不存,读的时候看
//! [`history::heard_sql`]。听过的行留着,「已听过」要摆它们。

use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::history;
use crate::store::liked;
use crate::store::playlist::TrackRef;

/// 歌单里的一首。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub track: TrackRef,
    /// 这个账号听过它(见 [`history::is_heard`])。
    pub heard: bool,
}

/// 把 `tracks` 按给定次序追加到队尾。已经在里面的跳过。返回新加了几首。
pub async fn append(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &[TrackRef],
) -> Result<u64, AppError> {
    let (platforms, ids): (Vec<&str>, Vec<&str>) = tracks
        .iter()
        .map(|track| {
            (
                track.platform.as_str(),
                track.track_id.as_str(),
            )
        })
        .unzip();
    let done = sqlx::query(
        "INSERT INTO radio_tracks (account_id, platform, track_id)
         SELECT $1, t.platform, t.track_id
         FROM unnest($2::text[], $3::text[])
              WITH ORDINALITY AS t (platform, track_id, n)
         ORDER BY t.n
         ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
    .execute(conn)
    .await?;

    Ok(done.rows_affected())
}

/// 整份歌单,不含这个账号红心的:红心的已经永久存着,不必再摆。先加的在前。
pub async fn list(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<Entry>, AppError> {
    let rows: Vec<(String, String, bool)> =
        sqlx::query_as(&format!(
            "SELECT r.platform, r.track_id, {heard}
             FROM radio_tracks AS r
             WHERE r.account_id = $1 AND NOT {liked}
             ORDER BY r.id",
            heard = history::heard_sql("r"),
            liked = liked::of_account_sql("r"),
        ))
        .bind(account_id)
        .fetch_all(conn)
        .await?;

    Ok(rows
        .into_iter()
        .map(|(platform, track_id, heard)| Entry {
            track: TrackRef { platform, track_id },
            heard,
        })
        .collect())
}

/// 这个账号电台与日推合起来还有几首没听的,同一首只算一次。
pub async fn unheard_total(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<usize, AppError> {
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM (
             SELECT account_id, platform, track_id FROM radio_tracks
             UNION
             SELECT account_id, platform, track_id FROM daily_picks
         ) AS m
         WHERE m.account_id = $1 AND NOT {liked} AND NOT {heard}",
        heard = history::heard_sql("m"),
        liked = liked::of_account_sql("m"),
    ))
    .bind(account_id)
    .fetch_one(conn)
    .await?;

    Ok(usize::try_from(total).unwrap_or(usize::MAX))
}

/// 这个账号电台与日推里的全部曲目,不管听没听过、红没红心。
pub async fn list_all(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<TrackRef>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT platform, track_id FROM radio_tracks
         WHERE account_id = $1
         UNION
         SELECT platform, track_id FROM daily_picks
         WHERE account_id = $1",
    )
    .bind(account_id)
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

/// 用过电台或日推的账号:后台补歌只替它们跑。
pub async fn listeners(
    conn: &mut PgConnection,
) -> Result<Vec<i64>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT account_id FROM radio_tracks
         UNION
         SELECT account_id FROM daily_picks",
    )
    .fetch_all(conn)
    .await?)
}

/// 从这个账号的电台歌单里拿掉一首。
pub async fn remove(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM radio_tracks
         WHERE account_id = $1 AND platform = $2 AND track_id = $3",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .execute(conn)
    .await?;

    Ok(())
}

/// 这首在不在这个账号的电台歌单里。
pub async fn holds(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM radio_tracks
             WHERE account_id = $1 AND platform = $2 AND track_id = $3
         )",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .fetch_one(conn)
    .await?)
}
