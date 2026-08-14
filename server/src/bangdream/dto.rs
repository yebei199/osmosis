//! bang-dream 的领域模型到 [`contract`] 的翻译。
//!
//! 翻译刻意**裁剪**:上游的 `Track` 有音质规格、付费等级等等,这里只留客户端
//! 此刻用得上的字段。加字段是兼容变更,用到时再加。

use contract::{
    ArtistDto, LyricDto, LyricLineDto, PlaySourceDto,
    PlaylistDto, PlaylistSource, TrackDto,
};

use super::lyric_split::split_long_lines;
use super::{non_empty, platform_name, proto};

/// 把上游的一个歌手翻成契约里的 [`ArtistDto`]。
///
/// 只在**搜歌手**这条路上用。内嵌在 `Track` 里的歌手走另一条路,翻成一串名字 ——
/// 那里要的是显示,这里要的是能点进去的实体。
pub fn artist_to_dto(artist: proto::Artist) -> ArtistDto {
    ArtistDto {
        id: artist.id,
        name: artist.name,
        avatar: non_empty(artist.avatar),
        album_count: artist.album_count,
    }
}

/// 把上游的一个歌单翻成契约里的 [`PlaylistDto`]。
///
/// `source` 一律是 `Platform`:能走到这个函数的都来自音乐平台。本地歌单
/// 由 [`crate::playlist`] 那侧翻,两条路各自打标,不共用一个带参数的函数 ——
/// 那样标错了不会有任何编译错误。
pub fn playlist_to_dto(
    list: proto::Playlist,
) -> PlaylistDto {
    PlaylistDto {
        source: PlaylistSource::Platform,
        id: list.id,
        name: list.name,
        cover: non_empty(list.cover),
        track_count: list.track_count,
    }
}

/// 网易云给红心歌单打的标记。
///
/// 别的特殊类型(年度歌单是 20)都是真歌单,所以判的是这一个值,不是「非零」。
///
/// 这个魔数是网易云的私有枚举,本该住在 bang-dream 里 —— `docs/adr/0022` 把它
/// 换到了这边,理由与代价都记在那篇。接第二个平台时这里会长出平台分支。
const LIKED_SPECIAL_TYPE: i32 = 5;

/// 把上游的歌单列表翻成平台那半张,顺带把红心歌单摘出去。
///
/// 摘它是因为「我喜欢的」在客户端是**另一个来源**([`PlaylistSource::Liked`]),
/// 由 [`crate::playlist::merged`] 单独置顶。不摘的话列表里会并排站着两个
/// 「我喜欢的」,而它们连 id 都不一样 —— 去重的活没人干得对。
pub fn platform_playlists_to_dto(
    lists: Vec<proto::Playlist>,
) -> Vec<PlaylistDto> {
    lists
        .into_iter()
        .filter(|list| {
            list.special_type != LIKED_SPECIAL_TYPE
        })
        .map(playlist_to_dto)
        .collect()
}

/// 从歌单列表里认出红心歌单的 id。
///
/// 与 [`platform_playlists_to_dto`] 是同一个判据的两面:那边把它摘出去,
/// 这边把它挑出来。判据只写一处([`LIKED_SPECIAL_TYPE`]),两边错开的话
/// 现象是红心既不在平台列表里、也取不到详情。
pub fn liked_playlist_id(
    lists: &[proto::Playlist],
) -> Option<String> {
    lists
        .iter()
        .find(|list| {
            list.special_type == LIKED_SPECIAL_TYPE
        })
        .map(|list| list.id.clone())
}

/// 把上游的一首歌翻成契约里的 [`TrackDto`]。
pub fn track_to_dto(track: proto::Track) -> TrackDto {
    TrackDto {
        platform: platform_name(track.platform),
        id: track.id,
        title: track.title,
        alias: non_empty(track.alias),
        artists: track
            .artists
            .into_iter()
            .map(|artist| artist.name)
            .collect(),
        cover: non_empty(track.cover),
        duration_ms: track.duration_ms,
    }
}

/// 把上游的一次播放源翻成契约里的 [`PlaySourceDto`]。
///
/// 不带 `size` 与 `level`:客户端边下边播,不需要预先知道体积;
/// 实际档位目前也没有消费者。用到时再加。
pub fn play_source_to_dto(
    source: proto::PlaySource,
) -> PlaySourceDto {
    PlaySourceDto {
        url: source.url,
        format: source.format,
        bit_rate: source.bit_rate,
        trial: source.trial,
    }
}

/// 把上游的歌词翻成契约里的 [`LyricDto`]。
///
/// 只取行级时间轴:逐字档位下上游已把整行 `text` 拼好(见 proto 的 `LyricLine`
/// 注释),行级消费方因此不必关心上游给的是哪一档。罗马音暂无消费者,不带。
///
/// 翻完过一道 [`split_long_lines`]:平台给的行粒度不可控,超长行要在这里切开。
pub fn lyric_to_dto(lyric: proto::Lyric) -> LyricDto {
    LyricDto {
        lines: split_long_lines(
            lyric
                .lines
                .into_iter()
                .map(|line| LyricLineDto {
                    start_ms: line.start_ms,
                    end_ms: line.end_ms,
                    text: line.text,
                    translation: non_empty(
                        line.translation,
                    ),
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests;
