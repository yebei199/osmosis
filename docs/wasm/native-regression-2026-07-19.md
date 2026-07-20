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

## 安卓 3D 页闪黑(未修完)

现象:3D 页上,一片黑从某一行往下盖到内容区底部,逐帧移动,看着像黑条扫下去。桌面没有。

2026-07-20 加深了采样量,推翻了本节 07-19 的三条结论。旧结论建立在每组 238 到 259 帧的
样本上,而真实发生率是 0.4% 到 0.7%,那个量级的样本里"测到 0"是常态。

### 形态:应用递交了半截画面

抓到出问题的那一帧看清楚了:上半部分完全正常,工具条、玻璃面板、透出来的模型都在;
从某条水平线往下整片空白,**连 Slint 自己画的底部标签栏都没了**,而 Android 系统导航栏
正常显示。

底部标签栏是普通 Slint 控件,不碰那张共享纹理,它跟着一起消失。所以这是一张只画了一部分
的画面。此前一直按"采样到了半写状态的纹理"排查,那个方向解释不了标签栏为什么会没。

### 测量方法

`adb shell screenrecord` 录 20 秒,ffprobe 一次过算逐帧内容区平均亮度:

```
ffprobe -f lavfi -i "movie=f.mp4,crop=1080:1100:0:800,signalstats" \
  -show_entries frame_tags=lavfi.signalstats.YAVG -of csv=p=0
```

低于中位亮度 80% 的帧记为半截帧。正常帧亮度约 68,半截帧稳定落在 37.2 到 37.3,两档之间
没有中间值,判据不敏感。脚本见 `$CLAUDE_JOB_DIR/tmp/flick2.sh` 的做法:逐帧起 `magick`
进程要跑十几分钟,上面这条几秒。

### 实测(2026-07-20)

| 条件 | 帧数 | 半截帧 | 比率 |
| --- | --- | --- | --- |
| Home 页,拖动 | 2147 | 0 | 0% |
| 3D 页,玻璃在 bevy 管线内 | 1534 | 6 | 0.39% |
| 3D 页,**玻璃关掉** | 1256 | 9 | 0.72% |
| 3D 页,自转、全程不触屏 | 90 | 2 | 见下 |

**玻璃不是原因。**把玻璃整个关掉(令 `GlassRect` 为空,render3d 会把 `GlassMaterial` 从
相机上摘掉),半截帧照旧出现,比率还略高一点。

**触摸不是触发条件。**自转、全程不碰屏幕的那组同样出现。这组只有 90 帧,因为 MIUI 把显示
刷新压到了 4Hz(应用日志:整帧 250.80ms = 回调 18.73ms + Slint 渲染 1.54ms + 空等
230.53ms),样本不足以定率,但足以否掉"必须有触摸"。

Home 页那组是 0,但它的亮度从头到尾几乎不变(最暗 70.0,中位 71.7),说明拖动在 Home 页
不产生重绘。这组只能说明测量方法不会凭空造出半截帧。

### 已否掉的假设

**玻璃 pass 的写与 skia 的读撞车。**玻璃关掉的对照直接否掉。

**读-改-写。**玻璃 pass 原本用 `LoadOp::Load`,换成 `Clear` 后从 10/243 降到 3~6/250,
没有消除。这条否定结果同时否掉了**双缓冲**:它同样只缩小撞车窗口。

**把驱动挪到 `AfterRendering`。**让 bevy 的写与 Slint 的读隔开一整帧,测得 6/1534,与不挪
(5/1230)一样。已回退,它只剩一帧延迟这个代价。

**"关掉玻璃就是 0"。**那次只跑了 259 帧。同一张旧表里 238 到 259 帧的每一行都有这个问题,
不要再拿它们做判据。

### 剩下的方向

嫌疑落在 3D 页每帧都做、而其他页不做的那件事:把一张由 wgpu 纹理支撑的 `slint::Image`
塞进属性,并请求重绘。半截帧的形态(脏区之外是空的)指向局部重绘与缓冲区轮换:本帧只重绘
了脏区,而轮到的那个缓冲区里没有上一帧的内容。

下一步要读的是 Slint 在安卓 skia 路径上怎么算脏区、以及换到的缓冲区是否被当作还留着旧内容
在用。这在本仓库里改不动。

顺带说明 `refactor(render3d): run the glass effect inside bevy's pipeline` 那个提交:它的
提交信息把闪黑降低算作了自己的功劳,按上面的数据那部分不成立。这次重构本身仍然站得住,
理由是净删 239 行、去掉一次独立 `queue.submit` 和每帧重建的 view 与 bind group,用上了
bevy 现成的 `FullscreenMaterial`。
