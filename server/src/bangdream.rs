//! bang-dream 聚合层的 gRPC 客户端,以及它的领域模型到 [`contract`] 的翻译。
//!
//! 这里是本服务唯一认识 gRPC 的地方 —— 往上只有 [`contract`] 里的 DTO。
//! 客户端因此不必知道 bang-dream 的存在,也不必为上游 proto 的演化重新编译。
//!
//! 翻译刻意**裁剪**:上游的 `Track` 有音质规格、付费等级等等,这里只留客户端
//! 此刻用得上的字段。加字段是兼容变更,用到时再加。

use contract::{
    LyricDto, LyricLineDto, PlaySourceDto, TrackDto,
};

/// 由 `build.rs` 从 `third_party/bang-dream/proto` 生成。
pub mod proto {
    tonic::include_proto!("bangdream.music.v1");
}

/// 上游平台枚举翻成契约里的字符串。
///
/// 用字符串而非数字:契约要能被人读懂,也要在加平台时不依赖枚举序号的稳定性。
fn platform_name(raw: i32) -> String {
    match proto::Platform::try_from(raw) {
        Ok(proto::Platform::Netease) => "netease",
        _ => "unknown",
    }
    .to_owned()
}

/// 空串翻成 `None`。
///
/// protobuf 的 `string` 没有"缺席"这个状态,平台没给就是空串。契约区分二者:
/// `None` 是"平台没给",`Some("")` 会被客户端当成一个真实存在的空值。
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
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
pub fn lyric_to_dto(lyric: proto::Lyric) -> LyricDto {
    LyricDto {
        lines: lyric
            .lines
            .into_iter()
            .map(|line| LyricLineDto {
                start_ms: line.start_ms,
                end_ms: line.end_ms,
                text: line.text,
                translation: non_empty(line.translation),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 造一行上游歌词。
    fn proto_line(
        start_ms: i64,
        text: &str,
        translation: &str,
    ) -> proto::LyricLine {
        proto::LyricLine {
            start_ms,
            end_ms: start_ms + 200,
            text: text.to_owned(),
            words: Vec::new(),
            translation: translation.to_owned(),
            romaji: String::new(),
        }
    }

    /// 逐行歌词:时刻与文本原样过来,顺序不变。
    #[test]
    fn lyric_maps_lines_in_order() {
        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Line as i32,
            lines: vec![
                proto_line(1_000, "第一句", "first"),
                proto_line(5_000, "第二句", "second"),
            ],
        });

        assert_eq!(dto.lines.len(), 2);
        assert_eq!(dto.lines[0].start_ms, 1_000);
        assert_eq!(dto.lines[0].end_ms, 1_200);
        assert_eq!(dto.lines[0].text, "第一句");
        assert_eq!(
            dto.lines[0].translation.as_deref(),
            Some("first")
        );
        assert_eq!(dto.lines[1].text, "第二句");
    }

    /// 纯音乐/未收录:上游给空歌词,这里必须是**空行表**而不是失败 ——
    /// 「这首歌没有歌词」是正常状态。
    #[test]
    fn empty_lyric_yields_empty_lines() {
        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Line as i32,
            lines: Vec::new(),
        });
        assert!(dto.lines.is_empty());
    }

    /// 没有译文的行:空串翻成 None,不用空串冒充「有一句空翻译」。
    #[test]
    fn line_without_translation_omits_it() {
        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Line as i32,
            lines: vec![proto_line(0, "只有原文", "")],
        });
        assert_eq!(dto.lines[0].translation, None);
    }

    /// 逐字档位:上游已拼好整行文本,行级消费方拿到的东西与逐行档位一致。
    #[test]
    fn word_timed_lyric_still_yields_line_text() {
        let mut line =
            proto_line(2_000, "拼好的整行", "");
        line.words = vec![
            proto::LyricWord {
                start_ms: 2_000,
                end_ms: 2_300,
                text: "拼好的".to_owned(),
            },
            proto::LyricWord {
                start_ms: 2_300,
                end_ms: 2_600,
                text: "整行".to_owned(),
            },
        ];

        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Word as i32,
            lines: vec![line],
        });

        assert_eq!(dto.lines.len(), 1);
        assert_eq!(dto.lines[0].text, "拼好的整行");
        assert_eq!(dto.lines[0].start_ms, 2_000);
    }

    /// 构造一首字段齐全的上游歌曲,各测试再按需改动其中一两个字段。
    fn full_track() -> proto::Track {
        proto::Track {
            platform: proto::Platform::Netease as i32,
            id: "1974443814".to_owned(),
            title: "紅蓮華".to_owned(),
            alias: "鬼滅の刃 OP".to_owned(),
            artists: vec![proto::Artist {
                id: "12345".to_owned(),
                name: "LiSA".to_owned(),
                ..Default::default()
            }],
            album: Some(proto::Album {
                id: "88888".to_owned(),
                name: "LiSA BEST".to_owned(),
                cover: "https://p1.music.126.net/cover.jpg"
                    .to_owned(),
                ..Default::default()
            }),
            duration_ms: 234_000,
            cover: "https://p1.music.126.net/cover.jpg"
                .to_owned(),
            quality: None,
            fee: proto::Fee::Free as i32,
        }
    }

    /// 歌曲身份与核心字段必须原样落到 DTO:平台、id、标题、时长、封面一个不丢、一个不改。
    #[test]
    fn track_maps_identity_and_core_fields() {
        let dto = track_to_dto(full_track());

        assert_eq!(dto.platform, "netease");
        assert_eq!(dto.id, "1974443814");
        assert_eq!(dto.title, "紅蓮華");
        assert_eq!(dto.duration_ms, 234_000);
        assert_eq!(
            dto.cover.as_deref(),
            Some("https://p1.music.126.net/cover.jpg")
        );
    }

    /// 多个歌手保持列表形态,服务端不拼字符串 —— 用「/」还是「&」拼是显示问题,属于 UI。
    #[test]
    fn track_keeps_artists_as_list() {
        let mut track = full_track();
        track.artists = vec![
            proto::Artist {
                name: "LiSA".to_owned(),
                ..Default::default()
            },
            proto::Artist {
                name: "梶浦由記".to_owned(),
                ..Default::default()
            },
        ];

        let dto = track_to_dto(track);

        assert_eq!(dto.artists, vec!["LiSA", "梶浦由記"]);
    }

    /// 平台没给歌手就是空列表,不编造「未知歌手」—— 缺失就是缺失,由客户端决定怎么显示。
    #[test]
    fn track_without_artists_yields_empty_list() {
        let mut track = full_track();
        track.artists = vec![];

        let dto = track_to_dto(track);

        assert!(dto.artists.is_empty());
    }

    /// 上游的封面来自专辑,没有专辑就没有封面。此时 DTO 给 `None` 而不是空串 ——
    /// 客户端拿到 `Some("")` 会当成一个真实地址去加载,拿到 `None` 才会走占位图。
    #[test]
    fn track_without_album_omits_cover() {
        let mut track = full_track();
        track.album = None;
        track.cover = String::new();

        let dto = track_to_dto(track);

        assert_eq!(dto.cover, None);
    }

    /// 别名是可选的。空 alias 翻成 `None`,免得客户端在标题下面渲染一行空副标题。
    #[test]
    fn track_alias_is_optional() {
        let with_alias = track_to_dto(full_track());
        assert_eq!(
            with_alias.alias.as_deref(),
            Some("鬼滅の刃 OP")
        );

        let mut track = full_track();
        track.alias = String::new();
        assert_eq!(track_to_dto(track).alias, None);
    }

    /// 播放源的直链、格式、码率照搬,`trial` 必须一并送出 ——
    /// 不告诉客户端这只是试听片段的话,歌放到 30 秒就停会被当成播放器坏了。
    #[test]
    fn play_source_maps_url_and_trial_flag() {
        let source = proto::PlaySource {
            url: "https://m8.music.126.net/x.mp3"
                .to_owned(),
            format: "mp3".to_owned(),
            size: 8_000_000,
            bit_rate: 320_000,
            level: proto::QualityLevel::Standard as i32,
            trial: true,
        };

        let dto = play_source_to_dto(source);

        assert_eq!(
            dto.url,
            "https://m8.music.126.net/x.mp3"
        );
        assert_eq!(dto.format, "mp3");
        assert_eq!(dto.bit_rate, 320_000);
        assert!(dto.trial);
    }
}
