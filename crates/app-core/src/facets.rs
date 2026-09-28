//! 歌单视图的分组与筛选(#160):同一批歌按歌手、专辑、时长段……分成几堆,
//! 或只留下某几类。
//!
//! 全在客户端算:最大的歌单千首量级,本地一遍扫完,省掉一套服务端查询参数。
//! 每首歌落进哪一堆由 [`keys`] 一处决定,分组、筛选、chips 都用它 ——
//! 三处各算一遍的话,「按时长分组有 12 首短歌」和「筛短歌出来 11 首」迟早对不上。

use std::collections::{HashMap, HashSet};

use contract::{
    FacetDto, FacetPickDto, LyricKindDto, TrackDto,
};

/// 一个分组维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facet {
    Artist,
    Album,
    Duration,
    Lyric,
    Plays,
    SkipRate,
    Verdict,
    Tag,
}

impl Facet {
    /// 分组条上的次序。界面的分组编号是 `下标 + 1`,0 是「不分组」。
    pub const ALL: [Self; 8] = [
        Self::Artist,
        Self::Album,
        Self::Duration,
        Self::Lyric,
        Self::Plays,
        Self::SkipRate,
        Self::Verdict,
        Self::Tag,
    ];

    /// 能当筛选 chip 的维度。歌手与专辑动辄上百个取值,一排 chip 摆不下,只分组。
    pub const CHIPS: [Self; 6] = [
        Self::Duration,
        Self::Lyric,
        Self::Plays,
        Self::SkipRate,
        Self::Verdict,
        Self::Tag,
    ];

    /// 线上的那个维度。只分组、不当 chip 的歌手与专辑没有。
    pub fn to_dto(self) -> Option<FacetDto> {
        match self {
            Self::Duration => Some(FacetDto::Duration),
            Self::Lyric => Some(FacetDto::Lyric),
            Self::Plays => Some(FacetDto::Plays),
            Self::SkipRate => Some(FacetDto::SkipRate),
            Self::Verdict => Some(FacetDto::Verdict),
            Self::Tag => Some(FacetDto::Tag),
            Self::Artist | Self::Album => None,
        }
    }

    /// 界面编号 → 维度。0 与认不出的都是「不分组」。
    pub fn from_index(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| Self::ALL.get(index).copied())
    }

    /// 维度 → 界面编号,与 [`Self::from_index`] 互逆。
    pub fn index(self) -> i32 {
        Self::ALL
            .iter()
            .position(|facet| *facet == self)
            .map_or(0, |at| at as i32 + 1)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Artist => "歌手",
            Self::Album => "专辑",
            Self::Duration => "时长",
            Self::Lyric => "歌词",
            Self::Plays => "播放",
            Self::SkipRate => "跳过",
            Self::Verdict => "赞踩",
            Self::Tag => "标签",
        }
    }

    /// 取值固定的维度按这个次序摆;歌手、专辑、标签按堆大小排。
    fn fixed_order(
        self,
    ) -> Option<&'static [&'static str]> {
        match self {
            Self::Duration => Some(&[SHORT, MEDIUM, LONG]),
            Self::Lyric => Some(&[
                WITH_LYRIC,
                WITHOUT_LYRIC,
                LYRIC_UNKNOWN,
            ]),
            Self::Plays => Some(&[
                NEVER_PLAYED,
                PLAYED_FEW,
                PLAYED_SOME,
                PLAYED_MANY,
            ]),
            Self::SkipRate => Some(&[
                SKIP_LOW,
                SKIP_MID,
                SKIP_HIGH,
                SKIP_UNKNOWN,
            ]),
            Self::Verdict => {
                Some(&[UPVOTED, DOWNVOTED, NO_VERDICT])
            }
            Self::Artist | Self::Album | Self::Tag => None,
        }
    }

    /// 「没有这一项」的那一堆。开放取值的维度把它排在最后。
    fn missing(self) -> Option<&'static str> {
        match self {
            Self::Artist => Some(UNKNOWN_ARTIST),
            Self::Album => Some(NO_ALBUM),
            Self::Tag => Some(NO_TAG),
            _ => None,
        }
    }
}

// 分段边界是主路由给的默认值(#160),用户可改 —— 改这里,测试跟着改。
const SHORT_MS: i64 = 3 * 60_000;
const LONG_MS: i64 = 5 * 60_000;
const FEW_PLAYS: u32 = 5;
const MANY_PLAYS: u32 = 20;
const LOW_SKIP: u8 = 20;
const HIGH_SKIP: u8 = 50;

const SHORT: &str = "3 分钟以内";
const MEDIUM: &str = "3–5 分钟";
const LONG: &str = "5 分钟以上";
const WITH_LYRIC: &str = "有歌词";
const WITHOUT_LYRIC: &str = "无歌词";
const LYRIC_UNKNOWN: &str = "歌词未知";
const NEVER_PLAYED: &str = "没听过";
const PLAYED_FEW: &str = "听过 1–5 次";
const PLAYED_SOME: &str = "听过 6–20 次";
const PLAYED_MANY: &str = "听过 20 次以上";
const SKIP_LOW: &str = "跳过 <20%";
const SKIP_MID: &str = "跳过 20–50%";
const SKIP_HIGH: &str = "跳过 >50%";
const SKIP_UNKNOWN: &str = "跳过率未知";
const UPVOTED: &str = "赞";
const DOWNVOTED: &str = "踩";
const NO_VERDICT: &str = "未表态";
const UNKNOWN_ARTIST: &str = "未知歌手";
const NO_ALBUM: &str = "无专辑";
const NO_TAG: &str = "无标签";

/// 这首歌在这个维度下落进哪几堆。多歌手、多标签的歌落进每一堆 —— 那是预期行为。
pub fn keys(track: &TrackDto, facet: Facet) -> Vec<String> {
    let facets = &track.facets;
    let one = |label: &str| vec![label.to_owned()];
    let many = |values: &[String], missing: &str| {
        if values.is_empty() {
            one(missing)
        } else {
            values.to_vec()
        }
    };
    match facet {
        Facet::Artist => {
            many(&track.artists, UNKNOWN_ARTIST)
        }
        Facet::Album => track.album.as_ref().map_or_else(
            || one(NO_ALBUM),
            |album| one(&album.name),
        ),
        Facet::Duration => one(match track.duration_ms {
            ms if ms < SHORT_MS => SHORT,
            ms if ms <= LONG_MS => MEDIUM,
            _ => LONG,
        }),
        Facet::Lyric => one(match facets.lyric_kind {
            Some(
                LyricKindDto::Lyric
                | LyricKindDto::Translated,
            ) => WITH_LYRIC,
            Some(
                LyricKindDto::Missing
                | LyricKindDto::Instrumental,
            ) => WITHOUT_LYRIC,
            None => LYRIC_UNKNOWN,
        }),
        Facet::Plays => one(match facets.play_count {
            0 => NEVER_PLAYED,
            n if n <= FEW_PLAYS => PLAYED_FEW,
            n if n <= MANY_PLAYS => PLAYED_SOME,
            _ => PLAYED_MANY,
        }),
        Facet::SkipRate => one(match facets.skip_rate {
            None => SKIP_UNKNOWN,
            Some(rate) if rate < LOW_SKIP => SKIP_LOW,
            Some(rate) if rate <= HIGH_SKIP => SKIP_MID,
            Some(_) => SKIP_HIGH,
        }),
        Facet::Verdict => one(match facets.verdict {
            Some(verdict) if verdict > 0 => UPVOTED,
            Some(_) => DOWNVOTED,
            None => NO_VERDICT,
        }),
        Facet::Tag => many(&facets.tags, NO_TAG),
    }
}

/// 一堆:它叫什么,里面是哪几首(`tracks` 里的下标,保持原次序)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pile {
    pub label: String,
    pub members: Vec<usize>,
}

/// 把 `shown` 这几首按 `facet` 分堆。取值固定的维度按固定次序,其余按堆大小、
/// 同样大按名字;「没有这一项」那堆垫底。空的堆不出现。
pub fn group(
    tracks: &[TrackDto],
    shown: &[usize],
    facet: Facet,
) -> Vec<Pile> {
    let mut piles: Vec<Pile> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for &index in shown {
        for key in keys(&tracks[index], facet) {
            let slot = *at
                .entry(key.clone())
                .or_insert_with(|| {
                    piles.push(Pile {
                        label: key,
                        members: Vec::new(),
                    });
                    piles.len() - 1
                });
            // 同一首的同一个取值只进一次(平台偶尔把同一个歌手写两遍)
            if piles[slot].members.last() != Some(&index) {
                piles[slot].members.push(index);
            }
        }
    }

    match facet.fixed_order() {
        Some(order) => piles.sort_by_key(|pile| {
            order
                .iter()
                .position(|label| *label == pile.label)
        }),
        None => {
            let missing = facet.missing();
            piles.sort_by(|a, b| {
                (Some(a.label.as_str()) == missing)
                    .cmp(
                        &(Some(b.label.as_str())
                            == missing),
                    )
                    .then(
                        b.members
                            .len()
                            .cmp(&a.members.len()),
                    )
                    .then(a.label.cmp(&b.label))
            });
        }
    }
    piles
}

/// 选中的筛选条件:同一维度内是「或」,不同维度之间是「且」。
pub type Chosen = HashSet<(Facet, String)>;

/// 选中的筛选翻成线上格式,电台续歌带着它(#166)。按维度、取值排好,
/// 同样的选择翻出同样的一份。
pub fn picks(chosen: &Chosen) -> Vec<FacetPickDto> {
    let mut picks: Vec<(i32, FacetPickDto)> = chosen
        .iter()
        .filter_map(|(facet, label)| {
            Some((
                facet.index(),
                FacetPickDto {
                    facet: facet.to_dto()?,
                    label: label.clone(),
                },
            ))
        })
        .collect();
    picks.sort_by(|a, b| {
        (a.0, &a.1.label).cmp(&(b.0, &b.1.label))
    });
    picks.into_iter().map(|(_, pick)| pick).collect()
}

/// 过得了筛选的那几首,原次序。什么都没选就是全部。
pub fn filter(
    tracks: &[TrackDto],
    chosen: &Chosen,
) -> Vec<usize> {
    let facets: HashSet<Facet> =
        chosen.iter().map(|(facet, _)| *facet).collect();
    (0..tracks.len())
        .filter(|&index| {
            facets.iter().all(|&facet| {
                keys(&tracks[index], facet).into_iter().any(
                    |key| chosen.contains(&(facet, key)),
                )
            })
        })
        .collect()
}

/// 一个可选的筛选条件,连同整批里有几首是它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub facet: Facet,
    pub label: String,
    pub count: usize,
}

/// 这批歌能摆出的全部 chip。按**整批**算,不随筛选变 —— 选中一个之后别的
/// chip 就消失的话,多选无从谈起。
pub fn chips(tracks: &[TrackDto]) -> Vec<Chip> {
    let all: Vec<usize> = (0..tracks.len()).collect();
    Facet::CHIPS
        .iter()
        .flat_map(|&facet| {
            group(tracks, &all, facet).into_iter().map(
                move |pile| Chip {
                    facet,
                    label: pile.label,
                    count: pile.members.len(),
                },
            )
        })
        .collect()
}

/// 列表上的一行:堆头,或一首歌。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Header {
        label: String,
        count: usize,
        collapsed: bool,
    },
    Track(usize),
}

/// 摆出来的样子:筛过、分过堆,折起来的堆只留堆头。不分组就是筛过的平列表。
pub fn arrange(
    tracks: &[TrackDto],
    chosen: &Chosen,
    grouping: Option<Facet>,
    collapsed: &HashSet<String>,
) -> Vec<Line> {
    let shown = filter(tracks, chosen);
    let Some(facet) = grouping else {
        return shown
            .into_iter()
            .map(Line::Track)
            .collect();
    };
    group(tracks, &shown, facet)
        .into_iter()
        .flat_map(|pile| {
            let folded = collapsed.contains(&pile.label);
            let header = Line::Header {
                count: pile.members.len(),
                collapsed: folded,
                label: pile.label,
            };
            let members: Vec<Line> = if folded {
                Vec::new()
            } else {
                pile.members
                    .into_iter()
                    .map(Line::Track)
                    .collect()
            };
            std::iter::once(header).chain(members)
        })
        .collect()
}

/// 点一首歌时排进队列的那一批:摆出来的次序,每首只一次(多标签的歌出现在
/// 几堆里,队列里不该放几遍)。折起来的堆也算 —— 筛选决定了「这一批」,
/// 折叠只是看不看得见。
pub fn batch(
    tracks: &[TrackDto],
    chosen: &Chosen,
    grouping: Option<Facet>,
) -> Vec<usize> {
    let mut seen = HashSet::new();
    arrange(tracks, chosen, grouping, &HashSet::new())
        .into_iter()
        .filter_map(|line| match line {
            Line::Track(index) => Some(index),
            Line::Header { .. } => None,
        })
        .filter(|index| seen.insert(*index))
        .collect()
}

#[cfg(test)]
mod tests {
    use contract::{AlbumRefDto, TrackFacetsDto};
    use similar_asserts::assert_eq;

    use super::*;
    fn track(id: &str) -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: format!("曲 {id}"),
            alias: None,
            artists: vec!["甲".to_owned()],
            cover: None,
            duration_ms: 200_000,
            album: None,
            facets: TrackFacetsDto::default(),
        }
    }

    /// 每一堆的名字与成员(成员写成 id,读起来比下标直观)。
    fn piles(
        tracks: &[TrackDto],
        facet: Facet,
    ) -> Vec<(String, Vec<String>)> {
        let all: Vec<usize> = (0..tracks.len()).collect();
        group(tracks, &all, facet)
            .into_iter()
            .map(|pile| {
                (
                    pile.label,
                    pile.members
                        .into_iter()
                        .map(|i| tracks[i].id.clone())
                        .collect(),
                )
            })
            .collect()
    }

    fn pile(
        label: &str,
        ids: &[&str],
    ) -> (String, Vec<String>) {
        (
            label.to_owned(),
            ids.iter().map(|id| (*id).to_owned()).collect(),
        )
    }

    /// 多歌手的歌进每一位歌手的堆;堆按大小排,没有歌手的垫底。
    #[test]
    fn artist_piles_count_each_artist() {
        let mut a = track("a");
        a.artists = vec!["甲".into(), "乙".into()];
        let b = track("b");
        let mut c = track("c");
        c.artists = Vec::new();

        assert_eq!(
            piles(&[a, b, c], Facet::Artist),
            vec![
                pile("甲", &["a", "b"]),
                pile("乙", &["a"]),
                pile("未知歌手", &["c"]),
            ]
        );
    }

    #[test]
    fn album_piles_fall_back_to_no_album() {
        let mut a = track("a");
        a.album = Some(AlbumRefDto {
            id: "1".into(),
            name: "专".into(),
        });
        let b = track("b");

        assert_eq!(
            piles(&[b, a], Facet::Album),
            vec![
                pile("专", &["a"]),
                pile("无专辑", &["b"])
            ]
        );
    }

    /// 边界:3 分钟整算中段,5 分钟整也算中段。
    #[test]
    fn duration_piles_use_three_and_five_minutes() {
        let at = |id: &str, ms: i64| {
            let mut t = track(id);
            t.duration_ms = ms;
            t
        };
        let tracks = [
            at("long", 300_001),
            at("short", 179_999),
            at("three", 180_000),
            at("five", 300_000),
        ];

        assert_eq!(
            piles(&tracks, Facet::Duration),
            vec![
                pile("3 分钟以内", &["short"]),
                pile("3–5 分钟", &["three", "five"]),
                pile("5 分钟以上", &["long"]),
            ]
        );
    }

    /// 纯音乐算无歌词;还没探过的单独一堆,不冒充无歌词。
    #[test]
    fn lyric_piles_separate_unknown() {
        let with =
            |id: &str, kind: Option<LyricKindDto>| {
                let mut t = track(id);
                t.facets.lyric_kind = kind;
                t
            };
        let tracks = [
            with("x", None),
            with("t", Some(LyricKindDto::Translated)),
            with("i", Some(LyricKindDto::Instrumental)),
            with("l", Some(LyricKindDto::Lyric)),
            with("m", Some(LyricKindDto::Missing)),
        ];

        assert_eq!(
            piles(&tracks, Facet::Lyric),
            vec![
                pile("有歌词", &["t", "l"]),
                pile("无歌词", &["i", "m"]),
                pile("歌词未知", &["x"]),
            ]
        );
    }

    #[test]
    fn play_piles_use_zero_five_twenty() {
        let played = |id: &str, n: u32| {
            let mut t = track(id);
            t.facets.play_count = n;
            t
        };
        let tracks = [
            played("21", 21),
            played("20", 20),
            played("6", 6),
            played("5", 5),
            played("1", 1),
            played("0", 0),
        ];

        assert_eq!(
            piles(&tracks, Facet::Plays),
            vec![
                pile("没听过", &["0"]),
                pile("听过 1–5 次", &["5", "1"]),
                pile("听过 6–20 次", &["20", "6"]),
                pile("听过 20 次以上", &["21"]),
            ]
        );
    }

    #[test]
    fn skip_piles_keep_unknown_apart_from_zero() {
        let skipped = |id: &str, rate: Option<u8>| {
            let mut t = track(id);
            t.facets.skip_rate = rate;
            t
        };
        let tracks = [
            skipped("none", None),
            skipped("0", Some(0)),
            skipped("20", Some(20)),
            skipped("50", Some(50)),
            skipped("51", Some(51)),
        ];

        assert_eq!(
            piles(&tracks, Facet::SkipRate),
            vec![
                pile("跳过 <20%", &["0"]),
                pile("跳过 20–50%", &["20", "50"]),
                pile("跳过 >50%", &["51"]),
                pile("跳过率未知", &["none"]),
            ]
        );
    }

    #[test]
    fn verdict_piles_are_up_down_and_undecided() {
        let judged = |id: &str, verdict: Option<i16>| {
            let mut t = track(id);
            t.facets.verdict = verdict;
            t
        };
        let tracks = [
            judged("n", None),
            judged("d", Some(-1)),
            judged("u", Some(1)),
        ];

        assert_eq!(
            piles(&tracks, Facet::Verdict),
            vec![
                pile("赞", &["u"]),
                pile("踩", &["d"]),
                pile("未表态", &["n"]),
            ]
        );
    }

    /// 一首歌挂两个标签就出现在两堆里 —— #160 写明的预期行为。
    #[test]
    fn tag_piles_repeat_multi_tagged_tracks() {
        let tagged = |id: &str, tags: &[&str]| {
            let mut t = track(id);
            t.facets.tags = tags
                .iter()
                .map(|tag| (*tag).to_owned())
                .collect();
            t
        };
        let tracks = [
            tagged("a", &["夜", "雨"]),
            tagged("b", &["夜"]),
            tagged("c", &[]),
        ];

        assert_eq!(
            piles(&tracks, Facet::Tag),
            vec![
                pile("夜", &["a", "b"]),
                pile("雨", &["a"]),
                pile("无标签", &["c"]),
            ]
        );
    }

    /// 空歌单:没有堆、没有 chip、摆出来什么都没有。
    #[test]
    fn an_empty_playlist_has_nothing_to_show() {
        for facet in Facet::ALL {
            assert!(piles(&[], facet).is_empty());
        }
        assert!(chips(&[]).is_empty());
        assert!(
            arrange(
                &[],
                &Chosen::new(),
                Some(Facet::Artist),
                &HashSet::new()
            )
            .is_empty()
        );
    }

    /// 老服务端没给聚合(全是默认值):每首都落进「未知/没听过」那几堆,不丢歌。
    #[test]
    fn missing_facets_land_in_the_fallback_piles() {
        let tracks = [track("a"), track("b")];

        for facet in Facet::ALL {
            let members: usize = piles(&tracks, facet)
                .iter()
                .map(|(_, ids)| ids.len())
                .sum();
            assert_eq!(members, 2, "{facet:?} 丢了歌");
        }
        assert_eq!(
            piles(&tracks, Facet::Lyric),
            vec![pile("歌词未知", &["a", "b"])]
        );
    }

    /// 同一维度内是或,跨维度是且。
    #[test]
    fn filters_or_within_and_across_facets() {
        let mut a = track("a");
        a.facets.verdict = Some(1);
        a.duration_ms = 100_000;
        let mut b = track("b");
        b.facets.verdict = Some(-1);
        b.duration_ms = 100_000;
        let mut c = track("c");
        c.facets.verdict = Some(1);
        c.duration_ms = 400_000;
        let tracks = [a, b, c];

        let chosen: Chosen = [
            (Facet::Verdict, "赞".to_owned()),
            (Facet::Verdict, "踩".to_owned()),
            (Facet::Duration, "3 分钟以内".to_owned()),
        ]
        .into();

        assert_eq!(filter(&tracks, &chosen), vec![0, 1]);
        assert_eq!(
            filter(&tracks, &Chosen::new()),
            vec![0, 1, 2]
        );
    }

    /// chip 按整批算,带数目。
    #[test]
    fn chips_count_the_whole_batch() {
        let mut a = track("a");
        a.facets.tags = vec!["夜".into()];
        let tracks = [a, track("b")];

        let tag_chips: Vec<(String, usize)> =
            chips(&tracks)
                .into_iter()
                .filter(|chip| chip.facet == Facet::Tag)
                .map(|chip| (chip.label, chip.count))
                .collect();

        assert_eq!(
            tag_chips,
            vec![
                ("夜".to_owned(), 1),
                ("无标签".to_owned(), 1)
            ]
        );
    }

    /// 折起来的堆只剩堆头;队列那一批不受折叠影响,多标签的歌只排一次。
    #[test]
    fn collapsed_piles_keep_their_header_and_their_queue_slot()
     {
        let mut a = track("a");
        a.facets.tags = vec!["夜".into(), "雨".into()];
        let mut b = track("b");
        b.facets.tags = vec!["雨".into()];
        let tracks = [a, b];
        let collapsed: HashSet<String> =
            ["雨".to_owned()].into();

        assert_eq!(
            arrange(
                &tracks,
                &Chosen::new(),
                Some(Facet::Tag),
                &collapsed
            ),
            vec![
                Line::Header {
                    label: "雨".into(),
                    count: 2,
                    collapsed: true,
                },
                Line::Header {
                    label: "夜".into(),
                    count: 1,
                    collapsed: false,
                },
                Line::Track(0),
            ]
        );
        assert_eq!(
            batch(
                &tracks,
                &Chosen::new(),
                Some(Facet::Tag)
            ),
            vec![0, 1]
        );
    }

    #[test]
    fn picks_are_sorted_and_carry_only_chip_facets() {
        let chosen: Chosen = [
            (Facet::Tag, "雨".to_owned()),
            (Facet::Duration, "3 分钟以内".to_owned()),
            (Facet::Tag, "夜".to_owned()),
            (Facet::Artist, "甲".to_owned()),
        ]
        .into();
        let pick = |facet, label: &str| FacetPickDto {
            facet,
            label: label.to_owned(),
        };

        assert_eq!(
            picks(&chosen),
            vec![
                pick(FacetDto::Duration, "3 分钟以内"),
                pick(FacetDto::Tag, "夜"),
                pick(FacetDto::Tag, "雨"),
            ]
        );
    }

    #[test]
    fn grouping_index_round_trips() {
        assert_eq!(Facet::from_index(0), None);
        assert_eq!(Facet::from_index(99), None);
        for facet in Facet::ALL {
            assert_eq!(
                Facet::from_index(facet.index()),
                Some(facet)
            );
        }
    }
}
