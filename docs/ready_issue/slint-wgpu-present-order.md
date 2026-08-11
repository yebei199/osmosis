# 待报上游:Skia 的绘制与 wgpu 的 present 之间缺少顺序约束

状态:**已在 fork 修复并真机验证(2026-08-11),上游上报待定。**
fork issue 与验证数据:<https://github.com/yebei199/slint/issues/1>。
修复提交:fork dev `be095f1`。要报上游时把本文与该 issue 翻成英文即可,
证据链已经完整;上游是否愿意收 `transition_resources` 这个修法,由他们定。

## 环境

| | |
| --- | --- |
| slint | 1.18,fork 在 `yebei199/slint`,分支 `dev` |
| 特性 | `unstable-wgpu-29`,`backend-android-activity-06` |
| 渲染器 | skia,走 `wgpu_29_surface` |
| 设备 | 小米 13(`fuxi`),MIUI,1080x2400,120Hz 屏,Vulkan |
| 应用 | `yebei199/osmosis`,共享 wgpu 纹理交给 Slint 合成 |

## 现象

部分帧从某条水平线往下整片空白,切口每帧位置不同,连不碰共享纹理的普通
Slint 控件都跟着消失。这是一张只画了一部分的画面被扫描输出拿走了。

## 机制(已定论)

`internal/renderers/skia/wgpu_29_surface.rs` 的 `render()`:Skia 经 `as_hal`
从 wgpu 掏出裸 `VkQueue`,直接往 `get_current_texture()` 给的交换链图像上画;
`flush_and_submit()` 只做 `gr_context.submit(None)`;紧接着 `frame.present()`。
wgpu 看不见 Skia 的提交,没有任何 wgpu 提交「用过」这张 surface texture,
present 等的信号量与绘制毫无关系,绘制与扫描输出之间没有顺序约束。
Skia 还把图像留在 `COLOR_ATTACHMENT_OPTIMAL`,没有人转到 `PRESENT_SRC`。

## 修复

Skia 提交之后、present 之前,提交一个**引用了这张交换链纹理**的近空命令缓冲:

```rust
let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
    label: Some("Skia present ordering encoder"),
});
encoder.transition_resources(
    std::iter::empty(),
    std::iter::once(wgpu::TextureTransition {
        texture: &frame.texture,
        selector: None,
        state: wgpu::TextureUses::PRESENT,
    }),
);
self.queue.submit(Some(encoder.finish()));
```

道理:vkQueueSubmit 的 semaphore signal 的第一同步范围涵盖同队列上**提交顺序
更早的全部命令**(Vulkan 规范 7.4.1),所以这个提交 signal 的 present 信号量
天然排在 Skia 的绘制之后;`transition_resources` 是 wgpu 文档钦点的原生互操作
屏障入口,`PRESENT` 态还顺带把布局交接做对。修复带一行一次性启动日志
`present ordered after Skia submission`,防止拿没修的包做测量。

## 验证(2026-08-11)

两轮同负载对照,播放页全屏点云,各 120 秒:

| 条件 | 帧数 | 半截帧 | 比率 | 帧率 |
| --- | --- | --- | --- | --- |
| 基线(未修复) | 10019 | 391 | 3.90% | 83fps |
| 修复(be095f1) | 9199 | 0 | 0% | 77fps |

按基线比率,修复版样本里期望约 359 次,实测 0。帧率约降 7%,与 2026-07 的
CPU 同步探针(92→50fps,腰斩)不同量级 —— 当年分不开的「补上顺序」与
「机会变少」由此分开:半截帧消失是顺序补上了,不是窗口变小了。
基线的可疑帧抽样目检过,确为「上半有内容、切口以下全黑」的真切口。

## 复现(2026-08 起的现役路径)

**旧文档里的 3D demo 页与 `just android-flicker` 配方已在 2026-07-29 删除**
(`9054338`),别再找它们。现役复现:

1. `ABIS="arm64-v8a" just android-build`,装机,登录,列表里点一首歌,
   打开播放页(全屏点云,自己每帧都动,不需要触摸)。
2. `adb shell screenrecord --time-limit 120 --bit-rate 16000000 /sdcard/flick.mp4`,
   拉回来。
3. 判半截帧用**上下两区亮度比**,不要用单区中位数阈值:播放页点云随音乐
   整帧明暗大幅波动,单区判据会把暗脉冲全误报(实测误报 32.69%)。
   两区判据:上区 `crop=1080:600:0:150`、下区 `crop=1080:600:0:1700` 各取
   ffprobe signalstats 的 YAVG,`下/上 < 0.35 且上 > 40` 记半截帧。
   整帧一起暗比值不动,只有下半没画才触发。
4. 样本量要到万帧;录制前后校验 `pidof` 一致,应用中途被杀这轮作废。

卡墙页复现不了(13047 帧 0 张):负载太轻,Skia 每次都赢了那场竞速。
要复现必须用播放页这种全屏重负载。

## 过程文档

- [`../wasm/native-regression-2026-07-19.md`](../wasm/native-regression-2026-07-19.md)
- [`../sys/2026-07-19-web-frame-rate-and-android-flicker.md`](../sys/2026-07-19-web-frame-rate-and-android-flicker.md)
- 2026-07 的 CPU 同步探针数据(0.105% 基线,3D 页)在本文件的 git 历史里。
