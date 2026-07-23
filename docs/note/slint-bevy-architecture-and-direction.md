# Slint + Bevy 的架构定位与未来方向

整理日期:2026-07-23。来源是一轮围绕"这套方案能做什么、边界在哪、往哪走"的讨论,落成文档供后续决策复用。

## 一句话定位

这套的分工是:Slint 负责 UI 与主循环,Bevy 在共享 wgpu device 上离屏渲染产出 3D 与 shader 效果,两者按深度逐像素合成。它是"带真 3D 与自定义 shader 的原生应用框架",不适合直接套用为重度游戏引擎。为什么是这个定位,下面逐条展开。

## 一、三层分工

| 层 | 职责 | 类比 |
|---|---|---|
| Slint | 布局、文字、输入、交互、常规视觉、事件循环 | UI 框架 |
| Bevy | ECS、几何、光照、离屏 3D 渲染 | 3D 渲染引擎 |
| wgpu | 全端统一 device,纹理共享的底座 | 见 ADR-0005 |
| render3d | 把 Bevy 的纹理交给 Slint 合成,支持逐像素深度遮挡 | 合成桥 |

全平台一致的底座是 wgpu,不是 Bevy。`crates/render3d` 的职责在其 Cargo.toml 写明:bevy 在共享 wgpu-29 device 上离屏渲染,产出 `slint::Image` 交给 UI 合成。Bevy 在这个架构里被去窗口化,只当渲染器,不拥有窗口、事件循环与输入。

窗口、事件循环、各平台输入、Android activity 生命周期、iOS UIView、Web canvas 绑定与 rAF 帧驱动,全部由 Slint 承担。这是 Slint 的看家能力,也是它替 Bevy 扛下的那部分全平台适配。

## 二、效果与 shader 的分工边界

这是全篇最容易混淆、也最需要说清的一节。

### Slint 自己能出的视觉

不写一行 shader,纯 `.slint` 就能出:渐变(`@linear-gradient` / `@radial-gradient` / `@conic-gradient`)、圆角、边框、`drop-shadow-*`、`opacity`、裁剪、矢量 `Path`、`animate` 与 `states` 动画。用 animate 驱动一个高光 Rectangle 或渐变的位置,就能做出流光扫过、边缘呼吸、press 涟漪这类动效。这些全后端一致,先榨干它们,不要用 shader 重造。

### Slint 的硬天花板

Slint 拿不到自己刚渲染的那一帧。`set_rendering_notifier` 回调里,`GraphicsAPI::WGPU29` 只暴露 `instance / device / queue`,没有 surface texture,没有 command encoder。所以任何"读回 Slint 画出的像素再处理"的效果都做不了:backdrop blur、液体折射扭曲背景、对 Slint 控件本身做后处理。声明式 shader(把 shader 挂在元素上)上游两次尝试均已关闭(PR #10874、#11191)。详见 `docs/slint/visual-effects-and-shaders.md`。

### "Slint 能不能接 shader"要拆成两个问题

| 说法 | 能否 |
|---|---|
| 在 `.slint` 里给元素挂 shader(类 CSS filter) | 不能。无此 API |
| 在 Slint 外用 wgpu 写 shader,渲染成纹理交给 Slint 当 `Image` 显示 | 能,shader 无限制。即 `docs/slint/visual-effects-and-shaders.md` 的路径 A |

结论表述:Slint 内部写不了 shader,Slint 外部能写任意 shader 出图再合成。`crates/render3d/src/glass.wgsl` 就是这条路径的实例。

### shader 的采样规矩

shader 能处理的像素,只有它自己采样得到的那些:它自己画的背景(3D 场景、壁纸、渐变、纹理)可以随意折射、模糊、流动。Slint 画的东西(其他控件、文字)采样不到,扭曲不了。所以"折射背景"这类效果只在背景由 GPU 自己画的页面成立(如 3D 页),普通 Slint 页面做不到。

### 分工落点

常规视觉(渐变、阴影、动画、圆角、流光)归 Slint。后处理视觉(模糊、折射、流动扭曲背景)归外置 shader。结构与交互(布局、文字、输入、hover、press)归 Slint。三者合成为最终画面。

## 三、跨端一致性:后端差异与降级

Slint 一份 `.slint` 由多个后端各自绘制,某后端未实现的绘制属性会被静默忽略,不报错、不塌布局。这带来跨端视觉差异,而且没有编译或运行时信号,只能靠人眼比对。

已知差异:

- `inner-shadow-*` 只在 Skia 后端实现(Slint 1.17 新增)。Desktop 与 Android 走 Skia 生效,Web 走 femtovg 静默失效。原因见 ADR-0006:wasm 编不了 Skia,`apps/web` 只能用 femtovg。
- 软件渲染器不支持 rotation、scale、drop-shadow。`mcp` 特性依赖软件渲染器时需注意。

决策规则:核心且要求跨端一致的视觉,不能依赖单后端特效。要么只用全后端都实现的属性,要么下沉到 wgpu shader(全端一致)。渐进增强性质的视觉(掉了不影响可用,如 Web 上少一道厚度光边)可以用单后端特效,当增强层。判断一个属性是否单后端:查 `docs/slint/visual-effects-and-shaders.md` 的表与 release notes,对 1.17+ 新增属性默认存疑,最终在 Web 上实测。

## 四、性能与效果的上限:原生和 Web 是两个答案

这套的性能优势来自绕开浏览器,所以优势只在原生端兑现。

原生端(desktop / android / ios):效果上限高于 Web(原生 wgpu 不吃 WebGL2 限制,例如 storage array 在 WebGL2 上限为 0,原生无此限,见对应 memory 记录),性能上限远高于 Web(直连 Vulkan/Metal/DX12,无浏览器沙箱、无 JS/GC,Bevy 多线程 ECS 直接可用)。

Web 端(wasm):效果上限约等于 Web 的上限(和传统 web 用同一块浏览器 GPU)。性能上限约等于传统 web,拿不出额外筹码,因为共用同一个浏览器、同一个 WebGPU/WebGL、同一个单线程模型。唯一例外是 CPU 计算密集型应用,wasm 对 JS 才有代差优势。

Web 端的痛苦(单线程、WebGL2 限制、包体大、只能靠 rAF)根源是 wasm 与浏览器,不是 Slint。相关排查见 `docs/wasm/frame-rate.md` 与 memory 中 141.8fps 一条。

## 五、应用还是游戏:主从关系决定

判断标准是谁拥有主循环。

Slint 当主(当前架构):适合应用、工具、可视化,哪怕内嵌 3D、哪怕 UI 被 3D 遮挡。当前项目已验证深度合成成立。轻到中等节奏的交互都在这个范围内。

引擎当主:重度游戏的硬需求。固定步长物理、一帧内 input → simulation → render 的顺序编排、最低输入延迟、帧调度自控,这些要求主循环归引擎。业界的 UE、Unity、各家自研引擎无一例外都是引擎当主、UI 当从属子系统(UE 的 UMG、Unity 的 uGUI / UI Toolkit)。没有重度游戏把 UI 框架当主程序。

当前"Slint 主 + Bevy 视图"用于重度游戏会主从颠倒:Bevy 当引擎最值钱的能力(独占事件循环、输入直达、全屏管线)被 Slint 接管,还要背上 device 共享与 fork Slint 的复杂度。重度游戏应把主从翻过来:Bevy 当主,Slint 降为从属 UI 层,或换 egui / bevy_ui。

关于主循环的一个具体陷阱:当前用 `set_rendering_notifier` 的 `BeforeRendering` 回调驱动 Bevy,并每帧强制 `request_redraw`(否则惰性渲染会冻住这块)。强制满帧解决的是画面连续,不解决固定步长物理、输入延迟、一帧内编排、帧预算。这四项的病根是主循环控制权在 Slint,与省不省电无关。轻负载下这些 workaround 成本接近零,负载(固定步长物理、大量实体、低延迟)上来后成本累积。

## 六、多线程

原生端 Bevy 默认多线程(task pool),ECS 调度自动并行。可自行 `std::thread::spawn` 或 rayon 开 worker。唯一约束:Slint 的事件循环与 UI 更新只能在主线程,worker 算完要用 `slint::invoke_from_event_loop` 回灌。

Web 端默认单线程。要多线程需 wasm threads(SharedArrayBuffer)+ `wasm-bindgen-rayon`,并且服务器发 COOP/COEP 跨源隔离头,页面内所有第三方资源要满足 CORP/CORS。工具链还需 nightly 与 `-Z build-std`。Bevy/Slint 在 wasm 多线程链路支持不完整。

收益判断:先分清瓶颈在 CPU 还是 GPU。渲染并行度在 GPU,不在 CPU 线程;上万实体只是画出来,瓶颈通常是 draw call,靠 instancing 合批解决。只有"上万实体且每帧跑重 CPU system(物理、AI、碰撞、寻路)"才是 CPU bound。即便如此,Web 端的顺序应是:先 profile 信任单线程 ECS,再 instancing,再考虑 WebGPU compute shader,最后才是 wasm threads。当前无此类负载,属于 YAGNI。

## 七、UI"简陋"的真相与抉择

Slint 内置组件(`std-widgets`)朴素、数量少,这个感受成立。但它是生态账,不是能力账:能达到的视觉上限是 GPU,不低;缺的是开箱的精致组件与现成资产。精致 UI 在 Slint 靠自己用基元搭(`glass.slint` 已证明可行),在 web 靠 npm 装现成组件库、动画库、设计系统。

抉择取决于核心诉求:

| 核心诉求 | 建议 |
|---|---|
| 视觉上限、原生性能、全平台、真 3D,组件种类不多但要精致 | 用这套,少数核心组件自己搭 |
| 炫酷 UI 的丰富度与出活速度,需要海量现成组件 | 用 web 栈,Slint 在这一维不划算 |

一个现实校准:多数应用只需要少数打磨过的组件(卡片、按钮、输入框、列表),不需要成套组件库。这种情况自己搭可行。真正需要海量现成组件快速拼装的是企业后台与表单密集型应用,那类 web 更合适。

## 八、落地样例:流动的液态玻璃按钮

能做,上限可达 GPU 级别的流动折射与模糊。做法是 shader 画玻璃视觉,Slint 套交互外壳:

- 玻璃视觉(流动折射、模糊、高光)由 wgpu shader 画。在现有 `glass.wgsl` 的基础上,给 `Params` 加 `time` 字段,每帧更新,让折射位移叠一层时间驱动扰动。
- 结构与交互(文字、点击区、hover、press)由 Slint 提供。hover/press 状态传成 uniform 给 shader,驱动流动强度。
- 这就是 3D 页 `GlassCard(backdrop: true)` 已在跑的模式,改成按钮形状加 `time` 即可。

前提:玻璃背后要被折射的背景必须由 GPU 自己画(3D 场景、壁纸、渐变)。按钮场景容易满足。普通 Slint 页(背景由 Slint 画)只能做表面流光(animate),做不到流动扭曲背景。

流光扫过、边缘呼吸、press 涟漪这些其实 Slint animate 就能出,只有流动折射与真模糊那一档必须 shader。落地时先用 animate 出七八成,剩下折射/模糊再上 shader。

## 九、决策速查

| 问题 | 判断 |
|---|---|
| 用 Slint 还是全 Bevy 做 UI | UI 是正经产品界面(复杂布局、文本输入、无障碍、主题)用 Slint;游戏 HUD、调试面板用 egui / bevy_ui |
| 效果做在 Slint 还是 shader | 常规视觉(渐变/阴影/动画/流光)Slint;后处理(模糊/折射/扭曲背景)shader |
| 效果要不要跨端一致 | 要,则不用单后端特效(如 inner-shadow),改全后端特效或下沉 shader |
| 目标是应用还是游戏 | Slint 主循环够用则是应用取向;需引擎独占主循环(固定步长物理/低延迟/大量实体)则反转成 Bevy 主 |
| Web 端要不要多线程 | 默认不要。仅当 CPU 密集且已过 profile → instancing → compute 三关仍不够时再考虑 |
| Web 端性能能否超传统 web | 一般不能,除非 CPU 密集型 |

## 相关

- `docs/adr/0005-wgpu-device-as-shared-base.md` wgpu device 作全端统一基座
- `docs/adr/0006-desktop-renderer-skia.md` desktop 切 Skia 与 inner-shadow 的代价
- `docs/slint/visual-effects-and-shaders.md` Slint 视觉效果上限与 shader 路径的完整调研
- `crates/render3d/src/glass.wgsl`、`crates/ui/slint/glass.slint` 玻璃拟态的 shader 与 Slint 两侧
