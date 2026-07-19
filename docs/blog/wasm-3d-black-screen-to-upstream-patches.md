---
title: "从一块黑屏到两个上游仓库:排查 Slint + Bevy 在 wasm 上的 3D 渲染"
published: 2026-07-19
description: "web 端 3D 面板全黑,查下来是三个互相独立的缺陷叠在一起,最后向 slint 和 femtovg 提了六条 issue 与 PR。真正的教训在验证方法上。"
tags:
  - "wasm"
  - "WebGPU"
  - "Slint"
  - "调试"
category: 技术实践
draft: false
---

桌面端已经跑通的 3D 面板(Bevy 离屏渲染,把 wgpu 纹理交给 Slint 合成),编到 wasm 上打开浏览器,面板是黑的。

最后查出来是三个彼此独立的缺陷叠在一起,分别属于应用代码、Slint、以及 Slint 依赖的 femtovg。过程里我用错的验证方法比缺陷本身更值得写。

## 缺陷一:纹理被静默丢弃

`internal/renderers/femtovg/images.rs` 里,把 wgpu 纹理交给 femtovg 的那个 match 分支挂着 `#[cfg(all(not(target_arch = "wasm32"), feature = "unstable-wgpu-29"))]`。wasm 上这个分支被编译掉,`ImageInner::WGPUTexture` 落进兜底的 `_ =>` 走 `render_to_buffer()`,而该函数对这个 variant 返回 `None`,整张图就此消失。

难查的地方在于全链路零报错:adapter 拿到了,Slint 打印 `Using FemtoVG WGPU renderer`,Bevy 报告 8 个可渲染实体,`Image::try_from` 成功返回,应用日志照常打出「纹理就绪,已导入 Slint」。丢弃发生在这些之后的绘制期,没有任何一环喊疼。

定位靠的是两刀二分。先把玻璃后处理关掉,画面依旧黑,排除后处理;再往 Bevy 的目标纹理里清一个品红色,画面仍旧黑,说明 Bevy 侧确实在画、问题在 Slint 采样这一端。这才把范围压到了上游。

修复是放宽那个 cfg,九个字符。

## 缺陷二:1ms 定时器饿死浏览器主线程

驱动 3D 的 Slint `Timer` 取 1ms,代码注释写明这是有意为之的「有多快跑多快」,前提是「Slint 的 Timer 与事件循环单线程,回调超时也不会叠加」。这个前提在桌面成立,在浏览器不成立。

浏览器主线程是唯一的线程,合成画面、派发输入、跑 wasm 全挤在上面。实测这个定时器每秒触发 350 到 470 次,每次跑一整轮 Bevy `app.update()` 加玻璃 pass,把主线程占死,`requestAnimationFrame` 掉到每秒 2 到 8 次。渲染全程正常,画出来的东西送不到屏幕上。

关键判据是切回其他页面时帧率读数同样是 0。卡住的是整个界面,连 3D 页之外的页面也停止重绘,这一条就把 3D 链路本身排除了。改成 wasm 上 16ms(原生保持 1ms)之后,rAF 回到每秒 52 到 58,帧计数增速从每秒约 400 降到 60,正好是 16ms 的上限。

## 缺陷三:高分屏上的 SVG 图标炸掉整个应用

前两个修完,在真实 1.25 倍缩放的屏幕上仍然是「闪一帧就全黑」。控制台里终于有了确凿报错:

```
panicked at wgpu-29.0.4/src/backend/webgpu.rs:2697:14:
called `Result::unwrap()` on an `Err` value: JsValue(OperationError:
Failed to execute 'copyExternalImageToTexture' on 'GPUQueue':
Copy rect is out of bounds of external image
```

Slint 为高分屏放大 SVG 的做法是给 `<img>` 元素设置 width/height 属性,femtovg 按这个属性尺寸算 copy extent。WebGL 的 `texImage2D` 会按属性尺寸重新栅格化 SVG,所以这段 2023 年写下的代码对它唯一的消费者一直是正确的。WebGPU 按图片的自然尺寸读取并据此做越界校验,属性尺寸就成了谎报,`unwrap()` 把 JS 侧的 `OperationError` 变成致命 panic,整个 wasm 实例死掉,后续的 `RefCell already borrowed` 只是 unwind 过程中的二次 panic。

触发源是 std-widgets 的 `LineEdit` 清除图标,全应用唯一的图片,恰好只在 3D 页出现,所以症状精准表现为「进 3D 页就炸」。

这里我做过一个错误判断,写下来提醒自己:我一度把修复描述成「用清晰度换稳定」。后来写了个最小探针页实测,结论相反。

```js
const img = new Image();
img.src = URL.createObjectURL(new Blob([SVG_16x16], { type: "image/svg+xml" }));
await img.decode();
img.width = 20; img.height = 20;   // 复刻 Slint 的做法
console.log(img.naturalWidth);      // 仍然是 16

device.queue.copyExternalImageToTexture({ source: img }, { texture: tex(20) }, [20, 20]);
// OperationError: Copy rect is out of bounds of external image
device.queue.copyExternalImageToTexture({ source: img }, { texture: tex(16) }, [16, 16]);
// 成功
```

16x16 的拷贝成功是决定性的一条:WebGPU 拿到的源图像就是自然尺寸,属性放大在这条路上根本没生效。所以跳过放大并没有牺牲任何清晰度,它删掉的是一个在该后端无效、且会致命的调用。对照组不存在「清晰」这个状态,只有「崩溃」。

## 真正的教训在验证方法上

三个缺陷里有两个被我错误地宣布过「已修复」,原因都出在验证手段上。分析本身没错,错在用来确认的工具。

**CDP 截图看不见合成层的问题。** 我用 Playwright 的截图反复确认「画面正常」,而用户看到的是全黑。CDP 截图取自渲染进程,窗口在 Wayland 合成器上显示不出来时,截图照样完美。后来改用 `niri msg action screenshot-window` 抓真实窗口像素,才看到用户看到的东西。任何跨越了渲染进程与合成器边界的问题,渲染进程内部的观测手段都是瞎的。

**视口模拟会把 devicePixelRatio 钉死在 1.0。** 缺陷三只在缩放大于 1 时出现,而 Playwright 的 `setViewportSize` 让 dpr 恒为 1.0,属性尺寸与自然尺寸恰好相等,永远不炸。我按用户的窗口尺寸复现了很多次都是绿的,直到用 `connectOverCDP` 连上一个真实窗口。要验高分屏相关的问题,必须连真实窗口,尺寸对上不等于环境对上。

**帧率读数比截图更早暴露问题。** 缺陷二发生时我看到帧率读数是 2,但没深究,因为截图看起来是对的。事后看,那个数字就是证据本身。后来改用 `requestAnimationFrame` 实测频率加上日志里的帧计数增速做判据,才是能证伪的检查。

## 两个环境陷阱

**改 `~/.cargo/git/checkouts/` 里的源码不会生效。** cargo 按 rev 认缓存,不查文件内容,改了不触发重编。我据此白测了一轮,还差点得出「补丁无效」的错误结论。判断补丁有没有真的进产物,看重编的包数:该依赖及其同仓库兄弟包应该被整片重编。要验证依赖补丁,把源码 clone 出来,用 `[patch.crates-io]` 指过去。

**dev 服务器不发 `Cache-Control` 会让人查错方向。** `python -m http.server` 只发 `Last-Modified`,浏览器按启发式新鲜度自行缓存,重新构建后页面还在跑旧产物。而旧产物的症状和真实缺陷长得一模一样。更麻烦的是 Slint 的 canvas 会吞掉键盘事件,`Ctrl+Shift+R` 到不了浏览器。最后写了个十几行的 `dev-server.py`,在 `SimpleHTTPRequestHandler` 上加一句 `Cache-Control: no-store`。dev 服务器让浏览器缓存一个几十 MB 的 wasm,本身就是个设计错误。

## 向上游提交:为什么拆成六条

两个上游仓库,三对 issue 加 PR:

- slint#12538 / #12539:wasm 上 wgpu 纹理被静默丢弃
- slint#12540 / #12541:高分屏 SVG 图标 panic
- femtovg#299 / #300:为 `ImageSource` 增加 `HtmlCanvasElement` 变体

拆开的理由有三点,都与改动大小无关。**独立性**:一个是 cfg 记账遗漏,一个是 hidpi 图片上传路径,不同子系统、不同成因。**风险不对等**:前者九个字符、行为显然正确;后者是有讨论空间的方案选择。捆在一起,后者的方案讨论会把前者一起卡住,而前者是「web 上 3D 能不能用」的总开关。**可回滚性**:分开提,维护者可以只要其中一个。

femtovg 那条走的路子稍有不同。Slint 侧的修复只是跳过无效的放大,图标因此在高分屏上略软。要真正恢复清晰度,需要把 SVG 按目标尺寸画进 canvas 再上传,而 femtovg 的 `ImageSource` 在 wasm 上只有 `HtmlImageElement` 一个变体。所以 femtovg#300 加的是新能力,那个 panic 由 Slint 侧的修复挡住。同一个探针页证明 canvas 那条路可行:20x20 的 canvas 上传成功,读回的红通道只有 3 个灰阶,边缘是硬的,与按 20x20 重新栅格化一致。我没有单独测量位图拉伸的对照组,这一条是推断。

提交前查过重复:femtovg PR #277 改的正是同一个函数,修了缺 early return 和 origin 写死为零两个问题,合并于 2026 年 5 月,唯独没碰 extent。所以我们踩的这个坑从维护者手底下过了一遍没被发现,也不存在撞车。

## 适用边界

这套排查里可复用的是方法。具体的 cfg 门和 SVG 放大逻辑一旦上游修好就失效了,但下面几条大概会长期成立:

跨进程、跨库、跨语言运行时的渲染链路上,「每一层都报告成功」是常态,因为丢弃往往发生在所有返回值检查之后。遇到这类问题,优先找能把链路切成两半的注入式检查,比如往目标纹理清一个刺眼的颜色。逐层读代码通常更慢。

浏览器里的性能问题要先问「给主线程留了多少时间」,再谈渲染管线。任何在 wasm 上跑的周期性重活都得先过这一关。

验证工具本身有观测边界。选工具之前先问它能看到哪一层,以及要验的现象在不在那一层里。这次两次误报,都是因为工具的观测层比问题所在的层更靠内。
