//! 播放页效果的选择:封面实物(默认)或点云。
//!
//! 选择在界面上即时生效 —— `.slint` 点下去就改 `Viz.viz-effect`,渲染循环下一帧把它
//! 交给 render3d —— 这里只管「记住」:启动时从 `api::settings` 恢复,选了就落盘。
//! 跟着设备走,与主题、动态效果同一份文件。
//!
//! 编号是界面那一侧的顺序(`viz-effect-names`),落盘写的是名字([`VizEffect`]),
//! 两者的对照只在这一处。

use api::settings::{self, Settings, VizEffect};
use slint::ComponentHandle;

use crate::{MainWindow, Viz};

/// 恢复上次的选择,接上「选了一档」的落盘。
pub(crate) fn bind(ui: &MainWindow) {
    let viz = ui.global::<Viz>();
    viz.set_viz_effect(index_of(
        settings::load().viz_effect,
    ));
    viz.on_viz_effect_chosen(|index| {
        settings::save(&Settings {
            viz_effect: effect_at(index),
            ..settings::load()
        });
    });
}

/// 效果 → 界面上的编号。
fn index_of(effect: VizEffect) -> i32 {
    match effect {
        VizEffect::Object => 0,
        VizEffect::Cloud => 1,
    }
}

/// 界面上的编号 → 效果。越界落默认(封面实物),与 render3d 的兜底一致。
fn effect_at(index: i32) -> VizEffect {
    match index {
        1 => VizEffect::Cloud,
        _ => VizEffect::Object,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个方向互为反函数 —— 存进去的是哪一档,读出来就落在哪一档上。
    #[test]
    fn every_effect_round_trips_through_its_index() {
        for effect in [VizEffect::Object, VizEffect::Cloud]
        {
            assert_eq!(effect_at(index_of(effect)), effect);
        }
    }

    /// 默认效果排在第 0 位:`.slint` 的 `viz-effect` 初值是 0。
    #[test]
    fn the_default_effect_is_index_zero() {
        assert_eq!(index_of(VizEffect::default()), 0);
    }

    /// 越界的编号落默认,不存一个不存在的效果。
    #[test]
    fn an_out_of_range_index_saves_the_default() {
        for index in [-1, 2, 99] {
            assert_eq!(effect_at(index), VizEffect::Object);
        }
    }
}
