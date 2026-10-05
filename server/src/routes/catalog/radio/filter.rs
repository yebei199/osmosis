//! 电台的筛选(#166):歌单视图里选中的 chip 变成电台的口味,续进来的歌照同一套过滤。
//!
//! 分段口径抄自客户端 `app_core::facets::keys` —— 两侧各维护各的(ADR 0001),
//! `tests` 里的对拍测试拿两边算同一批歌,哪边改了分段边界或 chip 文字,那条就红。

use std::collections::HashSet;

use contract::{
    FacetDto, FacetPickDto, LyricKindDto, TrackDto,
};

const SHORT_MS: i64 = 3 * 60_000;
const LONG_MS: i64 = 5 * 60_000;
const FEW_PLAYS: u32 = 5;
const MANY_PLAYS: u32 = 20;
const LOW_SKIP: u8 = 20;
const HIGH_SKIP: u8 = 50;

/// 这首歌在这个维度下是哪几个 chip。
pub(crate) fn labels(
    track: &TrackDto,
    facet: FacetDto,
) -> Vec<String> {
    let facets = &track.facets;
    let one = |label: &str| vec![label.to_owned()];
    match facet {
        FacetDto::Duration => {
            one(match track.duration_ms {
                ms if ms < SHORT_MS => "3 分钟以内",
                ms if ms <= LONG_MS => "3–5 分钟",
                _ => "5 分钟以上",
            })
        }
        FacetDto::Lyric => one(match facets.lyric_kind {
            Some(
                LyricKindDto::Lyric
                | LyricKindDto::Translated,
            ) => "有歌词",
            Some(
                LyricKindDto::Missing
                | LyricKindDto::Instrumental,
            ) => "无歌词",
            None => "歌词未知",
        }),
        FacetDto::Plays => one(match facets.play_count {
            0 => "没听过",
            n if n <= FEW_PLAYS => "听过 1–5 次",
            n if n <= MANY_PLAYS => "听过 6–20 次",
            _ => "听过 20 次以上",
        }),
        FacetDto::SkipRate => one(match facets.skip_rate {
            None => "跳过率未知",
            Some(rate) if rate < LOW_SKIP => "跳过 <20%",
            Some(rate) if rate <= HIGH_SKIP => {
                "跳过 20–50%"
            }
            Some(_) => "跳过 >50%",
        }),
        FacetDto::Verdict => one(match facets.verdict {
            Some(verdict) if verdict > 0 => "赞",
            Some(_) => "踩",
            None => "未表态",
        }),
        FacetDto::Tag if facets.tags.is_empty() => {
            one("无标签")
        }
        FacetDto::Tag => facets.tags.clone(),
    }
}

/// 过不过得了筛选:同一维度内或,跨维度且。`skip` 那一维先不看(歌词还没探)。
pub(crate) fn passes(
    track: &TrackDto,
    picks: &[FacetPickDto],
    skip: Option<FacetDto>,
) -> bool {
    let facets: HashSet<FacetDto> = picks
        .iter()
        .map(|pick| pick.facet)
        .filter(|facet| Some(*facet) != skip)
        .collect();
    facets.into_iter().all(|facet| {
        labels(track, facet).iter().any(|label| {
            picks.iter().any(|pick| {
                pick.facet == facet && &pick.label == label
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use app_core::facets::{self, Facet};
    use contract::TrackFacetsDto;

    use super::*;

    fn track() -> TrackDto {
        TrackDto {
            artist_identities: Vec::new(),
            platform: "netease".to_owned(),
            id: "x".to_owned(),
            title: "x".to_owned(),
            alias: None,
            artists: Vec::new(),
            cover: None,
            duration_ms: 200_000,
            album: None,
            facets: TrackFacetsDto::default(),
        }
    }

    fn pick(facet: FacetDto, label: &str) -> FacetPickDto {
        FacetPickDto {
            facet,
            label: label.to_owned(),
        }
    }

    /// 每个维度的每个分段各来一首,边界值都在里面。
    fn every_bucket() -> Vec<TrackDto> {
        let with = |edit: &dyn Fn(&mut TrackDto)| {
            let mut t = track();
            edit(&mut t);
            t
        };
        let mut tracks = Vec::new();
        for ms in [179_999, 180_000, 300_000, 300_001] {
            tracks.push(with(&|t| t.duration_ms = ms));
        }
        for kind in [
            None,
            Some(LyricKindDto::Missing),
            Some(LyricKindDto::Instrumental),
            Some(LyricKindDto::Lyric),
            Some(LyricKindDto::Translated),
        ] {
            tracks.push(with(&|t| {
                t.facets.lyric_kind = kind
            }));
        }
        for n in [0, 1, 5, 6, 20, 21] {
            tracks.push(with(&|t| t.facets.play_count = n));
        }
        for rate in [
            None,
            Some(0),
            Some(19),
            Some(20),
            Some(50),
            Some(51),
        ] {
            tracks
                .push(with(&|t| t.facets.skip_rate = rate));
        }
        for verdict in [None, Some(1), Some(-1)] {
            tracks.push(with(&|t| {
                t.facets.verdict = verdict
            }));
        }
        tracks.push(with(&|t| {
            t.facets.tags = vec!["夜".into(), "雨".into()];
        }));
        tracks
    }

    /// 对拍:服务端与客户端给同一首歌算出的 chip 一字不差。
    #[test]
    fn labels_match_the_client() {
        let pairs = [
            (FacetDto::Duration, Facet::Duration),
            (FacetDto::Lyric, Facet::Lyric),
            (FacetDto::Plays, Facet::Plays),
            (FacetDto::SkipRate, Facet::SkipRate),
            (FacetDto::Verdict, Facet::Verdict),
            (FacetDto::Tag, Facet::Tag),
        ];
        assert_eq!(pairs.len(), Facet::CHIPS.len());
        for track in every_bucket() {
            for (ours, theirs) in pairs {
                assert_eq!(
                    labels(&track, ours),
                    facets::keys(&track, theirs),
                    "{ours:?} 两边口径不一致"
                );
            }
        }
    }

    #[test]
    fn picks_or_within_and_across_facets() {
        let mut short_lyric = track();
        short_lyric.duration_ms = 100_000;
        short_lyric.facets.lyric_kind =
            Some(LyricKindDto::Lyric);
        let mut long_lyric = short_lyric.clone();
        long_lyric.duration_ms = 400_000;
        let mut short_bare = short_lyric.clone();
        short_bare.facets.lyric_kind =
            Some(LyricKindDto::Missing);

        let picks = [
            pick(FacetDto::Lyric, "有歌词"),
            pick(FacetDto::Duration, "3 分钟以内"),
            pick(FacetDto::Duration, "3–5 分钟"),
        ];
        assert!(passes(&short_lyric, &picks, None));
        assert!(!passes(&long_lyric, &picks, None));
        assert!(!passes(&short_bare, &picks, None));
        // 歌词那一维先不看:只剩时长
        assert!(passes(
            &short_bare,
            &picks,
            Some(FacetDto::Lyric)
        ));
        assert!(passes(&long_lyric, &[], None));
    }
}
