//! 歌单视图的分组与筛选要的聚合(#160):听过几次、赞踩、跳过率、歌词标记、标签。
//!
//! 全部查询时从原始表聚合,不存统计表 —— 与 `history` 同一条规矩,口径想改就改。
//!
//! **跳过**的口径:一次记下了听多久的播放,听了不到一半就算跳过。没记下
//! (进程被杀、老客户端)的那些不进分母;一次都没记下时跳过率是 `None`。

use std::collections::HashMap;

use contract::{LyricKindDto, TrackDto, TrackFacetsDto};
use sqlx::PgConnection;

use crate::error::AppError;

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

    let rows: Vec<Row> = sqlx::query_as(
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
                        WHERE pe.duration_ms > 0 AND pe.listened_ms * 2 < pe.duration_ms
                    ) AS skips
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
                coalesce(l.tags, '{}') AS tags
         FROM asked a
         LEFT JOIN platform_tracks d USING (platform, track_id)
         LEFT JOIN plays p USING (platform, track_id)
         LEFT JOIN labels l USING (platform, track_id)
         LEFT JOIN track_feedback f
           ON f.account_id = $1
          AND f.platform = a.platform AND f.track_id = a.track_id",
    )
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
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
}

impl Row {
    fn into_facets(self) -> TrackFacetsDto {
        TrackFacetsDto {
            play_count: u32::try_from(self.plays)
                .unwrap_or(u32::MAX),
            verdict: self.verdict,
            skip_rate: skip_rate(self.skips, self.measured),
            lyric_kind: self
                .lyric_kind
                .as_deref()
                .and_then(lyric_kind),
            tags: self.tags,
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
