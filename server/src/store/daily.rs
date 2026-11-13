//! 每个账号累积的每日推荐(ADR 0037)。
//!
//! 和电台歌单同一套逻辑:推过的一直留着,没听的摆在主列表,听过的归「已听过」,
//! 红心的不再摆。新的一批在前,批内按平台给的次序。

use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::history;
use crate::store::liked;
use crate::store::playlist::TrackRef;
use crate::store::radio::Entry;

/// 当前推荐日。网易云北京时间 6 点换批,即 UTC+2 的日期。
const TODAY: &str =
    "(now() AT TIME ZONE 'UTC' + interval '2 hours')::date";

/// 把 `tracks` 记成今天推的。以前推过的挪到今天这批,并按今天这批里的次序重排:
/// 留着旧 `id` 的话,重推的歌会排在今天真正新推的前面。
pub async fn add(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &[TrackRef],
) -> Result<(), AppError> {
    let (platforms, ids): (Vec<&str>, Vec<&str>) = tracks
        .iter()
        .map(|track| {
            (
                track.platform.as_str(),
                track.track_id.as_str(),
            )
        })
        .unzip();

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO daily_picks (account_id, platform, track_id, day)
         SELECT $1, platform, track_id, {TODAY}
         FROM unnest($2::text[], $3::text[]) WITH ORDINALITY
              AS t (platform, track_id, place)
         ORDER BY place
         ON CONFLICT (account_id, platform, track_id)
         DO UPDATE SET day = EXCLUDED.day, id = EXCLUDED.id
         WHERE daily_picks.day <> EXCLUDED.day"
    )))
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
    .execute(conn)
    .await?;

    Ok(())
}

/// 今天这批取过没有。取过就不必再问平台。
pub async fn has_today(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS (
             SELECT 1 FROM daily_picks
             WHERE account_id = $1 AND day = {TODAY}
         )"
    )))
    .bind(account_id)
    .fetch_one(conn)
    .await?)
}

/// 整份日推,不含这个账号红心的。新的一批在前。
pub async fn list(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<Entry>, AppError> {
    let rows: Vec<(String, String, bool, String)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT d.platform, d.track_id, {heard}, d.day::text
             FROM daily_picks AS d
             WHERE d.account_id = $1 AND NOT {liked}
             ORDER BY d.day DESC, d.id",
            heard = history::heard_sql("d"),
            liked = liked::of_account_sql("d"),
        )))
        .bind(account_id)
        .fetch_all(conn)
        .await?;

    Ok(rows
        .into_iter()
        .map(|(platform, track_id, heard, day)| Entry {
            track: TrackRef { platform, track_id },
            heard,
            picked_on: Some(day),
        })
        .collect())
}

/// 这首在不在这个账号的日推里。
pub async fn holds(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM daily_picks
             WHERE account_id = $1 AND platform = $2 AND track_id = $3
         )",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .fetch_one(conn)
    .await?)
}

/// 从这个账号的日推里拿掉一首。
pub async fn remove(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM daily_picks
         WHERE account_id = $1 AND platform = $2 AND track_id = $3",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .execute(conn)
    .await?;

    Ok(())
}
