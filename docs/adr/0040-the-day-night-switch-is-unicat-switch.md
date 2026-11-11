# 日夜开关是 unicat-switch,像素在 CPU 上画、按需出帧

日期:2026-11-11。状态:已采纳。遵循 [`0027`](0027-slint-state-lives-in-globals.md) 与
[`0035`](0035-render-on-demand-with-one-wake-path.md)。

## 背景

设置页主题区的主视觉原是 `textfx.slint` 里的 `DayNightSwitch`:纯 Slint 拼出的天空、云、
星与日月,参考一个 QML 演示。用户自己的仓库 unicat-switch 有一个更完整的版本(猫、
天色、弹簧、拖动),并且已经有 Slint 移植 `unicat-switch-slint`。

## 决策

- 依赖 `unicat-switch-slint`,git 地址钉在一个 rev 上(`crates/ui/Cargo.toml`,只写远程,
  与 `[patch.crates-io]` 同一条规矩)。它依赖 crates.io 的 slint 1.18,经根 `Cargo.toml`
  的 patch 统一到同一份 fork:`Cargo.lock` 里只有一个 `slint`。
- `UnicatSwitch` 组件经 `build.rs` 的 library path `@unicat` 引入;它的 `tick` 与
  `render`/`input` 回调接到 `DayNight` 全局(`slint/globals/daynight.slint`),Rust 侧在
  `crates/ui/src/motion.rs` 绑 `Driver`。状态在 global,不在 `MainWindow` 根上。
- 出帧走它自己的定时器:弹簧在动时 16ms 一拍把 `DayNight.tick` 加一,属性变了 Slint 才重画
  那一块;弹簧停了定时器就停。不经 `runtime::pace`,因为它从不在属性之外改画面。
- 「动态效果」关掉时 `Driver::set_reduced_motion(true)`,与 `Motion.calm` 同一个开关。
- 开关自己翻 `night`(拨动先翻、再喊 `toggled`);设置页把它对回 `Theme.dark`,
  档位真相仍在 `api::settings`。删除 `DayNightSwitch`。

## 备选方案

**把 unicat 的画面翻成 Slint 图元。** 不选:天色、猫、玻璃都是逐像素算的,翻成矩形与渐变
只会得到又一个近似版,两份实现从此各走各的。

## 后果

- 多一个 git 依赖与它的 `png`;开关像素在 CPU 上算,只在弹簧动的那半秒内有开销。
- 软件渲染器(无头测试)上没有阴影,开关周围的光晕只在 Skia 上可见。
