//! 歌单那一面的纯换算:来源枚举、副标题文案、可编辑判据、排序键与行的成形。

use api::TrackSortDto;
use app_core::{PlaylistDto, PlaylistSource, TracksDto};

use crate::PlaylistRow;

/// 歌单的来源。数值即 `PlaylistSource` 的顺序,也是 `.slint` 里
/// `PlaylistRow.source` 的取值 —— 三处手工对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Liked,
    Platform,
    Local,
}

impl Source {
    /// 由契约里的来源认出它。
    pub fn from_dto(source: PlaylistSource) -> Self {
        match source {
            PlaylistSource::Liked => Self::Liked,
            PlaylistSource::Platform => Self::Platform,
            PlaylistSource::Local => Self::Local,
        }
    }

    /// 界面用的编号。
    pub fn to_index(self) -> i32 {
        match self {
            Self::Liked => 0,
            Self::Platform => 1,
            Self::Local => 2,
        }
    }

    /// 由界面给的编号认回来。认不出的当平台歌单 ——
    /// 那是三者里唯一只读的,猜错了最多是取不到歌,不会误删本地数据。
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Liked,
            2 => Self::Local,
            _ => Self::Platform,
        }
    }
}

/// 一个歌单的副标题:它有多少首歌。
///
/// 在 Rust 侧格式化好再推给界面 —— `.slint` 里不做计算(见 types.slint)。
/// 空歌单说「暂无曲目」而不是「0 首」:后者读起来像个统计数字,
/// 而这里要说的是"点进去也没东西"。
pub fn track_count_text(count: i32) -> String {
    if count <= 0 {
        "暂无曲目".to_owned()
    } else {
        format!("{count} 首")
    }
}

/// 从 [`track_count_text`] 写出来的那句话里读回条数。
///
/// 读不出来就是 `None`。调用方据此**不动**那个数字 —— 凭空猜一个写上去,
/// 比留着一个旧的更难发现。
///
/// 「暂无曲目」也读不出来,这是**有意**的:它既对应 0,也对应上游给了个负数
/// (见 [`track_count_text`]),两者不该被读成同一个可加减的起点。
pub fn track_count_of(text: &str) -> Option<i32> {
    text.strip_suffix(" 首")?.parse().ok()
}

/// 「把刚才那批加进来」那行的文案。
///
/// 带上条数,因为进歌单那一刻列表已经换掉了 —— 不说清是哪一批,用户点下去
/// 才知道加了什么。空批返回空串,那一行整个不出现。
pub fn add_batch_text(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        format!("+ 把刚才那 {count} 首加进来")
    }
}

/// 「另有 N 首平台不再提供」那一行的文案。
///
/// 与 [`add_batch_text`] 同一条规矩:0 返回空串,那一行整个不出现。常态就是 0,
/// 一个恒显示的「另有 0 首」只会变成噪声。
///
/// 存在的理由是**别让歌单静默变短**:服务端把拿不到详情的曲目剔出成员关系
/// (见 server 的 `keep_available`),不说一声的话用户只看到数目对不上,
/// 而分不清「我少点了一个红心」和「平台不给这首歌的详情」。
///
/// 命中屏蔽规则被藏起来的(#161)也在这一行报:同一个理由,歌单对不上网易云时
/// 用户得知道是自己屏蔽的,不是平台少给了。
pub fn unavailable_text(
    count: usize,
    hidden: usize,
) -> String {
    let mut parts = Vec::new();
    if count > 0 {
        parts.push(format!("另有 {count} 首平台不再提供"));
    }
    if hidden > 0 {
        parts.push(format!("已隐藏 {hidden} 首"));
    }
    parts.join(" · ")
}

/// 这个来源的歌单能不能改。
///
/// 判据是**来源**不是名字:用户完全可以把一个本地歌单起名叫「我喜欢的」,
/// 而平台歌单与红心的真相不在这边,改名删歌都改不动。
pub fn is_editable(source: Source) -> bool {
    matches!(source, Source::Local)
}

/// 歌单排序的几种,界面编号即下标(ADR 0042)。次序由服务端排(`docs/adr/0021`),
/// 这里只记用户选了哪种、文案怎么写。
pub const SORTS: [(TrackSortDto, &str); 3] = [
    (TrackSortDto::Added, "加入时间"),
    (TrackSortDto::Score, "喜爱度"),
    (TrackSortDto::Plays, "播放次数"),
];

/// 界面编号 → 排序键。认不出的当加入时间:那是服务端的默认,也是老样子。
pub fn sort_from_index(index: i32) -> TrackSortDto {
    usize::try_from(index)
        .ok()
        .and_then(|index| SORTS.get(index))
        .map_or(TrackSortDto::Added, |(sort, _)| *sort)
}

/// 这个来源的歌单能不能换排序:「我的喜欢」与本地歌单是我们的,服务端按喜爱度、
/// 播放次数排得出来;平台歌单那条路不带排序。
pub fn is_sortable(source: Source) -> bool {
    matches!(source, Source::Liked | Source::Local)
}

/// 取某个歌单的曲目。三种来源各走各的路;`sort` 只对能排的两种有意义。
pub async fn tracks_of(
    source: Source,
    id: &str,
    sort: TrackSortDto,
) -> Result<TracksDto, api::ApiError> {
    match source {
        // 「我喜欢的」没有自己的 id —— 它是账号的属性,不是一个歌单实体
        Source::Liked => api::liked(sort).await,
        Source::Local => {
            api::playlist_tracks(id, sort).await
        }
        Source::Platform => {
            api::platform_playlist_tracks(id).await
        }
    }
}

/// 同 [`tracks_of`],但只读本地缓存里上次那份,不碰网络(#123)。
pub async fn cached_tracks_of(
    source: Source,
    id: &str,
    sort: TrackSortDto,
) -> Option<TracksDto> {
    match source {
        Source::Liked => api::cached_liked(sort).await,
        Source::Local => {
            api::cached_playlist_tracks(id, sort).await
        }
        Source::Platform => {
            api::cached_platform_playlist_tracks(id).await
        }
    }
}

/// 把契约里的歌单翻成界面要的一行。
///
/// 封面留空:它要一趟网络,而这个函数是同步的。图到了之后由
/// `crate::imagery::artwork::apply` 回填(见那个模块)。
pub fn to_row(list: &PlaylistDto) -> PlaylistRow {
    PlaylistRow {
        id: list.id.clone().into(),
        name: list.name.clone().into(),
        subtitle: track_count_text(list.track_count).into(),
        source: Source::from_dto(list.source).to_index(),
        cover: slint::Image::default(),
    }
}

#[cfg(test)]
mod tests;
