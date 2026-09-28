//! 屏蔽规则在客户端这一侧的唯一用处:队列前进时跳过命中的歌(#161)。
//!
//! 列表的隐藏在服务端出口做,这里不重复;但已经装进队列的那一批是规则建立
//! 之前拿到的,只能在前进那一刻再认一次。口径与服务端 `store::blocks::hits`
//! 一致,改一边要改另一边。

use contract::{BlockKind, BlockRuleDto, TrackDto};

/// 这首歌命不命中任一条规则。标签认的是曲目带着的 `facets.tags` ——
/// 那是装进队列那一刻的,之后才打的标签认不出来。
// ponytail: 标签用的是入队时的快照;真要认后打的标签,得在前进时问一次服务端
pub fn hits(
    rules: &[BlockRuleDto],
    track: &TrackDto,
) -> bool {
    rules.iter().any(|rule| match rule.kind {
        BlockKind::Artist => {
            track.artists.contains(&rule.value)
        }
        BlockKind::Tag => {
            track.facets.tags.contains(&rule.value)
        }
        BlockKind::Track => track.id == rule.value,
    })
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
        artist: &str,
        tag: &str,
    ) -> TrackDto {
        let mut track = TrackDto {
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: id.to_owned(),
            alias: None,
            artists: vec![artist.to_owned()],
            cover: None,
            duration_ms: 1,
            album: None,
            facets: Default::default(),
        };
        track.facets.tags = vec![tag.to_owned()];
        track
    }

    /// 三种规则各认自己那一样;都不沾的不命中。
    #[test]
    fn each_kind_matches_its_own_field() {
        let clean = track("9", "乙", "夜");
        assert!(hits(
            &[rule(BlockKind::Artist, "甲")],
            &track("1", "甲", "夜")
        ));
        assert!(hits(
            &[rule(BlockKind::Tag, "吵")],
            &track("2", "乙", "吵")
        ));
        assert!(hits(
            &[rule(BlockKind::Track, "3")],
            &track("3", "乙", "夜")
        ));
        assert!(!hits(
            &[
                rule(BlockKind::Artist, "甲"),
                rule(BlockKind::Tag, "吵"),
                rule(BlockKind::Track, "3"),
            ],
            &clean
        ));
    }
}
