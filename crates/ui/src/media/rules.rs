//! 与状态无关的换算:开关翻不翻、循环模式的编号,以及跳转目标与比例。

use app_core::LoopMode;

// 下面这些只有原生那半边用得到 —— wasm 上界面在、播放不在,那半边整个不存在
// (见下方「以下到文件末尾是原生那一半」)。

use super::seam::MediaCommand;

/// 这个键该不该翻转播放状态。
///
/// 界面只有一个「切换」回调。把 `Play` 一律当成切换,正在放的歌会被按停 ——
/// 锁屏上最容易误触的就是它。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn toggles(
    command: MediaCommand,
    playing: bool,
) -> bool {
    match command {
        MediaCommand::Toggle => true,
        MediaCommand::Play => !playing,
        MediaCommand::Pause => playing,
        _ => false,
    }
}

/// 这个键该不该翻转随机开关。
///
/// 与 [`toggles`] 同一道翻译:外面给的是绝对值,界面只有一个切换回调。
/// 值本来就一样还去调一次,开关会翻到反面 —— 而按下它的人什么都没要求。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn flips_shuffle(
    command: MediaCommand,
    on: bool,
) -> bool {
    matches!(command, MediaCommand::SetShuffle(want) if want != on)
}

/// 这个键要把循环拨到哪一态。与 [`flips_shuffle`] 同一道翻译:
/// 值本来就一样就不折腾 —— 按下它的人什么都没要求。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn wants_loop(
    command: MediaCommand,
    current: LoopMode,
) -> Option<LoopMode> {
    match command {
        MediaCommand::SetLoop(want) if want != current => {
            Some(want)
        }
        _ => None,
    }
}

/// slint 侧的循环三态镜像:0 关,1 列表,2 单曲。
/// builtin 之外的枚举同样过不了 seam,int 是那根线的形状。
pub(crate) fn loop_index(mode: LoopMode) -> i32 {
    match mode {
        LoopMode::Off => 0,
        LoopMode::All => 1,
        LoopMode::One => 2,
    }
}

/// [`loop_index`] 的反向。界面写坏了也不 panic,当作关。
pub(crate) fn loop_from_index(index: i32) -> LoopMode {
    match index {
        1 => LoopMode::All,
        2 => LoopMode::One,
        _ => LoopMode::Off,
    }
}

/// 这个键要跳到的绝对位置,毫秒。不是跳转键就没有答案。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn seek_target(
    command: MediaCommand,
    position_ms: i64,
) -> Option<i64> {
    match command {
        MediaCommand::SeekTo(at) => Some(at.max(0)),
        // 往回跳过了头就落到开头:负的绝对位置没有意义。
        MediaCommand::SeekBy(by) => {
            Some(position_ms.saturating_add(by).max(0))
        }
        _ => None,
    }
}

/// 绝对位置换成 Slint 的 `seek` 要的比例(见 `app.slint:74`)。
///
/// 时长为 0 —— 还没装起来,或上游没给 —— 时这次跳转没有意义,该被丢掉而不是除零。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn seek_ratio(
    target_ms: i64,
    duration_ms: i64,
) -> Option<f32> {
    if duration_ms <= 0 {
        return None;
    }

    let ratio = target_ms as f64 / duration_ms as f64;
    Some(ratio.clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests;
