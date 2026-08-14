//! 目录侧的只读请求:健康检查、搜索、播放直链、每日推荐、红心与歌词。

use contract::{
    ArtistSearchDto, HealthDto, LyricDto, PROTOCOL_VERSION,
    PlaySourceDto, PlaylistSearchDto, SearchDto, TracksDto,
};

use crate::url::{
    artist_tracks_url, lyric_url, play_url, search_url,
};
use crate::{ApiError, base_url, platform};

/// `GET /health`。
///
/// 校验协议版本是 `contract` 存在的意义所在:服务端换了线上格式而客户端没跟上时,
/// 这里立刻报错,而不是让某个字段静默地变成默认值。
pub async fn health() -> Result<HealthDto, ApiError> {
    let dto: HealthDto = platform::get_json(format!(
        "{}/health",
        base_url()
    ))
    .await?;

    check_version(dto)
}

/// 版本校验本身。从 [`health`] 里抽出来,好让它离开网络单独被测 ——
/// `base_url()` 是编译期常量,同一进程内无法把请求指向一个版本不同的假服务端。
///
/// 这条分支在本仓库里是**可达的**:手机上装着的旧 APK 焊死了它编译那一刻的
/// [`PROTOCOL_VERSION`],而开发机上的 server 每次都从当前源码重新编译。
fn check_version(
    dto: HealthDto,
) -> Result<HealthDto, ApiError> {
    if dto.protocol_version != PROTOCOL_VERSION {
        return Err(ApiError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            actual: dto.protocol_version,
        });
    }
    Ok(dto)
}

/// `GET /search/tracks?q=…`。
pub async fn search_tracks(
    keyword: &str,
) -> Result<SearchDto, ApiError> {
    platform::get_json(search_url("tracks", keyword)).await
}

/// `GET /search/artists?q=…`。
pub async fn search_artists(
    keyword: &str,
) -> Result<ArtistSearchDto, ApiError> {
    platform::get_json(search_url("artists", keyword)).await
}

/// `GET /search/playlists?q=…`。
///
/// 只搜平台的歌单。本地歌单已经在手上,过滤是界面的事。
pub async fn search_playlists(
    keyword: &str,
) -> Result<PlaylistSearchDto, ApiError> {
    platform::get_json(search_url("playlists", keyword))
        .await
}

/// `GET /artists/{id}/tracks` —— 某个歌手的热门曲目。
///
/// 搜到的歌手点下去听什么。不是这个歌手的全部作品 —— 平台给的就是「此刻热门」
/// 那几首,要全部得另开一条路。
pub async fn artist_tracks(
    artist_id: &str,
) -> Result<TracksDto, ApiError> {
    platform::get_json(artist_tracks_url(artist_id)).await
}

/// `GET /play/{track_id}`。
///
/// 拿到的是一条**临时**直链,带签名会过期。别缓存 —— 过期后服务端返回的
/// 是一个 HTML 错误页,解码那头会报"这不是音频",症状离病因很远。
pub async fn play_source(
    track_id: &str,
) -> Result<PlaySourceDto, ApiError> {
    platform::get_json(play_url(track_id)).await
}

/// `GET /daily` —— 今日推荐。
pub async fn daily() -> Result<TracksDto, ApiError> {
    platform::get_json(format!("{}/daily", base_url()))
        .await
}

/// `GET /liked` —— 我喜欢的音乐,取第一页。
///
/// 服务端不带 limit 时用它自己的默认页大小,客户端不必知道那个数字。
pub async fn liked() -> Result<TracksDto, ApiError> {
    platform::get_json(format!("{}/liked", base_url()))
        .await
}

/// `GET /lyric/{track_id}`。
///
/// 没有歌词(纯音乐、上游未收录)时给**空行表**而不是错误 —— 这条语义从
/// 服务端一路保持到这里,客户端据此隐藏歌词区,不必把它当故障处理。
pub async fn lyric(
    track_id: &str,
) -> Result<LyricDto, ApiError> {
    platform::get_json(lyric_url(track_id)).await
}

#[cfg(test)]
mod tests;
