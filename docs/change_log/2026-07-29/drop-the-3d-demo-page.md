# 删掉 3D 演示页,bevy 从可选 feature 变成硬依赖

## 1. Change Purpose

feature 面已经超出能验证的范围:桌面、android、web 三端各声明一份 `bevy-3d`,web 另有
`wgpu` 与 `repro`,乘上 `mcp`,组合数没人跑得全,其中多数分支从没被构建过。

追问「哪些是真开关」时得到的答案是:`bevy-3d` 不是。播放页(ADR 0010)把 warp 视觉、
粒子场、深度遮挡放进 `render3d` 之后,导航栏的 metaball 选中器也在里面 —— 关掉 bevy
剩下的不是「同样的应用少个 3D 页」,而是缺三处视觉的半成品。而 3D 演示页本身,是最后
一个「只为展示 3D 而存在」的界面,它展示的那套机制现在由播放页真正在用。

决定见 [`docs/adr/0011`](../../adr/0011-bevy-is-a-hard-dependency-not-a-feature.md)。

## 2. Change Scope

**先追平 fork**(独立提交,好让后面的删除不与它混在一起查):`Cargo.lock` 从
`f6bfe29` 到 `dc8f4086`,四条本地补丁随上游 rebase 换了哈希,内容一条不少;顺带修掉
新版本报出的 `viewport-height` 弃用,并改正 `Cargo.toml` 里 femtovg 分支名的过期注释。

**删 3D 演示页**

- `crates/render3d`:删 `glass.rs` + `glass.wgsl`(液态玻璃后处理,唯一消费者是演示页的
  热调工具条)、`SceneParams` / `Content` 枚举 / `render_frame` / `rebuild_content` /
  `compute_placements` / `build_mesh_palette`,以及相机后撤那一组(`pullback` /
  `camera_pos` / `REFERENCE_ASPECT` / `MAX_PULLBACK` 与它们的三条单测)—— 粒子场明确
  恒用基准距离,后撤只服务演示页的转盘。`render_viz_frame` 成为 bevy 侧唯一驱动入口,
  主相机的透明清屏与相机位姿因此可以在 spawn 时定死,`set_camera_clear` / `apply_cameras`
  一并消失。
- `crates/ui`:删 `scene_params.rs` 与 `SceneControls`;`run_with_renderer` 整个删除
  (唯一调用方是 web),`run_with_renderers` 去掉 3D 闭包与 `initial_tab` 两个参数;
  `MAX_TAB` 2 → 1。`app.slint` 删 3D 页的三块内容(画面 Image、遮挡层、工具条 GlassCard)、
  `ParamField` / `ParamRow` 两个组件、以及 `scene-3d` / `occluder-3d` / `rot-*` /
  `scene-index` / 四个 `*-text` / `scene-w/h` / `glass-*` / `card-*` / `render-active`
  这一组属性,1349 → 1064 行。
- `apps/desktop`、`apps/android`:`cfg(feature = "bevy-3d")` 分支去掉,`Scene` 不再需要
  `Rc<RefCell>` 共享(只剩 viz 一个消费者),`render3d` 改成非 optional 依赖,
  `slint/unstable-wgpu-29` 移进常规 feature 列表。
- `apps/web`:整个 bevy 链路删除,连同 `wgpu` 与 `repro` 两个 feature、`render3d` /
  `wasm-bindgen-futures` / `web-sys` / `log` 四个依赖。入口回到六行的 `ui::run()`。

**构建配置**

- `render3d` 列回 `default-members`:`apps/desktop` 硬依赖它之后,那道排除只剩「让
  `cargo test` 漏掉它的单测」这一个效果。`ci-test` 里为它单开的两条命令随之删除。
- `render3d.nix` 删除,`vulkan-loader` 并进 `slint.nix` —— 此前「带 3D 跑」缺 alsa、
  「有声音跑」缺 vulkan,是两个互斥的 shell,踩过一次。
- justfile 25 → 19 条:`desktop-dev-3d`、`android-build-3d` 并回主配方;
  `android-flicker`、`web-test`、`server-test`、`bang-dream-test` 按实际使用情况删除
  (后两条背后的 5 个 `#[ignore]` 测试保留,模块头注释里记着怎么跑)。
- `test/e2e/` 整个删除:1 条帧率 spec + `probes/` 下 24 个一次性探针。帧率排查已结案,
  结论在 `docs/wasm/`,补丁在 slint fork 里。同目录的两个纯 HTML 对照页保留。

## 3. Implementation Process

按「先追平基线、再删代码、最后清构建配置」的顺序拆提交。中途有一处推翻了原计划:

原本要保留 web 的 `bevy-3d`(理由是 WebGPU 不保证可用,得留降级路径)。动手时才发现
web 的 bevy **唯一渲染的东西就是 3D 演示页** —— 它走 `run_with_renderer`,不传 nav 闭包
也不传 viz 闭包;而 wasm 上 `ui::viz::Source` 是 `Option<Infallible>`,`payload()` 恒返回
`None`,播放页视觉在 web 上永远不会开门。演示页一删,那个 feature 会编出一整个 bevy 却
没有任何一帧被画出来。于是连它一起删掉。

## 4. Verification

- `cargo check --all-targets`、`cargo check -p app-web --target wasm32-unknown-unknown`
- `cargo clippy --all-targets -- -D warnings`(默认成员 + audio/syncplay/server/xtask 两轮)
- `cargo xtask boundaries`
- `cargo test`(默认成员,现含 render3d)+ `cargo test -p audio -p syncplay -p server -p xtask`

`cargo check -p app-ios --target aarch64-apple-ios` 在本机跑不了(`ring` 的 build script
要 `xcrun`,NixOS 上没有 Apple SDK),与本次改动无关 —— 这次的 lock 变更只碰了 slint 那一组包。

## 5. 留下的东西

液态玻璃后处理的架构没有作废,只是没有消费者了。两条踩出来的结论搬进了
[`docs/slint/visual-effects-and-shaders.md`](../../slint/visual-effects-and-shaders.md) 第五节:

1. 后处理要长进 bevy 的渲染管线,别在旁边自己起一趟提交 —— 改成 `FullscreenMaterial`
   之后链上只剩一张纹理、一个写入方,净删 239 行。
2. 那次重构一度被当成安卓闪黑的修复,**该说法不成立**:把玻璃整个关掉,闪黑照旧出现,
   比率还略高(9/1256 对 6/1534)。性能重构与渲染时序 bug 长得像,判定因果只能靠关掉
   变量重测。
