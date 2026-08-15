# 安卓的帧泵:请求重绘为什么叫不醒事件循环

小米13 上,播放页与 3D 页都只有 1fps,而 GPU 其实闲着。这份记录定位到上游 slint
安卓后端的一处欠实现,并说明我们选了哪种修法、为什么。

诊断过程与读数见 issue #20;本文记的是**结论与做法**。

## 一、帧泵是什么

Slint 惰性渲染:没有东西变脏、也没人要求重绘,它一帧都不画。而 3D 场景与播放页
可视化需要连续出帧 —— 没人改属性,画面也得动。

于是 `crates/ui/src/lib.rs` 把帧驱动挂在**渲染通知**上:每次重绘前 Slint 回调我们,
我们驱动 bevy 前进一帧、把纹理塞给属性,然后调 `window().request_redraw()` 为下一帧
买票。下一次重绘到来时又回调,首尾相接 —— 每帧为下一帧买票,循环靠自己转。

不用定时器是刻意的,理由在 web:浏览器主线程唯一,固定间隔的定时器要么间隔太小把
主线程占死(1ms 实测 rAF 掉到 2~8 次/秒),要么间隔太大硬性设了帧率上限(16ms 实测
只有 40fps)。挂在重绘周期上,web 端天然是 requestAnimationFrame、原生端是 vsync。

## 二、安卓后端断在哪

上游 `internal/backends/android-activity/androidwindowadapter.rs`:

```rust
fn request_redraw(&self) {
    self.pending_redraw.set(true);
}
```

只置了一个布尔标记,不唤醒事件循环、不向系统排任何东西。而消费它的地方在
`lib.rs` 的事件循环:

```rust
loop {
    let mut timeout = duration_until_next_timer_update();
    if self.window.window.has_active_animations() {
        timeout = Some(...min(Duration::from_millis(10)));   // 只有 Slint 动画能缩短超时
    }
    self.app.poll_events(timeout, |e| { ... });               // 阻塞在这里
    if self.window.pending_redraw.take() {
        self.window.do_render()?;                             // 渲染在这之后
    }
}
```

**`pending_redraw` 不参与超时计算。** 我们的帧泵在渲染回调里请求重绘,那一刻
`poll_events` 已经返回,标记置上后循环回到顶部重算超时 —— 它看不见这个标记,于是
接着睡,直到某个定时器到期或有输入进来。

播放页没有 Slint 属性动画(warp 与粒子的运动来自我们送进去的纹理),`has_active_animations()`
为假。剩下能叫醒它的只有我们自己的定时器:帧率采样(500ms)与自动续播轮询(1s)。
醒一次渲染一帧,再睡半秒到一秒 —— 实测整帧 960ms、空等 930ms、平均 1fps,而
`app.update()` 只有 29ms、Slint 渲染 22~32ms。数字对得严丝合缝。

摸屏幕时输入事件涌入,poll 频繁返回,于是成串爆发,FPS 角标读到 120 —— 这也是为什么
「看起来有时是流畅的」。

## 三、这是 bug,不是省电设计

三条证据:

1. `WindowAdapter::request_redraw` 的文档(`internal/core/window.rs`)写着 "An
   implementation should repaint the window in a subsequent iteration of the event
   loop, throttled to the screen refresh rate if possible"。安卓后端并不**产生**那次
   迭代,只是等别人产生。
2. 同一个 trait 的 winit 实现是置标记**加** `frame_throttle.request_throttled_redraw(window)`
   —— 真的向窗口系统要一次重绘。两个后端对同一契约的落实程度不同。
3. 安卓后端那句 `// FIXME: we should not hardcode a value here` 就贴在动画分支上,
   说明这块本来就没做完。若是刻意的省电策略,那条 10ms 分支不会存在。

它平时不暴露:常规应用的 `request_redraw` 由「属性变脏」触发,而变脏来自某个事件或
定时器 —— 那次唤醒已经发生,循环回到底部就渲染了。只有**自维持的帧泵**会踩到。

## 四、我们的修法:把 pending_redraw 纳入超时,且归零

改一行:计算超时时,若 `pending_redraw` 已置,超时取 0 —— poll 立刻返回,循环立刻渲染。

这条**已经在上游**(slint#12688,上游提交 `c51982e7c`,代码在
`internal/backends/android-activity/lib.rs`),不再是 fork 的本地补丁,别去补丁栈里找。

备选与为什么不选:

- **钳到 10ms**(与动画分支同路):每帧白睡至多 10ms。我们的帧成本约 55ms,于是
  18fps 掉到 15fps,而且那 10ms 是后端替应用做的决定。
- **用 waker 唤醒**(`app.create_waker()`):效果与归零相同,却要从渲染回调里唤醒
  自己所在的循环,多一次描述符写入。waker 真正的用处是跨线程请求重绘,而
  `request_redraw` 本就要求在 UI 线程调。
- **接 Choreographer**(winit 那种做法):才是文档里 "throttled to the screen refresh
  rate" 的正解,120Hz 屏上 8.3ms 一帧。但那是功能实现,不是几行 —— 留给上游。

归零不给帧率设人为上限,**省电交给应用自己的门**,而不是后端背着应用偷偷节流。这正
符合本项目的取向:播放页展开且在播且聚焦时就该一直刷,离开或进后台就完全停
(`crates/ui/src/lib.rs` 的三条门)。进后台那条已经接好 —— `MainEvent::LostFocus`
派发 `WindowActiveChanged(false)`,`is_active()` 随之变假,门自动关闭。

安全性核对过一处:`PollEvent` 有 `Timeout` 变体,后端的 poll 回调对**每个**事件(含
`Timeout`)都会调 `update_timers_and_animations()`,所以把超时改零不会饿死 Slint 的
定时器与动画。

## 五、实测

改动前后的读数(小米13,APK 带 bevy-3d,播放页):

| | 改动前 | 改动后 |
|---|---|---|
| 整帧 | 959ms(1fps) | **12.92ms(77fps)** |
| 空等 | 928ms | **0.79ms** |
| `app.update()` | 29.2ms | **9.3ms** |
| Slint 渲染 | 32.0ms | **12.1ms** |

空等归零是这一行改动的直接效果。`app.update()` 与 Slint 渲染各降到三分之一是**意外**
收获,原因大概是之前每帧间隔近一秒:缓存全冷、GPU 时钟也降下来了,每帧都要从头热起来;
连续出帧之后单帧反而更便宜。这条提醒了一件事 —— **在帧泵坏着的时候测出来的单帧开销
是虚高的**,当时据此得出的「粒子太贵」结论并不成立。

温度:约 25 分钟连续满帧后电池 35.0°C、热区 35.6°C(基线 33.0/33.9)。持续 77fps
渲染 1300 颗粒子 + 全屏 warp,升温约 2°C,可以接受。

改动前一并测过 3D 演示页(968ms/1fps,空等 936ms),与播放页读数一致,这正是判断
「问题在后端而非播放页」的依据。

## 六、上游

已提 issue [slint#12687](https://github.com/slint-ui/slint/issues/12687) 与 PR
[slint#12688](https://github.com/slint-ui/slint/pull/12688)。

issue 带这组读数(同一份代码桌面 11ms 整帧、安卓 960ms,而 GPU 只占 55ms,两个页面
读数一致因而排除了应用侧),并把三种修法并列摆出来让维护者挑;PR 只带归零那一条,
正文写明愿意按他们的选择改成 Choreographer。

PR 的分支从上游 master 新拉,只 cherry-pick `2347f16` 一条。我们 fork 的 `dev` 落后
上游 194 个 commit(上游已走到 femtovg 0.26 + wgpu-30,本项目仍钉 wgpu-29),不参与
这次提交。核对过:上游 master 的那段事件循环与我们诊断时逐字相同,结论仍成立。

PR 正文当时只写了「节奏交给应用」半句,没交代为什么不选 10ms 钳位。已补一条
[评论](https://github.com/slint-ui/slint/pull/12688#issuecomment-5115126656)把上面
第四节那两条理由摆给维护者:钳位每帧白睡至多 10ms,按当时约 55ms 的帧成本是 18fps
掉到 15fps,而那个上限是后端替一个没提此要求的应用定的;归零让渲染成本自己决定帧率,
也把省电的决定留在应用能做的地方。

提交后的状态:CLA 通过,13 项 CI 全绿,`mergeable_state` 为 blocked,缺的只是一个
required review。标签打不了,我们在 slint-ui/slint 上只有 `pull` 权限,加标签需要
triage 以上,试过一次返回 403。分类只能靠标题的 `Android:` 前缀。

## 更新记录

- 2026-07-29 首版:定位到 `request_redraw` 只置标记而不参与超时计算;选定归零方案。
- 2026-07-29 补实测:fork 侧 `2347f16` 落地,1fps → 77fps、空等 928ms → 0.79ms;
  顺带发现帧泵坏着时单帧开销虚高三倍。
- 2026-07-29 提上游:issue #12687、PR #12688。
- 2026-07-29 补 PR 评论,把不选 10ms 钳位的两条理由写给维护者;记下 CLA/CI 状态与
  外部贡献者打不了标签这件事。
