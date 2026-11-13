//! 歌单视图的分组与筛选要的聚合(#160):听过几次、赞踩、喜爱度、跳过率、歌词标记、标签。
//!
//! 全部查询时从原始表聚合,不存统计表 —— 与 `history` 同一条规矩,口径想改就改。
//!
//! **跳过**的口径是 #157 定的、也告诉过用户的那一条:前 30 秒内切走(见
//! [`SKIP_WITHIN_MS`])。没记下听了多久的播放(进程被杀、老客户端)不进分母;
//! 一次都没记下时跳过率是 `None`。
//!
//! **喜爱度**(ADR 0042)的公式在 [`crate::score`];这里只把每首歌记下了听多久的
//! 那几次播放按列取成数组([`LISTENS`]),交给它在 Rust 里算 —— 公式只有一份、
//! 能单测,SQL 里不再抄一遍。还在放的那一次此刻只有检查点的累计,暂时读成没听完,
//! 放完补记后自己改正;进程被杀的那一次停在最后一个检查点,接受。

use std::collections::HashMap;

use contract::{LyricKindDto, TrackDto, TrackFacetsDto};
use sqlx::PgConnection;

use crate::error::AppError;
use crate::score::{self, Listen};
use crate::store::liked;
use crate::store::playlist::TrackRef;

/// `play_events AS pe` 按曲目分组后,算喜爱度要的三列:听了多久、时长、离现在几天。
/// 只取记下了听多久、时长已知的那些,三列按同一个次序排,下标对得上。
const LISTENS: &str = "
    coalesce(array_agg(pe.listened_ms ORDER BY pe.id) FILTER (
        WHERE pe.listened_ms IS NOT NULL AND pe.duration_ms > 0
    ), '{}') AS listened,
    coalesce(array_agg(pe.duration_ms ORDER BY pe.id) FILTER (
        WHERE pe.listened_ms IS NOT NULL AND pe.duration_ms > 0
    ), '{}') AS lengths,
    coalesce(array_agg(
        extract(epoch FROM now() - pe.played_at)::float8 / 86400
        ORDER BY pe.id
    ) FILTER (
        WHERE pe.listened_ms IS NOT NULL AND pe.duration_ms > 0
    ), '{}') AS ages";

/// [`LISTENS`] 那三列读回来的样子。
#[derive(sqlx::FromRow)]
struct Listens {
    listened: Vec<i64>,
    lengths: Vec<i64>,
    ages: Vec<f64>,
}

impl Listens {
    fn score(&self, verdict: Option<i16>) -> i16 {
        score::score(
            verdict,
            self.listened
                .iter()
                .zip(&self.lengths)
                .zip(&self.ages)
                .map(
                    |(
                        (&listened_ms, &duration_ms),
                        &age_days,
                    )| {
                        Listen {
                            listened_ms,
                            duration_ms,
                            age_days,
                        }
                    },
                ),
        )
    }
}

/// 听不到这么久就切走,算一次跳过(#157 的口径,毫秒)。全仓只此一处;
/// `test/feedback-e2e.sh skip` 按同一个数断言。
pub const SKIP_WITHIN_MS: i64 = 30_000;

/// 给这批曲目填上这个账号的聚合。一条查询,与歌单多长无关。
///
/// 覆盖原有的 `facets`:它只从这里来,别处给的都是默认值。
pub async fn fill(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &mut [TrackDto],
) -> Result<(), AppError> {
    if tracks.is_empty() {
        return Ok(());
    }
    let (platforms, ids): (Vec<&str>, Vec<&str>) = tracks
        .iter()
        .map(|track| {
            (track.platform.as_str(), track.id.as_str())
        })
        .unzip();

    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH asked AS (
             SELECT DISTINCT platform, track_id
             FROM unnest($2::text[], $3::text[]) AS a (platform, track_id)
         ),
         plays AS (
             SELECT pe.platform, pe.track_id,
                    count(*) AS plays,
                    count(*) FILTER (
                        WHERE pe.listened_ms IS NOT NULL AND pe.duration_ms > 0
                    ) AS measured,
                    count(*) FILTER (
                        WHERE pe.duration_ms > 0 AND pe.listened_ms < $4
                    ) AS skips,
                    {LISTENS}
             FROM play_events pe
             JOIN asked USING (platform, track_id)
             WHERE pe.account_id = $1
             GROUP BY pe.platform, pe.track_id
         ),
         labels AS (
             SELECT tt.platform, tt.track_id,
                    array_agg(t.name ORDER BY t.id) AS tags
             FROM track_tags tt
             JOIN asked USING (platform, track_id)
             JOIN tags t ON t.id = tt.tag_id
             WHERE tt.account_id = $1
             GROUP BY tt.platform, tt.track_id
         )
         SELECT a.platform, a.track_id, d.lyric_kind,
                coalesce(p.plays, 0) AS plays,
                coalesce(p.measured, 0) AS measured,
                coalesce(p.skips, 0) AS skips,
                f.verdict,
                coalesce(l.tags, '{{}}') AS tags,
                coalesce(p.listened, '{{}}') AS listened,
                coalesce(p.lengths, '{{}}') AS lengths,
                coalesce(p.ages, '{{}}') AS ages
         FROM asked a
         LEFT JOIN platform_tracks d USING (platform, track_id)
         LEFT JOIN plays p USING (platform, track_id)
         LEFT JOIN labels l USING (platform, track_id)
         LEFT JOIN track_feedback f
           ON f.account_id = $1
          AND f.platform = a.platform AND f.track_id = a.track_id",
    )))
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
    .bind(SKIP_WITHIN_MS)
    .fetch_all(conn)
    .await?;

    let found: HashMap<(String, String), TrackFacetsDto> =
        rows.into_iter()
            .map(|row| {
                (
                    (
                        row.platform.clone(),
                        row.track_id.clone(),
                    ),
                    row.into_facets(),
                )
            })
            .collect();
    for track in tracks {
        track.facets = found
            .get(&(
                track.platform.clone(),
                track.id.clone(),
            ))
            .cloned()
            .unwrap_or_default();
    }
    Ok(())
}

/// 这个账号喜爱度最高的至多 `limit` 首,高的在前;只要喜爱度为正的。电台补歌拿它们
/// 当心动模式的种子(ADR 0042)。
///
/// 候选只看红心与赞过、特别喜欢过的:修正碰不到 ±0.2,没表态的歌喜爱度为正也只是
/// 「一般」那一档里靠前,不该挤掉说过喜欢的。同分按曲目标识排,次序稳定。
pub async fn top_scored(
    conn: &mut PgConnection,
    account_id: i64,
    limit: usize,
) -> Result<Vec<TrackRef>, AppError> {
    let rows: Vec<Scored> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH asked AS (
             SELECT lt.platform, lt.track_id
             FROM local_playlist_tracks lt
             JOIN local_playlists lp ON lp.id = lt.playlist_id
             WHERE lp.account_id = $1 AND lp.system = $2
             UNION
             SELECT platform, track_id
             FROM track_feedback
             WHERE account_id = $1 AND verdict > 0
         ),
         plays AS (
             SELECT pe.platform, pe.track_id,
                    {LISTENS}
             FROM play_events pe
             JOIN asked USING (platform, track_id)
             WHERE pe.account_id = $1
             GROUP BY pe.platform, pe.track_id
         )
         SELECT a.platform, a.track_id, f.verdict,
                coalesce(p.listened, '{{}}') AS listened,
                coalesce(p.lengths, '{{}}') AS lengths,
                coalesce(p.ages, '{{}}') AS ages
         FROM asked a
         LEFT JOIN plays p USING (platform, track_id)
         LEFT JOIN track_feedback f
           ON f.account_id = $1
          AND f.platform = a.platform AND f.track_id = a.track_id",
    )))
    .bind(account_id)
    .bind(liked::SYSTEM)
    .fetch_all(conn)
    .await?;

    let mut scored: Vec<(i16, TrackRef)> = rows
        .into_iter()
        .map(|row| {
            (
                row.listens.score(row.verdict),
                TrackRef {
                    platform: row.platform,
                    track_id: row.track_id,
                },
            )
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.platform.cmp(&b.1.platform))
            .then_with(|| a.1.track_id.cmp(&b.1.track_id))
    });
    scored.truncate(limit);
    Ok(scored.into_iter().map(|(_, track)| track).collect())
}

/// [`top_scored`] 读回来的一行。
#[derive(sqlx::FromRow)]
struct Scored {
    platform: String,
    track_id: String,
    verdict: Option<i16>,
    #[sqlx(flatten)]
    listens: Listens,
}

#[derive(sqlx::FromRow)]
struct Row {
    platform: String,
    track_id: String,
    /// 曲目不在缓存里时是 NULL。
    lyric_kind: Option<String>,
    plays: i64,
    measured: i64,
    skips: i64,
    verdict: Option<i16>,
    tags: Vec<String>,
    #[sqlx(flatten)]
    listens: Listens,
}

impl Row {
    fn into_facets(self) -> TrackFacetsDto {
        TrackFacetsDto {
            play_count: u32::try_from(self.plays)
                .unwrap_or(u32::MAX),
            verdict: self.verdict,
            score: self.listens.score(self.verdict),
            skip_rate: skip_rate(self.skips, self.measured),
            lyric_kind: self
                .lyric_kind
                .as_deref()
                .and_then(lyric_kind),
            tags: self.tags,
            // 推荐日不是聚合,日推的出口自己挂
            picked_on: None,
        }
    }
}

/// 百分数取整;分母为 0 是「算不出来」。
fn skip_rate(skips: i64, measured: i64) -> Option<u8> {
    (measured > 0).then(|| {
        u8::try_from(skips * 100 / measured).unwrap_or(100)
    })
}

/// 库里的取值翻成线上的;`unknown`(还没探)与认不出的都是 `None`。
fn lyric_kind(stored: &str) -> Option<LyricKindDto> {
    match stored {
        "none" => Some(LyricKindDto::Missing),
        "instrumental" => Some(LyricKindDto::Instrumental),
        "lyric" => Some(LyricKindDto::Lyric),
        "translated" => Some(LyricKindDto::Translated),
        _ => None,
    }
}
