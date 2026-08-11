# 待报上游:Skia 的绘制与 wgpu 的 present 之间缺少顺序约束

状态:**已在 fork 修复并真机验证(2026-08-11),上游 draft PR 已提交:
<https://github.com/slint-ui/slint/pull/12861>**(分支 `fix/wgpu29-present-order`,
基于 upstream/master 的最小 diff,不带 fork dev 里的日志标记)。
fork issue 与验证数据:<https://github.com/yebei199/slint/issues/1>。
修复提交:fork dev `be095f1`,PR 分支 `5efe0d9`。复测第二轮 4376 帧 0 半截帧,
两轮修复版合计 13575 帧零事件。

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

半截帧(录屏 120 秒,两区亮度比判据,抽帧目检):

| 条件 | 帧数 | 半截帧 | 比率 |
| --- | --- | --- | --- |
| 基线(未修复) | 10019 | 391 | 3.90% |
| 修复(be095f1,两轮) | 9199 + 4376 | 0 | 0% |

帧率成本(SurfaceFlinger present 时间戳,不开录屏,ABAB 交错四轮重装、
同一首歌同一段内容各 60 秒):114.4 → 96.0 → 86.7 → 85.6。单调下滑是
手机热节流,交错正是为把它与被测变更分开;热稳态下相邻的未修复/修复
差 1.3%,在轮间噪声内 —— **修复的帧率成本测不出来**。录屏产帧数当帧率
用会大幅低估且随机漂移(曾据此误报 7% 成本),别再用。
CPU 同步探针(肉眼可见的吞吐腰斩)仍是对照:顺序可以近乎免费地补上。

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
