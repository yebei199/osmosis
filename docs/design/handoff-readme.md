# Handoff: Osmosis 播放器改版（绿色主调 · 卡墙主线 · 11 个页面）

## Overview

Osmosis 现有 UI 的一次整体改版。范围：**壳的首页（应用启动器）+ 音乐应用的十个页面 + 移动端紧凑版式**，配套八个 shader / Bevy 效果方案。

主要变化：

1. **色板从琥珀改为绿**（保留 `theme.slint` 的语义 token 结构，只换值）。
2. **Home 不再是"音乐首页"**，而是 Osmosis 这个壳的应用启动器 —— 瓦片浮在同一个 3D 场景里，底部常驻迷你播放条，所以在别的应用（单词/视频/新闻）里也能控播放。
3. **音乐页主视图改为 3D 卡墙**，列表是同一批卡塌回 z=0，不是两个页面。
4. **播放条重做**：悬浮胶囊 + 环形进度播放键（原通栏灰带作废）。
5. **播放页保留体素封面**，功能层收成一枚胶囊，守 ADR-0010（装饰入 3D、功能留 2D）。
6. 补齐原本没有的**设置页**与**个人主页**。

## About the Design Files

本包里的 `Osmosis 播放器改版.dc.html` 是**设计参考，不是可以直接拿去用的产品代码**。它是用 HTML/CSS 画的高保真稿，用来表达最终的排布、尺寸、颜色、层级与动效意图。

**目标环境是这个仓库自己的技术栈**：`crates/ui/slint/*.slint`（界面声明）+ `crates/ui/src/*.rs`（绑定）+ `crates/render3d`（wgpu / WGSL）+ Bevy 场景。请把设计稿**在 Slint 里重建**，沿用仓库既有的模式（`Theme` 全局取色、`ListView` 虚拟化、`GlassCard` / `AuroraBackground` 基元、省电门），不要把 HTML 结构或 CSS 直译过去。

设计稿里的 3D 部分（体素封面、卡墙、Home 瓦片场）是**用 CSS 3D 做的近似**，只表达构图与观感，真实实现走 Bevy / WGSL —— 见 `shaders.md`。

## Fidelity

- **2D 部分：高保真（hifi）。** 颜色、字号、圆角、间距、层级都是最终值，按 README 的数值实现。
- **3D 与 shader 部分：中保真。** 构图、层级关系、时长是设计决定；粒子密度、噪声参数、景深半径需要在真机上调，设计稿给的是方向和分层拆解。

## ⚠️ 开工前必须先解决的一件事：中文显示字体

设计稿的大标题用了 **Caprasimo**（`_ds` 设计系统的 display 字体）。**Caprasimo 不含中文字形** —— 稿子里的「每日推荐」「我的歌单」在浏览器里是靠 fallback 字体渲染的，看起来正常，但那不是 Caprasimo。

仓库现状：`crates/ui/fonts/cjk-subset.ttf` 是中文子集，`cargo test -p ui` 有 glyph 测试守着"硬编码中文必须落在子集里"。

**三选一，先定：**

| 方案 | 做法 | 代价 |
| --- | --- | --- |
| A（推荐） | 中文标题用一款有份量的中文字体（如思源宋体 Heavy / 方正等），英文/数字仍走 Caprasimo | 要新增一个子集字体，glyph 测试要覆盖新字重 |
| B | 中文标题直接用 Figtree + 系统中文，靠字号和字重拉层级 | 最省事，但"大标题"的性格弱掉一档 |
| C | 大标题只用英文（DAILY / PLAYLISTS / SETTINGS），中文降为副标题 | 与"中英混排"的定位一致，但信息密度变了 |

所有标着 `Caprasimo` 的地方都受这条影响。**在没定之前不要开始做 Home 和各页标题。**

## Design Tokens

### 明暗两套（对应 `crates/ui/slint/theme.slint` 的 `Theme` 全局，token 名不变，只换值）

| token | dark | light | 说明 |
| --- | --- | --- | --- |
| `window` | `#0b100c` | `#e9ece6` | 窗口底，只露边角 |
| `base` | `#0e130f` | `#f2f5ee` | 音乐页的底 |
| `surface` | `#131a14` | `#ffffff` | 卡片、面板、控制条 |
| `raised` | `#1e2b1f` | `#dde7d6` | 选中项 |
| `hover` | `#18211a` | `#eaf0e5` | 悬停 |
| `text` | `#e6e8ef` | `#1c2419` | 正文最强一档 |
| `text-dim` | `#9aa0b4` | `#5a6356` | |
| `text-faint` | `#6f7688` | `#8d968a` | 时长、未点亮的红心 |
| `accent` | `#8fc46a` | `#4f7a3f` | **唯一的强调色** |
| `accent-text` | `#0f2410` | `#f2f7ec` | 压在强调色实底上的字 |
| `accent-ink` | `#8fc46a` | `#35521f` | 中性底上表示"选中"的字与图标 |
| `overlay-weak` | `#ffffff08` | `#00000008` | |
| `overlay` | `#ffffff14` | `#00000014` | |
| `divider` | `#ffffff10` | `#00000012` | |
| `liked` | `#d96c8a` | `#d96c8a` | 不跟主题翻，"喜欢"不是明暗 |

**⚠️ 与现状的一处语义变化：** 原来 `accent-text` 是近白 `#f0e6d8`（琥珀底上压浅字）。绿色的明度高得多，浅字在 `#8fc46a` 上只有 ~1.9:1，**必须改成深墨 `#0f2410`（≈ 9.4:1）**。`crates/ui/tests/theme.rs` 里如果有针对该 token 的对比度断言，需要同步更新期望值。

### 沉浸层（播放页/歌词页，两套主题取值相同 —— 这是定义，不是偷懒）

| token | 值 |
| --- | --- |
| `immersive` | `#0a1210` |
| `immersive-panel` | `#101a15ee` |
| `immersive-text` | `#eaf3e2` |
| `immersive-text-dim` | `#9aa0b4` |
| `immersive-accent` | `#8fc46a` |
| `immersive-line` | `#ffffff1f` |

### 极光与玻璃（`glass.slint`）

| token | dark | light |
| --- | --- | --- |
| `aurora-base` | `#131a14` | `#eef2e8` |
| `aurora-warm` | `#3f7a4a9c` | `#bcd6a4b0` |
| `aurora-deep` | `#2b5c3f95` | `#a3c9ada0` |
| `aurora-soft` | `#86b98c66` | `#d8ead0aa` |
| `glass-fill` | `#ffffff1f` | `#ffffff8c` |
| `glass-edge` | `#ffffff3d` | `#ffffffcc` |
| `glass-inner` | `#ffffff40` | `#ffffffd9` |
| `glass-sheen` | `#ffffff2b` | `#ffffff73` |
| `glass-shadow` | `#00000073` | `#0000001f` |

### 副色板（12 对，**不承担任何界面语义**）

只用于：封面占位、卡墙卡片、歌单封面、波形条、统计数字、口味画像的条。**绝不用于按钮、选中态、链接、状态色** —— 一屏里颜色可以很多，但"重点"永远只有绿。

```
0 #2f6b47 → #8fc46a    6 #6b5a2f → #d9c46a
1 #1f5c5a → #6fc2a8    7 #2a6b62 → #8fd9c4
2 #3d6b2f → #c3d96a    8 #4a3d6b → #8f9fd9
3 #5a4a86 → #a08fd9    9 #6b3d3d → #d9958f
4 #7a4a52 → #d98fa0   10 #3d6b5a → #8fd9b4
5 #2f5c86 → #6fa8d9   11 #5c6b2f → #bdd98f
```

取用规则：`hues[index % 12]`，渐变一律 `linear-gradient(150deg, 亮色, 深色)`（列表小图用 `140deg`）。**真实封面到位后，这些只作为封面加载前的占位与失败兜底。**

### 尺寸 / 圆角 / 间距

| 项 | 值 |
| --- | --- |
| 左侧图标栏宽 | `76px`；图标槽 `52×52`，圆角 `16px`，间距 `6px` |
| 二级导航栏宽 | `208px`（仅 1a 沉浸底片方向用到，卡墙主线不用） |
| 容器圆角 | 卡片/面板 `18px`，大卡 `22px`，列表行 `14px`，列表小图 `11px`，中图 `12–16px` |
| 胶囊 | `border-radius: 999px`（按钮、输入框、控制条、标签一律） |
| 控制条高 | 桌面 `62px`（音乐页）/ `72px`（播放页）；移动 `44–54px` |
| 环形播放键 | 桌面 外 `48px` / 内 `40px`；播放页 外 `60px` / 内 `50px`；移动 外 `30–40px` |
| 列表行 | padding `9px 14px`，封面 `42px`，标题 `14px/600`，副标题 `12px`，行间靠 gap `2px` |
| 页面内边距 | 内容区 `26px`（左侧留 `100px` = 76 栏 + 24 空） |
| 底部导航（移动） | 高 `48px`，图标 `17px`，四项等分 |
| 触摸目标 | 移动端不低于 `44px` |

### 阴影

| 用途 | 值 |
| --- | --- |
| 悬浮玻璃条 | `0 24px 46px -20px #000`，加 `inset 0 1px 0 #ffffff26` |
| 卡片 | `0 18px 34px -20px #000` |
| 大卡/瓦片 | `0 26px 44px -22px #000` |
| 强调键 | `0 12px 26px -10px <accent>` |

### 字号

| 角色 | 字体 | 大小 / 字重 |
| --- | --- | --- |
| 页面大标题 | Caprasimo（见上面的字体问题） | `30–52px` |
| 播放页歌名 | Caprasimo | `38–44px` |
| 区块标题 | Figtree | `17px / 700` |
| 列表标题 | Figtree | `13.5–14px / 600` |
| 正文 | Figtree | `12.5–13.5px / 400` |
| 副文本 | Figtree | `11–12px / 400` |
| 数字与全大写标签 | DM Mono | `9.5–12px`，`letter-spacing .10–.24em` |
| 统计大数字 | Figtree | `30px / 700`，取副色板 |

**时长、进度、版本号、设备名等一切等宽场景都用 DM Mono**（原设计里全局 mono 的观感由此保留，但不再让整个界面都是 mono）。

## Screens / Views

设计稿里的编号即锚点，`Osmosis 播放器改版.dc.html` 里 `id="3a"` 等可直接定位。

### 3a — Home（应用启动器）

- **用途**：Osmosis 的常驻导航。音乐只是第一个应用，后面还有单词 / 视频 / 新闻 / 阅读。
- **布局**：左侧 76px 图标栏（Home 选中 / Music / 个人 / 设置，个人与设置沉在底部）。左上 118px 处标题区：`OSMOSIS`（DM Mono 11px，letter-spacing .24em，accent 色）→ 大标题「今天做点什么」→ 一行副文案。标题区下方（top 190px）是 3D 瓦片场，`perspective: 1100px`，`perspective-origin: 50% 40%`，整体 `rotateX(8deg)`。
- **瓦片**：6 张，圆角 22px，尺寸随深度递减（210 / 180 / 168 / 150 / 132 / 126），`translateZ` 从 +40 到 -330，越远越暗（opacity `0.72 + z/1400`）。每张上面压一层 `linear-gradient(180deg,#ffffff1a,#00000059)` 让文字可读；左下角是应用名（16px/700）+ 状态标签（DM Mono 11px，如 `MUSIC · 在放`、`VOCAB · 今日 40`）。最后一张是空槽「＋ 装一个」。
- **底部**：居中悬浮迷你播放条（60px 高），封面 40px + 曲名/歌手 + 环形播放键 + 下一首。**这条在所有应用里都在**，这是"壳"这个概念的具体体现。
- **真实实现**：瓦片是 Bevy 场景里的物体，慢速漂移（周期 ≥ 20s，振幅 ≤ 8px），鼠标位置带一点视差。省电门：窗口失焦即停。

### 3b — 音乐页 · 卡墙（主视图）

- **用途**：挑歌。默认视图，不是"3D 模式"这种额外档位。
- **布局**：76px 图标栏；顶部 24px 起标题行 —— 「每日推荐」(Caprasimo 30px) + 副行 `拖动翻找 · 双击播放 · 滚轮进深度`（DM Mono 10.5px）；右侧一排四个次级入口（每日推荐 / 我的歌单 / 最近播放 / 红心，胶囊，未选中态 `#ffffff0d`）；最右是「列表 / 卡墙」二选一开关（选中态 accent 实底 + `accent-text` 深墨）。
- **卡墙**：场区 `left:76 right:0 top:96 bottom:92`，`perspective: 1200px`，`perspective-origin: 50% 42%`，整体 `rotateX(6deg)`。卡片 150×150，圆角 14px，4 列 × 3 行，列距 204px，行距 136px，奇数行整体右移 26px；`translateZ` 在 -240…+80 之间散开，`rotateY` -16…+14°，`rotateX` -5…+7°；越远越暗。卡片左下角是曲名（11.5px/700，白字 + `0 1px 3px #000` 描边）。
- **⚠️ 坐标不要跨尺寸复用**：设计稿里两处卡墙（1b 是 1104×540 的场，3b 是 904×432）用了两套坐标。真实实现应按容器尺寸**算**列距行距，不要写死像素。
- **交互**：拖动 = 整场绕 Y 轴旋转 + 平移；滚轮 = 相机沿 z 推拉；单击 = 卡片浮到前面并高亮；双击 = 播放并进 3f。切到「列表」= 所有卡插值回 z=0 并排成行（**不是切页面**）。
- **底部**：悬浮控制条（见下方"播放条"）。

### 3c — 我的歌单

- 76px 栏 + 内容区。标题「我的歌单」+ 右上「新建歌单」实底键。副行 `4 本地 · 3 平台（只读）`。
- 4 列网格，gap 18px。封面 `aspect-ratio: 1`，圆角 18px，副色板渐变。**平台歌单右上角有只读徽标**：22px 高胶囊，`#00000073` + `backdrop-filter: blur(6px)`，锁图标 + 「只读」11px。本地歌单没有徽标。
- 判据走 `playlist::is_editable`（ADR-0016）。平台歌单的右键菜单里不出现改名/删除/移除。

### 3d — 搜索

- 顶部 56px 胶囊输入框（`surface` 底 + `#ffffff2b` 边），左侧 accent 色放大镜，右侧提示 `ENTER 播放第一条`（DM Mono 10.5px）。
- 下面三个页签胶囊，带计数：`单曲 · 24` / `歌手 · 3` / `歌单 · 7`。选中态 accent 实底。
- 结果列表：三列网格 `1fr 180px 70px` —— 封面+标题/歌手、专辑、时长。行 hover `hover` 色。
- 关键词记在 Rust 侧（输入框长在 `if` 里，Rust 引用不到 —— 沿用 `search.rs` 现有做法）。

### 3e — 最近播放（时间轴）

- 左侧一条 2px 竖线，`linear-gradient(180deg, accent, accent22)`；每行一个圆点，**最新那条是 12px accent 实心 + `0 0 0 4px accent2e` 光晕**，其余是 8px `#ffffff2b`。
- 行内容：38px 封面 + 标题/歌手 + 右侧相对时间（DM Mono 11.5px，`刚刚` / `12 分钟前` / `今天 14:20` / `周日 20:33`）。
- 顶部副行给周统计：`本周 62 首 · 4 小时 11 分`。数据来自 `server/migrations/0003_play_events.sql`。

### 3f — 播放页

- 底 `immersive`，`radial-gradient(55% 45% at 50% 55%, #16211a, #070b09 72%)`。
- **体素封面**：500×500 的平面 `rotateX(62deg) rotateZ(-3deg)`，顶部 52px 起。真实实现是 Bevy 点云（现有 `cloud.rs`），设计稿用 CSS 近似。
- 左上「收起」胶囊（36px 高），右上「视觉预设」面板（`immersive-panel` 底，圆角 18px，宽 150px，当前只有「封面点云」一项 —— 见 ADR-0014，入口留着）。
- 歌名 Caprasimo 38px 居中（top 214），下面 DM Mono 的 `M3MO · 2:13`。
- 歌词三行（top 312）：上下行 15px `#9aa0b4a8`，当前行 23px/700 `immersive-text`，全部带 `0 4px 22px #000` 阴影。**歌词卡 z 抬到位移峰值之上，粒子从字后走**（ADR-0010 补充条款）。
- 底部控制簇：72px 高胶囊，`红心 · 上一首 · 环形播放键(60/50) · 下一首 · 随机`，分隔线后是 `1:34 / 3:47`（DM Mono 11px）。**功能层是 2D，不参与遮挡。**

### 3g — 歌词页

- 从播放页横向滑出（不是新页面栈）。点云退成背景光，14 颗粒子从歌词后面横穿（5–11s 线性，随机 y 与尺寸，取副色板，opacity .55）。
- 左上角 64px 封面 + 歌名/歌手。
- 歌词列，gap 22px：当前行 30px/700 `immersive-text` 无模糊；其余按到当前行的距离给 `blur(距离 × 0.62px)` 与递减的 opacity —— **真实实现用 CoC 半径做真景深，不是贴一层模糊**。
- 右上两个 mono 胶囊：`逐字 · 开` / `翻译 · 关`。
- 底部 58px 迷你条：时间 + 260px 细进度 + 环形键。

### 3h — 空状态

- 点云以 50% opacity 待机（280×280 平面 `rotateX(60deg)`，双向 comb 纹理）。
- 大标题「点一首歌开始」(Caprasimo 40px) + 副行「点云在待机，选一首它就活过来」。
- 三个动作：`随便放一首`（accent 实底）/ `打开本地文件夹`（玻璃）/ `登录同步平台歌单`（最弱一档）。
- 底部一行 mono：`未登录 · 本地音乐照常可用`。**未登录不拦人。**

### 3i — 设置

- 左侧 186px 分组导航（外观 / 账号与授权 / 缓存与存储 / 快捷键 / 关于），选中态 `raised` 胶囊。
- 右侧卡片流，每张 `surface` 底 + `#ffffff14` 边 + 18px 圆角 + `18px 20px` 内边距，标题是 DM Mono 10px accent 色全大写。
  - **外观**：主题三选一分段控件（深色 / 浅色 / 跟随系统）；「动态极光按钮」开关（44×26 轨道，accent 实底，20px 深墨滑块），副文案「只在前台跑，退到后台自动停」。
  - **账号与授权**：每行 = 状态点 + 名称/状态 + 右侧动作键。三行：BanG Dream（已授权 · 3 小时前同步 / 重新授权）、本地文件夹（3 个目录 · 1284 首 / 管理）、Osmosis 账号（会话 12 天后过期 / 退出）。
  - **缓存与存储**：12px 高分段条，四段 46% / 27% / 15% / 12%，依次 `#8fc46a` `#6fc2a8` `#a08fd9` `#ffffff1f`；下面图例（曲目封面 840M / 歌单封面 490M / 音频缓存 270M / 其他）。数据源见 `artwork.rs`（按歌单 id 键）与 `thumbnail.rs`（按封面 URL 键 + blake3 磁盘层）。
  - **快捷键**：两列（动作 / 按键），按键用 DM Mono。
  - **关于**：右下角 `Osmosis 0.9.2 · Slint 1.17 + Bevy` + 「查看日志 · 检查更新」。

### 3j — 个人主页

- 顶部：82px 圆形头像（副色板渐变）+ 昵称 Caprasimo 32px + mono 副行（`加入 412 天 · 本机 linux · 3 台设备`）+ 右侧「2026 年中报告」实底键。
- 四张统计卡（等宽网格，gap 14px）：本月时长 41h / 听过 1284 / 红心 218 / 连续在听 23 天。**大数字 30px/700，每张取不同副色**，下面一行灰色对比文案。
- 下半区 `1.25fr 1fr`：
  - 左：**口味画像** —— 五条横向进度（独立 78% / 电子 61% / City Pop 44% / 后摇 31% / 民谣 18%），标签 74px 定宽，条高 9px，右侧百分比 mono。底部一行说明把画像和"每日推荐怎么来的"连起来。
  - 右上：**已连接平台**（状态点 + 名称 + 右侧 mono 状态）。
  - 右下：**同播设备名册**（本机 / 在线 / 离线，三种状态点颜色 `#8fc46a` `#6fc2a8` `#6f7688`）。

### 3k — 移动端紧凑版式

宽度 `< 600px` 时（ADR-0007，判据写在 `.slint` 里，Rust 不参与）：

- 左侧竖栏 → **底部 48px 导航**（Home / Music / 搜索 / 我的，图标 17px，选中态 accent 描边）。
- 迷你播放条浮在底部导航**之上** 8px，44px 高，圆角 999px，左右各留 10–12px。
- 卡墙列数降到 2，卡片 62–78px，深度范围收窄（竖视口相机要后撤，见 `render3d` 的 `pullback`）。
- 播放页：体素平面缩到 220px，歌词三行字号降到 10/13px，控制簇只留 上一首 / 环形键 / 下一首。
- 歌词页整屏，行距 13px，当前行 15px/700。
- 个人主页统计卡改 2×2；设置页分组导航折叠成整块卡片流。

### 播放条（全局，B 方案）

选定的是**环形进度播放键**：

- 外圈 `conic-gradient(accent 0 <进度>%, #ffffff1f <进度>% 100%)`，内圈实心 `surface` 圆 + accent 图标。
- 条身：`surface` + `e6` 透明度，`#ffffff2b` 1px 边，`0 24px 46px -20px #000`，圆角 999px，桌面居中悬浮、离底 22px。
- 内容顺序：封面(44) · 曲名/歌手+时间 · 分隔线 · 上一首 · 环形键 · 下一首 · 分隔线 · 随机/音量/展开。
- **进度不再需要单独的细线**（环本身就是进度）；需要精确拖动时，hover 条身才浮出一条 4px 细轨。
- 移动端同一枚环形键直接复用，不用另做一套。

## Interactions & Behavior

| 交互 | 行为 | 时长 / 缓动 |
| --- | --- | --- |
| 列表 ⇄ 卡墙 | 同一批卡在 z 上插值，不换页 | 420ms `ease-out` |
| 行/卡 → 播放页 | 相机 dolly + 卡片 `rotateX` 展平，Slint 层同时淡出；落位后才起点云 | 420ms |
| 换歌 | 体素塌落 → curl noise 流场 → 重聚（见 `shaders.md` §1） | 900ms |
| 拖动卡墙 | 惯性 + 阻尼，松手吸附到最近格点 | 阻尼 0.92 |
| 悬停列表行 | 底色 → `hover` | 120ms |
| 按下按钮 | accent 走一步深（暗底 `#7bb356`，亮底 `#3f6633`） | 80ms |
| 键盘焦点 | `outline: 2px solid accent; outline-offset: 2px` | — |
| 主题切换 | 只写 `Theme.dark` 一位布尔，值存 `api::settings`（跟设备不跟账号） | 200ms 交叉淡入 |
| 缓冲 | 点云加高频噪声 + 轻微失焦 | 循环 |
| 断流 | saturation uniform 3s 降到 0.06，画面冻结；横幅降级为一行小字 | 3s |

**省电门（沿用仓库现有三道门，不要新增每帧重绘）**：转场门（导航选中器）、warp 门（展开∧播放∧可见）、`render-active` 门（3D 场景）。动态极光按钮是第四类，判据是"前台 ∧ 窗口可见 ∧ 用户没关"。

## State Management

新增/变更的状态（其余沿用现有绑定）：

| 状态 | 住在哪 | 说明 |
| --- | --- | --- |
| `view-mode: list \| wall` | `.slint` property | 音乐页主视图，不持久化（每次开局回卡墙） |
| `wall-camera: {yaw, pitch, dolly}` | `.slint` property，由 Bevy 读 | 拖动/滚轮改它 |
| `aurora-colors: [color;3]` | Rust → uniform | 从封面提色，换歌时 400ms 插值 |
| `dynamic-aurora-buttons: bool` | `api::settings` | 跟设备走，与主题、音量同理由 |
| `theme-mode: dark \| light \| system` | `api::settings` | **注意**：现在 `Settings.dark` 是 bool，加"跟随系统"要改成三值枚举，`theme.rs` 的 `bind` 与 `crates/ui/tests/theme.rs` 都要跟着改 |
| `active-app: music \| vocab \| …` | 壳层（app-core 之上） | Home 的选中态；迷你播放条跨应用常驻 |

数据获取无新增接口：歌单/搜索/最近播放/红心/账号/设备名册都走现有 `api` 与 `server` 路由；个人主页的统计与口味画像需要在 `server` 侧新增聚合查询（基于 `play_events`）—— **这是唯一一处需要后端新工作的地方**。

## Assets

- **没有引入任何新图片资源。** 设计稿里所有封面、头像、瓦片都是副色板渐变占位，真实实现接现有封面链路（`cover.rs` / `artwork.rs` / `thumbnail.rs`）。
- **图标**：Lucide，`stroke-width: 2.75`（设计系统规定，比默认粗，配合圆角观感）。设计稿里用的是内联 SVG，实现时按仓库现有方式引入。
- **字体**：Figtree（UI）、DM Mono（数字/标签）、Caprasimo（display，**但有中文问题，见上文**）、`cjk-subset.ttf`（现有）。

## Files

| 文件 | 内容 |
| --- | --- |
| `Osmosis 播放器改版.dc.html` | 全部设计稿。四轮：`#brief` 设计说明与色板、`#t3` 十一个页面（3a–3k）、`#t2` 八个 shader 方案（2a–2i）、`#t1` 第一轮三个方向（1a–1e，历史，供对照） |
| `shaders.md` | 八个 shader / Bevy 效果的分层拆解与实现顺序 |
| `README.md` | 本文件 |

在浏览器里打开 `.dc.html`，按 `#3a` … `#3k` 锚点定位到具体页面。

## Screen map（设计 → 仓库文件）

| 页面 | 主要落点 |
| --- | --- |
| 3a Home | `crates/ui/slint/app.slint`（tab 结构）、壳层导航 |
| 3b 音乐页卡墙 | `app.slint` + `tracklist.slint`、`src/music.rs`、`crates/render3d`（卡墙场景） |
| 3c 我的歌单 | `playlists.slint`、`src/playlist.rs`（`is_editable`） |
| 3d 搜索 | `src/search.rs` + 搜索页 `.slint` |
| 3e 最近播放 | `server/src/history.rs`、`play_events` 表 |
| 3f 播放页 | `app.slint` 覆层、`crates/render3d/src/cloud.rs`、`src/viz.rs` |
| 3g 歌词页 | `src/music.rs` 的 `LyricFeed` |
| 3h 空状态 | `app.slint` |
| 3i 设置 | `src/theme.rs`、`api::settings`、`src/account.rs` |
| 3j 个人主页 | `src/account.rs`、`server` 新增聚合查询 |
| 3k 移动端 | `app.slint` 的 `compact: root.width < 600px` |
| 播放条 | `app.slint` 的 ControlCluster、`src/progress.rs` |
| 色板 | `crates/ui/slint/theme.slint`、`crates/ui/tests/theme.rs` |
| 玻璃/极光 | `crates/ui/slint/glass.slint`、`crates/render3d/src/glass.rs` |

## 建议的实现顺序

1. **定字体方案**（见上文），再动任何标题。
2. **换色板** —— 只改 `theme.slint` 的值 + `accent-text` 的语义修正 + 更新 `tests/theme.rs`。改完整个 app 立刻变绿，风险最低、反馈最大。
3. **播放条改成环形键胶囊** —— 纯 Slint，`app.slint` + `progress.rs`，不碰 3D。
4. **3i 设置 / 3j 个人主页 / 3c 歌单只读徽标 / 3e 时间轴** —— 纯 2D 页面，可并行，个人主页要等后端聚合查询。
5. **移动端断点** —— 桌面拖窄窗口就能验证（`just shot 420`）。
6. **shader 四件套**：grain+扫描线 → 动态极光按钮 → 封面取色喂极光 → 换歌过渡（`shaders.md` 有顺序理由）。
7. **卡墙与相机推近** —— 最重，动到页面切换逻辑与相机，放最后。
