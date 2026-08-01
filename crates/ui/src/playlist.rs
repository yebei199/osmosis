//! 歌单列表与详情的绑定。
//!
//! 三种来源的歌单在界面上是同一张列表(见 `docs/adr/0016`),但**取曲目的路
//! 不同**:「我喜欢的」是平台的红心列表,本地歌单在自家库里,平台歌单要问上游。
//! 走错的现象是点开一个歌单看到另一个歌单的歌 —— 而两边都不报错。

use app_core::{PlaylistDto, PlaylistSource, TrackDto};
use slint::ComponentHandle;

use crate::{MainWindow, PlaylistRow};

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

/// 取某个歌单的曲目。三种来源各走各的路。
pub async fn tracks_of(
    source: Source,
    id: &str,
) -> Result<Vec<TrackDto>, api::ApiError> {
    match source {
        // 「我喜欢的」没有自己的 id —— 它是账号的属性,不是一个歌单实体
        Source::Liked => {
            api::liked().await.map(|dto| dto.tracks)
        }
        Source::Local => api::playlist_tracks(id)
            .await
            .map(|dto| dto.tracks),
        Source::Platform => {
            api::platform_playlist_tracks(id)
                .await
                .map(|dto| dto.tracks)
        }
    }
}

/// 把契约里的歌单翻成界面要的一行。
pub fn to_row(list: &PlaylistDto) -> PlaylistRow {
    PlaylistRow {
        id: list.id.clone().into(),
        name: list.name.clone().into(),
        subtitle: track_count_text(list.track_count).into(),
        source: Source::from_dto(list.source).to_index(),
    }
}

/// 拉一次歌单列表,填进界面。
pub fn refresh(ui: &MainWindow) {
    let weak = ui.as_weak();

    let _ = slint::spawn_local(async move {
        let found = api::playlists().await;
        let Some(ui) = weak.upgrade() else { return };

        match found {
            Ok(dto) => {
                let rows: Vec<PlaylistRow> =
                    dto.playlists.iter().map(to_row).collect();
                ui.set_playlists(
                    slint::ModelRc::new(
                        slint::VecModel::from(rows),
                    ),
                );
            }
            Err(err)
                if crate::account::handle_session_expiry(
                    &ui, &err,
                ) => {}
            Err(err) => ui.set_playback_text(
                format!("取歌单失败: {err}").into(),
            ),
        }
    });
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            Source::from_index(99),
            Source::Platform
        );
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
}
