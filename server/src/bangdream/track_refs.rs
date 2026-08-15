//! 曲目标识集合的两个运算:上游少给了哪些详情,以及哪些成员关系写得进去。

use std::collections::HashSet;

use contract::TrackDto;

use crate::cache::TrackRef;

/// 上游的歌单详情少给了哪些曲目的详情。
///
/// 歌单详情一次会带回一批完整曲目,但那一批**会被平台截断**,而标识列表是全量的。
/// 够全时一次补拉都不必发;截断了就只补差额 —— 不是整份重取。
///
/// 返回的次序跟着 `refs` 走,便于调用方按批切片。
pub fn refs_missing_from(
    detail_tracks: &[TrackDto],
    refs: &[TrackRef],
) -> Vec<String> {
    let present: HashSet<&str> = detail_tracks
        .iter()
        .map(|track| track.id.as_str())
        .collect();

    refs.iter()
        .filter(|track| {
            !present.contains(track.id.as_str())
        })
        .map(|track| track.id.clone())
        .collect()
}

/// 剔掉平台给不出详情的那些成员关系,并报出剔了几条。
///
/// 成员关系必须先有详情,否则外键会拒绝(见 `0004` 那条注释)。所以拿不到详情的
/// 曲目只能不写进去 —— 但**必须报出来**:不报的话歌单会静默变短,而用户看到的
/// 只是数目对不上,分不清「我少点了一个红心」和「平台不给这首歌的详情」。
///
/// 次序跟着 `refs` 走,过滤不重排。
pub fn keep_available(
    refs: &[TrackRef],
    unavailable: &HashSet<String>,
) -> (Vec<TrackRef>, usize) {
    let known: Vec<TrackRef> = refs
        .iter()
        .filter(|track| !unavailable.contains(&track.id))
        .cloned()
        .collect();

    // 用差值而不是 `unavailable.len()`:那个集合里可能有不属于这个歌单的 id,
    // 拿它当条数会报出一个用户在这张列表上永远对不上的数字。
    let dropped = refs.len() - known.len();

    (known, dropped)
}

#[cfg(test)]
mod tests;
