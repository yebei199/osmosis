# desktop 渲染器从 femtovg 切到 skia

日期:2026-07-12。状态:已采纳。
**部分取代 [ADR-0005](0005-wgpu-device-as-shared-base.md)** —— 该 ADR 的「wgpu-29 device 作全端
统一基座」这一核心不变;被取代的只是它里面「desktop 用 femtovg-on-wgpu」那一句。

## 背景

给 UI 加玻璃拟态(`crates/ui/slint/glass.slint`)时撞上一个硬约束:
**`inner-shadow-*` 只在 Skia 渲染器上实现**(Slint 1.17 新增,见
`docs/slint/slint-1.17-release-notes.md`),而 desktop 当时跑的是 `renderer-femtovg`。

内阴影不是可有可无的装饰:它画的是**沿边框内侧一整圈的阴影环**,`offset-y` 取负即可做出
"光从上方来、玻璃底部内侧留一道反光"—— 液态玻璃最抓眼的那道边。用 `@linear-gradient` 叠层
只能模拟线性亮度过渡,**圆角拐弯处那圈连续的光环凑不出来**(方向性属于矩形,不属于边框形状)。

## 决定

`apps/desktop` 的 slint 特性从 `renderer-femtovg` 改为 `renderer-skia`。
`bevy-3d` 特性同步去掉 `slint/renderer-femtovg-wgpu`,只保留 `slint/unstable-wgpu-29`。

**没有 `renderer-skia-wgpu` 这种 feature** —— skia 的 wgpu 路径就是
`renderer-skia` + `unstable-wgpu-29`,与 `apps/android` 现有组合一字不差
(android 的后端本就强制 skia)。所以纹理共享的前提不受影响,`render3d` 一行没动。

## 理由

1. **inner-shadow 只有这一条路。** 见上。
2. **顺带统一了 desktop 与 android 的渲染器。** android 本来就是 skia,切之前两端在文字与
   Path 抗锯齿上本就有细微差异;切完只剩 web 是异类(且那是上游边界,不是我们的选择)。
3. **文字与 Path 质量更好。** 上游自己承认 femtovg 的 text/path quality sometimes sub-optimal。

## 代价(照单收下)

- **skia 静态库首次编译很慢、二进制显著变大。** 本仓库因 android 已编过 skia,切换时命中缓存,
  实际成本远低于预期 —— 但对 clean checkout 不成立。
- **desktop 与 web 视觉不完全一致:web 上没有 inner-shadow。** wasm 编不了 skia,`apps/web`
  只能是 `renderer-femtovg`。玻璃卡片在 web 上少一层厚度感,**不报错、不塌布局** ——
  渐进增强,与本仓库既有的"余端 graceful 缺省"(`scene-3d.width > 0` 空图守卫)同一套。
- **`renderer-software` 仍需保留** —— `mcp` 特性依赖它。

## 未采纳的替代方案

- **不切,用 `@linear-gradient` 叠层模拟厚度。** 零成本、全平台一致,但凑不出圆角处的连续光环,
  且底部反光要叠三层才勉强像。为一个内阴影不值得切渲染器 —— 但"顺带统一 android"这条独立的
  收益把天平压过去了。
- **两套都写(基础渐变 + inner-shadow 渐进叠加)。** 会让 desktop(femtovg)与 android(skia)
  长得不一样,调参时两边永远对不齐。

## 相关

- `docs/slint/visual-effects-and-shaders.md` —— Slint 视觉效果上限的完整调研,含 backdrop blur
  与自定义 shader 的上游现状。
- `crates/ui/slint/glass.slint` —— inner-shadow 的实际用法与"仅 Skia"的注释落点。
