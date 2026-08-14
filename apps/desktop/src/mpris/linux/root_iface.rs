//! 根接口 `org.mpris.MediaPlayer2`:这个播放器是谁,能拿它怎么办。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

/// 根接口:这个播放器是谁,能拿它怎么办。
pub(super) struct Root;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    /// 规范要求有,但我们没有「把窗口拉到前面」这条路 —— `CanRaise` 为假,
    /// 规矩的客户端不会调它。
    fn raise(&self) {}

    /// 同上。退出由用户在应用里做,不由 bar 上的按钮做。
    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "Osmosis".to_owned()
    }

    /// `.desktop` 文件的 stem。规范让报,外壳拿它回头找这个应用的条目。
    ///
    /// 与 `assets/io.github.osmosis.desktop` 的文件名绑死,改一个要同时改
    /// 另一个 —— 报一个不存在的 id,查它的外壳只会查空。
    ///
    /// **别指望它换来媒体卡片上的图标。** 各家外壳拿它做什么各不相同,
    /// 本机这条 DMS bar 压根不看:`Modules/DankBar/Widgets/Media.qml` 里那个
    /// 图标写死是 Material 的 `music_note`,所有播放器一个样;它唯一用到
    /// `desktopEntry` 的地方是排除名单的字符串匹配。GNOME 与 KDE 的媒体
    /// 控件确实按这个 id 取图标,报它是为了那些外壳。
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "io.github.osmosis".to_owned()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}
