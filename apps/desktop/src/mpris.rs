//! Linux 这一端的系统媒体控件:session bus 上的 MPRIS。
//!
//! 占住 `org.mpris.MediaPlayer2.osmosis` 这个名字,实现 `org.mpris.MediaPlayer2`
//! 与 `org.mpris.MediaPlayer2.Player` 两个接口。DMS/quickshell、waybar、GNOME 的
//! 锁屏控件认的都是它。
//!
//! 只说方言,不记状态 —— 该记的都在 `ui::media` 那一侧(见 `docs/adr/0020`)。
//! 这里存的那份 `NowPlaying` 不是第二个真相,是**上一句话的副本**:D-Bus 的属性
//! 是被拉的,对面什么时候来问不由我们决定,总得有个东西接得住那一问。

/// 别的桌面(Windows / macOS)还没有实现。
#[cfg(not(target_os = "linux"))]
pub fn start(
    _hooks: ui::MediaHooks,
) -> Box<dyn ui::MediaControls> {
    Box::new(ui::NoControls)
}

#[cfg(target_os = "linux")]
pub use linux::start;

#[cfg(target_os = "linux")]
mod linux;
