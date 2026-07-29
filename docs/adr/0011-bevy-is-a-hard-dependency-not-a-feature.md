# 0011. bevy 是渲染层,不是可选页面

日期:2026-07-29
状态:已接受。取代 0005 中「3D 由 feature 门控」的部分,与 0010 同一条线。

## 背景

bevy 进本仓库时,身份是「Slint 做不来的那一块」:一个可以关掉的 3D 演示页,挂在
`bevy-3d` feature 后面。桌面、android、web 三端各声明一份同名 feature,关掉就退回
`ui::run()`,界面照常可用。围绕这个可关性建了一整套东西:`render3d` 被排除在
`default-members` 之外好让 IDE 的 `cargo check` 别编 bevy;`render3d.nix` 与 `slint.nix`
两个 shell 分管「带 3D 跑」和「普通跑」;justfile 里 `desktop-dev` / `desktop-dev-3d`、
`android-build` / `android-build-3d` 各有一对。

后来播放页(0010)把 warp 反馈视觉、粒子场、深度遮挡全放进了 `render3d`,导航栏的
metaball 选中器也放了进去。于是「关掉 bevy」的那条路径悄悄变了性质:它不再是「少一个
演示页」,而是「导航栏没有选中器、播放页没有粒子也没有 warp」—— 一个没人会想跑、
也从没有人验证过的降级形态。

## 决定

**桌面与 android 硬依赖 `render3d`,`bevy-3d` feature 取消。** 同时删掉 3D 演示页:
它是最后一个「只为展示 3D 而存在」的界面,而它展示的那套机制(深度正确的遮挡)
现在由播放页真正在用。

web 与 ios 仍然不碰 bevy,由 `xtask boundaries` 守住 —— 这条边界不但没松,反而更硬了:
web 上曾有过一个 `bevy-3d` feature,现在一个也没有。

## 理由

- **可关的开关必须有人验证。** 一个从不被构建、从不被截图、从不被测试的 cfg 分支,
  是负债不是灵活性。三端各一份同名 feature,乘上 `mcp`,组合面已经没人跑得全。
- **降级路径已经名不副实。** 关掉之后剩下的不是「同样的应用,少个 3D 页」,而是一个
  缺了三处视觉的半成品。与其维护一个假的降级形态,不如承认 bevy 是渲染层的一部分。
- **排除 `render3d` 已经拦不住什么。** `apps/desktop` 一旦硬依赖它,裸 `cargo check`
  照编 bevy —— 那道排除只剩下「让 `cargo test` 漏掉 render3d 的单测」这一个效果。

## 代价

- **IDE 的 `cargo check` 每次都编 bevy。** 首次慢一次,之后走缓存;dev profile 已经把
  依赖编成优化版(见根 `Cargo.toml` 的 `profile.dev.package."*"`)。
- **桌面开发从此需要 vulkan 运行期库。** `vulkan-loader` 并进 `slint.nix`,`render3d.nix`
  删除。无 GPU / headless 的机器跑不了桌面窗口 —— 这一条 0005 里已经记过,现在成了唯一形态。
- **web 暂时失去了 3D 能力。** 不是遗憾,是把事实写明:wasm 上没有原生音频栈
  (`ui::viz::Source` 恒为 `None`),播放页视觉在 web 上永远不会开门,留着 feature 只会
  编出一份不会被画的 bevy。等 web 的播放链路通了再接。

## 一并删掉的东西

3D 演示页、它的热调参数工具条、那套参数解析(`ui::scene_params`)、液态玻璃后处理
(`render3d::glass` + `glass.wgsl`,唯一消费者是工具条)、相机随长宽比后撤的
`pullback`(唯一消费者是演示页的转盘;粒子场明确恒用基准距离)。

玻璃后处理那套架构本身没有作废,它的结论记在
[`docs/slint/visual-effects-and-shaders.md`](../slint/visual-effects-and-shaders.md) 第五节 ——
包括「后处理要长进 bevy 的渲染管线、别在旁边自己起一趟」,以及「那次重构不是安卓闪黑的
修复,关掉玻璃闪黑照旧」这两条踩出来的结论。
