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
    let mut line = proto_line(2_000, "拼好的整行", "");
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
        url: "https://m8.music.126.net/x.mp3".to_owned(),
        format: "mp3".to_owned(),
        size: 8_000_000,
        bit_rate: 320_000,
        level: proto::QualityLevel::Standard as i32,
        trial: true,
    };

    let dto = play_source_to_dto(source);

    assert_eq!(dto.url, "https://m8.music.126.net/x.mp3");
    assert_eq!(dto.format, "mp3");
    assert_eq!(dto.bit_rate, 320_000);
    assert!(dto.trial);
}

/// 歌手的详情字段都过桥;头像空串翻成 None,不用空串冒充"有头像"。
#[test]
fn artist_to_dto_maps_the_detail_fields() {
    let dto = artist_to_dto(proto::Artist {
        id: "11127".to_owned(),
        name: "本兮".to_owned(),
        avatar: "https://p1.music.126.net/a.jpg".to_owned(),
        description: String::new(),
        album_count: 24,
    });

    assert_eq!(dto.id, "11127");
    assert_eq!(dto.name, "本兮");
    assert_eq!(
        dto.avatar.as_deref(),
        Some("https://p1.music.126.net/a.jpg")
    );
    assert_eq!(dto.album_count, 24);

    let no_avatar = artist_to_dto(proto::Artist {
        id: "1".to_owned(),
        name: "甲".to_owned(),
        ..Default::default()
    });
    assert_eq!(no_avatar.avatar, None);
}

/// 走这条路的歌单一律标成平台来源 —— 本地歌单从不经过这里。
/// 标错的现象是界面把删除键画在了平台歌单上。
#[test]
fn playlist_to_dto_tags_the_platform_source() {
    let dto = playlist_to_dto(proto::Playlist {
        id: "24381616".to_owned(),
        name: "华语经典".to_owned(),
        track_count: 120,
        ..Default::default()
    });

    assert_eq!(dto.source, PlaylistSource::Platform);
    assert_eq!(dto.id, "24381616");
    assert_eq!(dto.track_count, 120);
    // 封面缺席时是 None
    assert_eq!(dto.cover, None);
}

/// 红心歌单不进平台那半张列表。
///
/// bang-dream 从此原样透传它(见 `docs/adr/0022`),排除的责任转移到了这边。
/// 漏掉这一步的现象是界面上并排站着两个「我喜欢的」,而它们连 id 都不一样。
#[test]
fn the_liked_playlist_is_dropped_from_the_platform_half() {
    let got = platform_playlists_to_dto(vec![
        proto::Playlist {
            id: "403421443".to_owned(),
            name: "青城叶北喜欢的音乐".to_owned(),
            special_type: 5,
            ..Default::default()
        },
        proto::Playlist {
            id: "17627306389".to_owned(),
            name: "favorite_1".to_owned(),
            ..Default::default()
        },
    ]);

    assert_eq!(got.len(), 1, "红心歌单不该进平台那半张");
    assert_eq!(got[0].id, "17627306389");
}

/// 别的特殊类型一个都不能误伤:年度歌单(`special_type` 20)是真歌单。
/// 按「非零就排除」写会把它一起吃掉 —— 这个坑上游踩过一次。
#[test]
fn other_special_types_stay_in_the_platform_half() {
    let got =
        platform_playlists_to_dto(vec![proto::Playlist {
            id: "13053579003".to_owned(),
            name: "青城叶北的2024年度歌单".to_owned(),
            special_type: 20,
            ..Default::default()
        }]);

    assert_eq!(got.len(), 1, "年度歌单是真歌单");
}

/// 边界:`special_type` 缺席落到 0,那是普通歌单,要留下。
#[test]
fn a_playlist_without_a_special_type_is_ordinary() {
    let got =
        platform_playlists_to_dto(vec![proto::Playlist {
            id: "812552458".to_owned(),
            name: "anime".to_owned(),
            ..Default::default()
        }]);

    assert_eq!(got.len(), 1);
}
