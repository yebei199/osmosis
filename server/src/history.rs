//! 播放事件与最近播放。
//!
//! 事件流只增不改:起播即追加一条,不补记听了多久 —— 补记要靠客户端在退出或切歌时
//! 再发一次,而崩溃与断网时那一条就丢了(见 `docs/adr/0016`)。
//!
//! 「最近播放」「最常听」这类东西都是**查询时**的聚合,不存在独立的统计表。
//! 口径想改就改,因为原始事件都在。

use sqlx::PgConnection;

use crate::error::AppError;
use crate::playlist::TrackRef;

/// 记一次起播。
pub async fn record(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO play_events (account_id, platform, track_id)
         VALUES ($1, $2, $3)",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .execute(conn)
    .await?;

    Ok(())
}

/// 最近播放的曲目标识,最近的在前。
///
/// **同一首歌只出现一次**,位置取它最后一次被播放的时刻。不去重的话,
/// 一次单曲循环就能把整张列表填满,而那张列表存在的意义正是"我最近都听了什么"。
///
/// 去重发生在这里而不是写入时:事件流那边一条不少,统计口径因此还改得动。
///
/// 排序用主键而不是 `played_at`:主键是严格递增的插入顺序,不依赖时钟 ——
/// 同一个事务里插入的几条 `now()` 完全相同,按时间排会得到任意顺序。
/// `played_at` 是数据,不是排序依据。
pub async fn recent(
    conn: &mut PgConnection,
    account_id: i64,
    limit: i64,
) -> Result<Vec<TrackRef>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT platform, track_id FROM (
             SELECT DISTINCT ON (platform, track_id)
                    platform, track_id, id
             FROM play_events
             WHERE account_id = $1
             ORDER BY platform, track_id, id DESC
         ) AS latest
         ORDER BY id DESC
         LIMIT $2",
    )
    .bind(account_id)
    .bind(limit)
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

/// 收听统计。全部查询时聚合,不存统计表(见模块头)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningStats {
    /// 本月起播了多少次。**不是时长**:事件流不记听了多久。
    pub month_plays: i64,
    /// 一共听过多少首不同的歌。
    pub distinct_tracks: i64,
    /// 连续在听的天数:从最近一个有播放的日子往回数,断一天即止。
    /// 今天还没听不清零,到昨天为止仍算连着;断更超过一天读作 0。
    pub streak_days: i64,
}

/// 聚合一个账号的收听统计。
///
/// 日界按 UTC 切:服务端不知道用户在哪个时区。跨着午夜听歌的人
/// 会在边界上差一天,代价可接受,比在契约里传时区便宜得多。
pub async fn stats(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<ListeningStats, AppError> {
    let (month_plays, distinct_tracks): (i64, i64) =
        sqlx::query_as(
            "SELECT
                 count(*) FILTER (WHERE played_at >= date_trunc('month', now())),
                 count(DISTINCT (platform, track_id))
             FROM play_events
             WHERE account_id = $1",
        )
        .bind(account_id)
        .fetch_one(&mut *conn)
        .await?;

    // 连续段:第 n 个日子(按新到旧数)恰好比最新日子早 n 天,才还连着。
    // 断档之后的日子永远追不上这个等式,不会误入。
    let (streak_days,): (i64,) = sqlx::query_as(
        "WITH days AS (
             SELECT DISTINCT (played_at AT TIME ZONE 'UTC')::date AS d
             FROM play_events WHERE account_id = $1
         ),
         latest AS (SELECT max(d) AS max_d FROM days),
         runs AS (
             SELECT d, row_number() OVER (ORDER BY d DESC) - 1 AS off
             FROM days
         )
         SELECT COALESCE((
             SELECT count(*) FROM runs, latest
             WHERE runs.d = latest.max_d - runs.off::int
               AND latest.max_d >= (now() AT TIME ZONE 'UTC')::date - 1
         ), 0)",
    )
    .bind(account_id)
    .fetch_one(&mut *conn)
    .await?;

    Ok(ListeningStats {
        month_plays,
        distinct_tracks,
        streak_days,
    })
}

/// 常听歌手,按出现在事件流里的次数排,次数相同按名字稳定排。
///
/// 名字来自 `platform_tracks` 缓存:缓存里没有详情的曲目不进榜单,
/// 缓存随浏览补齐,统计跟着变准 —— 不为榜单单独去平台抓详情。
pub async fn top_artists(
    conn: &mut PgConnection,
    account_id: i64,
    limit: i64,
) -> Result<Vec<(String, i64)>, AppError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT artist, count(*) AS plays
         FROM play_events AS pe
         JOIN platform_tracks AS pt
           ON pt.platform = pe.platform
          AND pt.track_id = pe.track_id,
         LATERAL unnest(pt.artists) AS artist
         WHERE pe.account_id = $1
         GROUP BY artist
         ORDER BY plays DESC, artist
         LIMIT $2",
    )
    .bind(account_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;

    Ok(rows)
}
