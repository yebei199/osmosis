use super::*;

/// 三种来源的编号来回转都对得上。
///
/// 转错的现象是点开一个歌单看到另一个歌单的歌 —— 而两边都不报错。
#[test]
fn each_source_has_its_own_way_in() {
    for source in
        [Source::Liked, Source::Platform, Source::Local]
    {
        assert_eq!(
            Source::from_index(source.to_index()),
            source,
            "{source:?} 的编号转不回来"
        );
    }

    // 三个编号互不相同,否则上面那条也会过
    assert_eq!(Source::Liked.to_index(), 0);
    assert_eq!(Source::Platform.to_index(), 1);
    assert_eq!(Source::Local.to_index(), 2);

    // 认不出的编号落到平台歌单:三者里唯一只读的那个
    assert_eq!(Source::from_index(99), Source::Platform);
}

/// 「把刚才那批加进来」的文案带上条数;没有可加的就是空串,那一行不出现。
///
/// 进歌单那一刻列表已经换掉了 —— 不说清是哪一批,用户点下去才知道加了什么。
#[test]
fn add_batch_label_says_how_many() {
    assert_eq!(
        add_batch_text(30),
        "+ 把刚才那 30 首加进来"
    );
    assert_eq!(add_batch_text(1), "+ 把刚才那 1 首加进来");
    assert_eq!(
        add_batch_text(0),
        "",
        "没有可加的就不该有这一行"
    );
}

/// 能改的只有本地歌单。
///
/// 判据是来源不是名字:用户完全可以把一个本地歌单起名叫「我喜欢的」,
/// 而按名字判的话,那个歌单会突然变得不能改。
#[test]
fn only_local_playlists_are_editable() {
    assert!(is_editable(Source::Local));
    assert!(!is_editable(Source::Liked));
    assert!(!is_editable(Source::Platform));
}

/// 副标题说的是有多少首歌;空歌单说「暂无曲目」而不是「0 首」——
/// 后者读起来像个统计数字,而这里要说的是"点进去也没东西"。
#[test]
fn the_subtitle_says_how_many_tracks() {
    assert_eq!(track_count_text(120), "120 首");
    assert_eq!(track_count_text(1), "1 首");
    assert_eq!(track_count_text(0), "暂无曲目");
    // 上游给了个负数也不该露出来
    assert_eq!(track_count_text(-1), "暂无曲目");
}

/// 写出去的条数读得回来 —— 点红心之后要就地把这个数字加一,
/// 而界面上只剩这句话,没有别处存着那个数。
#[test]
fn a_track_count_survives_a_round_trip_through_its_text() {
    for count in [1, 7, 120, 976] {
        assert_eq!(
            track_count_of(&track_count_text(count)),
            Some(count),
            "{count} 首该读得回来"
        );
    }
}

/// 边界:读不出数字时返回 `None`,调用方据此不动它。
///
/// 「暂无曲目」是其中一种 —— 它对应 0,但也对应「上游给了个负数」,
/// 两者不该被读成同一个可加减的起点。
#[test]
fn an_unreadable_subtitle_yields_no_count() {
    // 「暂无曲目」既对应 0,也对应上游给的负数 —— 不是可加减的起点
    assert_eq!(track_count_of(&track_count_text(0)), None);
    assert_eq!(track_count_of(&track_count_text(-1)), None);
    // 别处写来的文案不能被误读成条数
    assert_eq!(track_count_of("12 张专辑"), None);
    assert_eq!(track_count_of(""), None);
    assert_eq!(track_count_of("首"), None);
    assert_eq!(track_count_of("很多 首"), None);
}

/// 「另有 N 首平台不再提供」要说出具体几首。
///
/// 服务端把拿不到详情的曲目剔出成员关系(见 server 的 `cached_tracks`),
/// 不说一声的话用户看到的只是数目对不上,而分不清「我少点了一个红心」
/// 和「平台不给这首歌的详情」。
#[test]
fn the_note_says_how_many_are_unavailable() {
    assert!(unavailable_text(1).contains('1'));
    assert!(unavailable_text(23).contains("23"));
}

/// 边界:一首都没少时返回空串,那一行整个不出现 ——
/// 与 `add_batch_text` 同一条规矩。常态就是这一条,一个恒显示的
/// 「另有 0 首」只会变成噪声。
#[test]
fn no_note_when_nothing_is_unavailable() {
    assert_eq!(
        unavailable_text(0),
        "",
        "一首都没少时那一行整个不该出现"
    );
}

/// 契约里的来源原样翻成界面编号,不在中间丢掉。
#[test]
fn the_contract_source_survives_the_trip() {
    let row = to_row(&PlaylistDto {
        source: PlaylistSource::Local,
        id: "3".to_owned(),
        name: "睡前".to_owned(),
        cover: None,
        track_count: 12,
    });

    assert_eq!(row.id, "3");
    assert_eq!(row.name, "睡前");
    assert_eq!(row.subtitle, "12 首");
    assert_eq!(row.source, Source::Local.to_index());
}
