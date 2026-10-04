//! 屏蔽规则的线上格式与两端共享的纯匹配(#181)。

use serde::{Deserialize, Serialize};

/// 按什么屏蔽。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// 歌手身份 JSON；历史纯名字仍可解析。
    Artist,
    /// 账号级标签名(#158)。
    Tag,
    /// 单曲,值是平台内的曲目 id。
    Track,
    /// 完全同名且歌手交集。
    Song,
    /// 规范化标题且歌手交集，包含其他版本。
    SongVersions,
}

/// 一条屏蔽规则。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct BlockRuleDto {
    pub id: String,
    pub kind: BlockKind,
    pub value: String,
    /// 设置页「已屏蔽」里显示的那一行。单曲的 `value` 是一串数字,
    /// 得靠它说出是哪首歌;歌手与标签就是 `value` 本身。
    pub label: String,
}

/// `POST /blocks` 的请求体。`label` 不给就用 `value`。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct NewBlockRuleDto {
    pub kind: BlockKind,
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
    /// 仅歌曲规则可同时给当前平台曲目记负反馈，服务端事务提交。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disliked_track: Option<crate::PlayedDto>,
}

/// `GET /blocks` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct BlockRulesDto {
    pub rules: Vec<BlockRuleDto>,
}

/// 平台内的歌手身份；没有 id 的旧数据按规范化名字回退。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct ArtistIdentityDto {
    pub platform: String,
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
}

/// song 与 song_versions 的 value 内容。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct SongBlockDto {
    pub title: String,
    pub artists: Vec<ArtistIdentityDto>,
}

/// 展示名仍是 artists；缺少身份的数据逐个补成仅名字身份。
pub fn track_artists(
    track: &crate::TrackDto,
) -> Vec<ArtistIdentityDto> {
    let mut artists = track.artist_identities.clone();
    for name in &track.artists {
        if !artists
            .iter()
            .any(|artist| artist.name == *name)
        {
            artists.push(ArtistIdentityDto {
                platform: track.platform.clone(),
                id: None,
                name: name.clone(),
            });
        }
    }
    artists
}

fn folded(value: &str) -> String {
    use unicode_casefold::UnicodeCaseFold;
    use unicode_normalization::UnicodeNormalization;
    value
        .nfkc()
        .case_fold()
        .collect::<String>()
        .trim()
        .to_owned()
}

fn same_artist(
    left: &ArtistIdentityDto,
    right: &ArtistIdentityDto,
) -> bool {
    match (
        left.id.as_deref().filter(|id| !id.is_empty()),
        right.id.as_deref().filter(|id| !id.is_empty()),
    ) {
        (Some(left_id), Some(right_id)) => {
            left.platform == right.platform
                && left_id == right_id
        }
        _ => {
            !left.name.trim().is_empty()
                && folded(&left.name) == folded(&right.name)
        }
    }
}

fn version_title(title: &str) -> String {
    let normalized = folded(title);
    let mut rest = normalized.as_str();
    loop {
        if let Some(prefix) = strip_bracket(rest) {
            rest = prefix.trim_end();
            continue;
        }
        if let Some((prefix, suffix)) =
            rest.rsplit_once(" - ")
        {
            if [
                "live",
                "remaster",
                "version",
                "ver.",
                "edit",
                "mix",
                "acoustic",
                "instrumental",
                "现场",
                "伴奏",
                "重制",
            ]
            .iter()
            .any(|keyword| suffix.contains(keyword))
            {
                rest = prefix.trim_end();
                continue;
            }
        }
        return rest.to_owned();
    }
}

fn strip_bracket(title: &str) -> Option<&str> {
    let (open, close) = match title.chars().last()? {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        '】' => ('【', '】'),
        _ => return None,
    };
    let mut depth = 0;
    for (index, character) in title.char_indices().rev() {
        if character == close {
            depth += 1;
        }
        if character == open {
            depth -= 1;
            if depth == 0 {
                return Some(&title[..index]);
            }
        }
    }
    None
}

/// 客户端队列和服务端目录、组推进使用相同口径。损坏规则不匹配。
pub fn block_hits(
    rules: &[BlockRuleDto],
    track: &crate::TrackDto,
) -> bool {
    rules.iter().any(|rule| match rule.kind {
        BlockKind::Track => track.id == rule.value,
        BlockKind::Tag => {
            track.facets.tags.contains(&rule.value)
        }
        BlockKind::Artist => {
            let artist = serde_json::from_str::<
                ArtistIdentityDto,
            >(&rule.value)
            .unwrap_or_else(|_| ArtistIdentityDto {
                platform: track.platform.clone(),
                id: None,
                name: rule.value.clone(),
            });
            track_artists(track).iter().any(|candidate| {
                same_artist(&artist, candidate)
            })
        }
        BlockKind::Song | BlockKind::SongVersions => {
            let Ok(song) = serde_json::from_str::<
                SongBlockDto,
            >(&rule.value) else {
                return false;
            };
            let title_matches = if rule.kind
                == BlockKind::Song
            {
                song.title.trim() == track.title.trim()
            } else {
                let left = version_title(&song.title);
                let right = version_title(&track.title);
                if left.is_empty() || right.is_empty() {
                    song.title.trim() == track.title.trim()
                } else {
                    left == right
                }
            };
            title_matches
                && song.artists.iter().any(|artist| {
                    track_artists(track).iter().any(
                        |candidate| {
                            same_artist(artist, candidate)
                        },
                    )
                })
        }
    })
}

/// 选择歌曲理由后的写入请求，负反馈只绑定此次选中的平台曲目。
pub fn song_dislike(
    track: &crate::TrackDto,
    versions: bool,
) -> NewBlockRuleDto {
    let artists = track_artists(track);
    let names = track.artists.join(" / ");
    let suffix = if versions {
        "（含其他版本）"
    } else {
        ""
    };
    NewBlockRuleDto {
        kind: if versions {
            BlockKind::SongVersions
        } else {
            BlockKind::Song
        },
        value: serde_json::to_string(&SongBlockDto {
            title: track.title.clone(),
            artists,
        })
        .expect("string-only song rule"),
        label: Some(format!(
            "{} — {}{}",
            track.title.trim(),
            names,
            suffix
        )),
        disliked_track: (!versions).then(|| {
            crate::PlayedDto {
                platform: track.platform.clone(),
                track_id: track.id.clone(),
            }
        }),
    }
}

/// 单歌手直接保存，多歌手由用户选出的身份保存。
pub fn artist_dislike(
    artist: ArtistIdentityDto,
) -> NewBlockRuleDto {
    NewBlockRuleDto {
        kind: BlockKind::Artist,
        label: Some(artist.name.clone()),
        value: serde_json::to_string(&artist)
            .expect("string-only artist rule"),
        disliked_track: None,
    }
}
