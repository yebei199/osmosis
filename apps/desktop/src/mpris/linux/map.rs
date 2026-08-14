//! 把 ui 那侧的快照翻成 MPRIS 的方言:状态名、循环名、轨道路径、元数据字典。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::{MICROS_PER_MILLI, NO_TRACK, TRACK_PREFIX};

pub(super) fn status_name(
    status: ui::MediaStatus,
) -> &'static str {
    match status {
        ui::MediaStatus::Playing => "Playing",
        ui::MediaStatus::Paused => "Paused",
        ui::MediaStatus::Stopped => "Stopped",
    }
}

/// 循环三态说成 MPRIS 的方言。取值规范钉死:None / Track / Playlist。
pub(super) fn loop_status_name(
    mode: ui::LoopMode,
) -> &'static str {
    match mode {
        ui::LoopMode::Off => "None",
        ui::LoopMode::All => "Playlist",
        ui::LoopMode::One => "Track",
    }
}

/// [`loop_status_name`] 的反向。不认识的值给 `None`(Option 的那个)。
pub(super) fn loop_mode_of(
    name: &str,
) -> Option<ui::LoopMode> {
    Some(match name {
        "None" => ui::LoopMode::Off,
        "Playlist" => ui::LoopMode::All,
        "Track" => ui::LoopMode::One,
        _ => return None,
    })
}

/// 曲目 id 变成一条合法的 object path。
///
/// object path 的每一段只认 `[A-Za-z0-9_]`,而平台 id 里 `-`、`.` 都常见。
/// 不换掉,D-Bus 会拒收**整条** Metadata —— 不是少一个字段,是 bar 上什么
/// 都不显示。
pub(super) fn track_path(id: &str) -> String {
    if id.is_empty() {
        return NO_TRACK.to_owned();
    }

    let mut path = String::with_capacity(
        TRACK_PREFIX.len() + id.len(),
    );
    path.push_str(TRACK_PREFIX);
    for ch in id.chars() {
        path.push(if ch.is_ascii_alphanumeric() {
            ch
        } else {
            '_'
        });
    }
    path
}

/// 一首歌的 Metadata。
pub(super) fn metadata_of(
    now: &ui::NowPlaying,
) -> HashMap<String, OwnedValue> {
    let mut map = HashMap::new();

    // 路径转不出来就整条不发:一条非法 object path 会让对面丢掉整个 Metadata,
    // 而 `track_path` 的产物按构造必然合法,走到这里说明前提被改坏了。
    let path = track_path(&now.track_id);
    match ObjectPath::try_from(path.clone()) {
        Ok(path) => {
            put(&mut map, "mpris:trackid", path.into())
        }
        Err(err) => {
            log::error!("曲目路径 {path} 不合法: {err}");
            return map;
        }
    }

    if now.track_id.is_empty() {
        return map;
    }

    put(&mut map, "xesam:title", now.title.clone().into());
    // 列表原样给出:`xesam:artist` 的类型就是字符串数组,join 成一句
    // 会让外面拿到一个名叫「甲/乙」的人。
    put(
        &mut map,
        "xesam:artist",
        now.artists.clone().into(),
    );
    put(
        &mut map,
        "mpris:length",
        (now.duration_ms * MICROS_PER_MILLI).into(),
    );
    // 没有封面就不写这个键 —— 给空串的话对面会当成一条取不到的图去拉。
    if let Some(url) = &now.art_url {
        put(&mut map, "mpris:artUrl", url.clone().into());
    }

    map
}

/// 往 Metadata 里放一个键。转不成 `OwnedValue` 的就不放:宁可少一个键,
/// 也不要让整条 Metadata 发不出去。
pub(super) fn put(
    map: &mut HashMap<String, OwnedValue>,
    key: &str,
    value: Value<'_>,
) {
    match OwnedValue::try_from(value) {
        Ok(value) => {
            map.insert(key.to_owned(), value);
        }
        Err(err) => {
            log::warn!("Metadata 的 {key} 放不进去: {err}");
        }
    }
}

#[cfg(test)]
mod tests;
