# 桌面与安卓回归:femtovg 改动的影响面

结论:**桌面与安卓不受影响,因为它们的依赖图里根本没有 femtovg。**
web 端的排查与修复见 [`femtovg-wgpu-per-element-cost.md`](femtovg-wgpu-per-element-cost.md)。

## 为什么结构上就不可能受影响

| 端 | Slint 渲染器 |
| --- | --- |
| web | `renderer-femtovg` |
| 桌面 | `renderer-skia` |
| 安卓 | `backend-android-activity-06`(同样是 skia) |

```sh
cargo tree -p app-desktop --features bevy-3d -i femtovg   # nothing to print
cargo tree -p app-android --features bevy-3d --target aarch64-linux-android -i femtovg
```

两条都是 `nothing to print`:femtovg 不在这两端的依赖图里。

不过"依赖图里没有"与"构建仍然通过"是两件事。patch 改的是 slint fork 的
`internal/renderers/femtovg/Cargo.toml`,那份 manifest 三端共用,所以仍然要各跑一遍。

## 实测

**桌面**(`just shot "" 2`,144Hz 屏):3D 页 **144fps**,画面与改动前逐处一致:
导航栏图标与文字、玻璃工具条的模糊与阴影、3D 场景、中文文案全部正常。

**安卓**(小米 13,120Hz 屏,`just android-build-3d` + 直装):画面正常。帧率分两种情形:

| 情形 | 帧率 | 一帧的构成 |
| --- | --- | --- |
| 画面静止(自转速度 0.0,不触摸) | 2fps | 回调 28ms + Slint 渲染 1.7ms + **空等 471ms** |
| 持续拖动模型 | **97~106fps** | 回调 6.1ms + Slint 渲染 0.7ms + 空等 2.8ms |

**静止时的 2fps 不是缺陷**,是 MIUI 的自适应刷新率:画面没有变化就把刷新降到极低。
一旦有持续变化就回到百来帧。量安卓帧率时必须让画面真的在动,否则量到的是省电策略。

拖动时的 ~104fps 受限于回调里的 6.1ms(bevy 的 `app.update()`),不是 Slint 的
0.7ms。要往 120 推该去减 bevy 的每帧开销。

## 顺带查出并修掉的一个真缺陷

`build_ui` 本来就读 `SLINT_STUDY_TAB`,`just shot 420 2` 靠它直接截到 3D 页。给
`run_with_renderer` 加 `initial_tab` 参数时,我在 `build_ui` 之后又 `set_current_tab`,
于是桌面和安卓传的 0 把环境变量覆盖掉了,截图全部回到 Home 页。

现在页签只在一处决定:参数给默认值,环境变量覆盖它。

这正是回归测试的价值。缺陷与被测的那个改动毫无关系,是同一批改动里另一处的副作用。

## 测量时踩到的两件事

`adb devices` 报 `no permissions`:`adb kill-server && adb start-server` 即可,不必动
udev 规则。

安卓上没有 `SLINT_STUDY_TAB` 这条路(`am start` 传不了环境变量),切页只能按坐标点:

```sh
adb shell wm size                    # 1080x2400
adb shell input tap 895 2196         # 底部导航第三项
```

## 安卓 3D 页拖动时闪黑(未修完)

现象:3D 页上触摸时,一片黑色从某一行往下盖到内容区底部,逐帧移动,看着像黑条扫下去。
桌面没有。

### 证据

用 `adb shell screenrecord` 录 8 秒,逐帧算行亮度,统计"大片变暗且一直暗到内容区底部"的帧:

| 条件 | 帧数 | 黑到底的帧 |
| --- | --- | --- |
| 3D 页,自转、不触摸 | 238 | 1 |
| 3D 页,拖模型 | 240 | 11 |
| 3D 页,拖工具条 | 243 | 10 |
| Home 页,拖动 | 236 | 0 |
| **3D 页,玻璃 pass 关掉** | **259** | **0** |

触摸是触发条件,画面在动不是。任何位置的触摸都触发,不限于模型区。

render3d 侧打桩实测,拖动全程 **0 次 resize、0 次取不到纹理、0 次重新导入**:每帧交给
Slint 的都是同一张有效纹理。故障不在我们产出的内容里。

### 已否掉的假设

**读-改-写。**玻璃 pass 原本用 `LoadOp::Load`,而 bevy 的相机每帧清屏。两张纹理都是单缓冲、
都被 skia 采样,只有前者闪,所以怀疑是 `Load` 去读旧内容时与 skia 的采样撞上。换成 `Clear`
后从 10/243 降到 3~6/250,**没有消除**。机制不成立,或不是全部。

这条否定结果同时否掉了**双缓冲**:它同样只缩小撞车窗口而不针对机制,而 `Clear` 已经演示了
"缩小窗口"能降到三分之一却降不到零。

### 剩下的方向

唯一测到 0 的配置是"玻璃 pass 不存在"。它与正常路径的差别是:多一张 wgpu 追踪器之外被
skia 采样的纹理、多一次独立 `queue.submit`、每帧重建 view 与 bind group。共同的根是这张
纹理由 bevy 之外的通道写、又被 Slint 用 wgpu 看不见的命令读。

有证据支持的解只有一个:**把玻璃 pass 并进 bevy 的渲染图**,让链上只剩一张纹理,由 bevy
以清屏语义写、Slint 采样,即实测 0 异常的那个配置。工程量是一个 render graph node 加
shader 移植,`GlassPass` 整个删掉。
