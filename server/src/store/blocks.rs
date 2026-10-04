//! 屏蔽规则(#161):按歌手 / 标签 / 单曲,命中的歌在所有列表里隐藏。
//!
//! 每个函数都收 `account_id` 并把它写进 WHERE,与 [`crate::store::tags`]
//! 同一条规矩。规则一个账号至多几十条,匹配在内存里做,不下推进 SQL。

use contract::{BlockKind, BlockRuleDto, TrackDto};
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
    if matches!(
        kind,
        BlockKind::Song | BlockKind::SongVersions
    ) {
        let song = serde_json::from_str::<
            contract::SongBlockDto,
        >(value)
        .map_err(|_| {
            AppError::Invalid("歌曲屏蔽规则格式无效")
        })?;
        if song.title.trim().is_empty()
            || song.artists.is_empty()
            || song
                .artists
                .iter()
                .any(|artist| artist.name.trim().is_empty())
        {
            return Err(AppError::Invalid(
                "歌曲屏蔽规则缺少歌名或歌手",
            ));
        }
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

/// 这首歌命不命中任一条规则。标签认的是 `facets.tags`,调用方得先填好聚合。
///
/// 客户端与这里都调用 contract 的纯匹配，口径共用。
pub fn hits(
    rules: &[BlockRuleDto],
    track: &TrackDto,
) -> bool {
    contract::block_hits(rules, track)
}

/// 滤掉命中规则的,返回滤掉了几首。
pub fn drop_hits(
    rules: &[BlockRuleDto],
    tracks: &mut Vec<TrackDto>,
) -> usize {
    let before = tracks.len();
    tracks.retain(|track| !hits(rules, track));
    before - tracks.len()
}

fn kind_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Artist => "artist",
        BlockKind::Tag => "tag",
        BlockKind::Track => "track",
        BlockKind::Song => "song",
        BlockKind::SongVersions => "song_versions",
    }
}

fn parse_kind(stored: &str) -> Option<BlockKind> {
    match stored {
        "artist" => Some(BlockKind::Artist),
        "tag" => Some(BlockKind::Tag),
        "track" => Some(BlockKind::Track),
        "song" => Some(BlockKind::Song),
        "song_versions" => Some(BlockKind::SongVersions),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: BlockKind, value: &str) -> BlockRuleDto {
        BlockRuleDto {
            id: "1".to_owned(),
            kind,
            value: value.to_owned(),
            label: value.to_owned(),
        }
    }

    fn track(
        id: &str,
        artists: &[&str],
        tags: &[&str],
    ) -> TrackDto {
        let mut track = TrackDto {
            artist_identities: Vec::new(),
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: id.to_owned(),
            alias: None,
            artists: artists
                .iter()
                .map(|a| (*a).to_owned())
                .collect(),
            cover: None,
            duration_ms: 1,
            album: None,
            facets: Default::default(),
        };
        track.facets.tags =
            tags.iter().map(|t| (*t).to_owned()).collect();
        track
    }

    /// 三种规则各命中自己那一首;合唱里有一位被屏蔽就算命中。
    #[test]
    fn each_kind_hits_its_own_track() {
        let rules = [
            rule(BlockKind::Artist, "甲"),
            rule(BlockKind::Tag, "吵"),
            rule(BlockKind::Track, "3"),
        ];
        let mut tracks = vec![
            track("1", &["乙", "甲"], &[]),
            track("2", &["乙"], &["吵", "夜"]),
            track("3", &["乙"], &[]),
            track("4", &["乙"], &["夜"]),
        ];

        assert_eq!(drop_hits(&rules, &mut tracks), 3);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "4");
    }

    /// 歌手名是整名比对,不是子串:屏蔽「甲」不该连带「甲乙丙」。
    #[test]
    fn artist_match_is_whole_name() {
        let rules = [rule(BlockKind::Artist, "甲")];
        assert!(!hits(
            &rules,
            &track("1", &["甲乙丙"], &[])
        ));
    }

    /// 没有规则时一首都不滤。
    #[test]
    fn no_rules_drop_nothing() {
        let mut tracks = vec![track("1", &["甲"], &["吵"])];
        assert_eq!(drop_hits(&[], &mut tracks), 0);
        assert_eq!(tracks.len(), 1);
    }
}
