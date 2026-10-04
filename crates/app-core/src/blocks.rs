//! 客户端已有队列与视图的屏蔽匹配入口(#181)。
//!
//! 服务端过滤未来响应，客户端保存成功后清理已有批次。
//! 两端共同调用 contract 的纯匹配。

#[cfg(test)]
use contract::BlockKind;
use contract::{BlockRuleDto, TrackDto};

/// 这首歌命不命中任一条规则。标签认的是曲目带着的 `facets.tags` ——
/// 那是装进队列那一刻的,之后才打的标签认不出来。
// ponytail: 标签用的是入队时的快照;真要认后打的标签,得在前进时问一次服务端
pub fn hits(
    rules: &[BlockRuleDto],
    track: &TrackDto,
) -> bool {
    contract::block_hits(rules, track)
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
            artist_identities: Vec::new(),
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

#[cfg(test)]
mod dislike_tests {
    use super::*;
    use serde_json::{Value, json};

    fn artist(name: &str, id: Option<&str>) -> Value {
        json!({"name": name, "id": id, "platform": "netease"})
    }

    fn song_rule(
        kind: &str,
        title: &str,
        artists: Vec<Value>,
    ) -> BlockRuleDto {
        serde_json::from_value(json!({
            "id": "181", "kind": kind,
            "value": json!({"title": title, "artists": artists}).to_string(),
            "label": title,
        })).expect("新歌曲规则必须可在线上格式中表达")
    }

    fn song(title: &str, artists: Vec<Value>) -> TrackDto {
        serde_json::from_value(json!({
            "platform": "netease", "id": "different-id", "title": title,
            "artists": artists.iter().map(|artist| artist["name"].clone()).collect::<Vec<_>>(),
            "artist_identities": artists, "duration_ms": 90000,
            "album": {"id": "different-album", "name": "other"},
        })).expect("曲目兼容新增的可选歌手身份")
    }

    // 完全同名只 trim；曲目 id、专辑不能参与歌曲规则的匹配。
    #[test]
    fn exact_song_matches_all_ids_but_preserves_case_width_and_versions()
     {
        let rule = song_rule(
            "song",
            "  Song  ",
            vec![artist("甲", None)],
        );
        for (title, expected) in [
            ("Song", true),
            (" Song ", true),
            ("song", false),
            ("Ｓｏｎｇ", false),
            ("Song (Live)", false),
            ("Song - Live", false),
        ] {
            assert_eq!(
                hits(
                    std::slice::from_ref(&rule),
                    &song(title, vec![artist("甲", None)])
                ),
                expected,
                "{title}"
            );
        }
    }

    // 尾括号可叠加；版本后缀有限定，不吞普通破折号标题或中间括号。
    #[test]
    fn normalized_song_strips_only_approved_trailing_versions()
     {
        let rule = song_rule(
            "song_versions",
            "Song",
            vec![artist("甲", None)],
        );
        for (title, expected) in [
            (" ＳＯＮＧ ", true),
            ("song (Live)", true),
            ("Song（现场）", true),
            ("Song [Remastered]【伴奏】 (2024)", true),
            ("Song - Live", true),
            ("Song - remaster", true),
            ("Song - Remastered 2024", true),
            ("Song - Version", true),
            ("Song - ver. 2", true),
            ("Song - Edit", true),
            ("Song - Remix", true),
            ("Song - Acoustic", true),
            ("Song - Instrumental", true),
            ("Song - 现场", true),
            ("Song - 伴奏", true),
            ("Song - 重制", true),
            ("Song (Live) - Acoustic", true),
            ("Song - Live (2024)", true),
            ("Song - Story", false),
            ("Song (Live) Again", false),
            ("Song (Unclosed", false),
            ("Other", false),
        ] {
            assert_eq!(
                hits(
                    std::slice::from_ref(&rule),
                    &song(title, vec![artist("甲", None)])
                ),
                expected,
                "{title}"
            );
        }
    }

    // casefold 不等于 lowercase；NFKC 还必须处理兼容字符。
    #[test]
    fn normalized_song_and_artist_use_unicode_nfkc_casefold()
     {
        let rule = song_rule(
            "song_versions",
            "Straße",
            vec![artist("ＡＢＣ", None)],
        );
        assert!(hits(
            &[rule],
            &song(
                "STRASSE (Live)",
                vec![artist("abc", None)]
            )
        ));
    }

    // 合作歌手取交集，不把同歌手的其他歌或他人的同名歌屏蔽掉。
    #[test]
    fn song_requires_title_and_an_intersecting_artist() {
        for kind in ["song", "song_versions"] {
            let rule = song_rule(
                kind,
                "Song",
                vec![
                    artist("甲", None),
                    artist("乙", None),
                ],
            );
            for (title, names, expected) in [
                ("Song", vec!["乙", "丙"], true),
                ("Song", vec!["丙"], false),
                ("Other", vec!["甲"], false),
                ("Song", vec![], false),
            ] {
                assert_eq!(
                    hits(
                        std::slice::from_ref(&rule),
                        &song(
                            title,
                            names
                                .into_iter()
                                .map(|name| artist(
                                    name, None
                                ))
                                .collect()
                        )
                    ),
                    expected,
                    "{kind}/{title}"
                );
            }
        }
    }

    // 双方有平台 id 时以平台 + id 为准；旧曲目没 id 时按规范化名字退回。
    #[test]
    fn artist_identity_prefers_platform_ids_and_falls_back_for_old_tracks()
     {
        let rule = song_rule(
            "song",
            "Song",
            vec![artist("甲", Some("7"))],
        );
        assert!(hits(
            std::slice::from_ref(&rule),
            &song("Song", vec![artist("改名", Some("7"))])
        ));
        assert!(!hits(
            std::slice::from_ref(&rule),
            &song("Song", vec![artist("甲", Some("8"))])
        ));
        let mut other_platform = artist("甲", Some("7"));
        other_platform["platform"] = json!("other");
        assert!(!hits(
            std::slice::from_ref(&rule),
            &song("Song", vec![other_platform])
        ));
        assert!(hits(
            &[rule],
            &song("Song", vec![artist("甲", None)])
        ));
    }

    // 规范化后空标题退回 exact，不能把所有括号标题归为同一首。
    #[test]
    fn empty_normalized_titles_fall_back_to_exact_match() {
        let rule = song_rule(
            "song_versions",
            " (Live) ",
            vec![artist("甲", None)],
        );
        assert!(hits(
            std::slice::from_ref(&rule),
            &song("(Live)", vec![artist("甲", None)])
        ));
        assert!(!hits(
            &[rule],
            &song("(Acoustic)", vec![artist("甲", None)])
        ));
    }

    // 持久数据损坏应无匹配，而不能 panic 或误屏蔽整份目录。
    #[test]
    fn malformed_song_values_never_match() {
        for value in [
            "",
            "not-json",
            "{}",
            r#"{"title":"Song","artists":[]}"#,
        ] {
            let mut rule = song_rule(
                "song",
                "Song",
                vec![artist("甲", None)],
            );
            rule.value = value.to_owned();
            assert!(!hits(
                &[rule],
                &song("Song", vec![artist("甲", None)])
            ));
        }
    }

    // 旧 artist / tag / track 规则保留，artist 名字也遵守 Unicode 比较。
    #[test]
    fn legacy_artist_tag_and_track_rules_keep_working() {
        let mut track =
            song("Song", vec![artist("ＡＢＣ", None)]);
        track.facets.tags = vec!["夜".to_owned()];
        for (kind, value) in [
            (BlockKind::Artist, "abc"),
            (BlockKind::Tag, "夜"),
            (BlockKind::Track, "different-id"),
        ] {
            assert!(hits(
                &[BlockRuleDto {
                    id: "old".to_owned(),
                    kind,
                    value: value.to_owned(),
                    label: value.to_owned()
                }],
                &track
            ));
        }
    }
}
