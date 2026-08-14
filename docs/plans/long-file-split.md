# 超长文件拆分计划

日期:2026-08-14。跟踪 issue 见 `docs/TODO.md`。

全仓 23 个文件超过或逼近 `CLAUDE.md` 的 700 行硬线。本文是逐文件的拆分方案:
切在哪、切完各多少行、验证怎么跑、风险在哪。

## 约定

**模块布局**:同名 mod。`foo.rs` 是模块根,子模块落在 `foo/` 目录里,
`foo.rs` 用 `mod bar;` 声明。**不使用 `mod.rs`**,包括 `tests/` 目录下的集成测试。
理由与代价见 [ADR 0026](../adr/0026-modules-split-by-same-name-directory.md)。

**尺寸**:按内聚切,单文件软上限 400 行、硬上限 700 行。宁可让一个强连通的
`impl` 块留在 500 行,也不为凑行数把它拦腰斩断。

**测试**:留在单元测试里,搬进 `foo/tests.rs`。不外移到 `tests/` 集成目录——
外移会丢掉私有符号访问,要为此放宽可见性,是拿架构换行数。测试自身超过 400 行时,
`foo/tests.rs` 继续按主题声明 `mod`,落到 `foo/tests/` 下。

**公共面**:每个模块根负责把子模块的对外符号 `pub use` 回来。crate 外部的调用
路径一律不变,这是每一步的验收条件。

**Slint**:属性面按域拆成 `export global` 单例,页面拆成独立组件直接读 global。
Rust 侧 `bind_*` 改用 `ui.global::<X>()`。见
[ADR 0027](../adr/0027-slint-state-lives-in-globals.md)。

**不在范围内**:`server/proto/music/v1/music.proto`(513)、
`docs/wasm/frame-rate.md`(536)、`docs/design/aurora-button.js`(533)。
proto 拆分等于改 package 布局与线上契约,文档和参考实现不受行数约束。

## 验证

每拆完一个文件,跑对应 crate 的测试再提交:

```
cargo test -p <crate>
```

两条已知陷阱:

- `cargo check -p ui` 单独跑在干净树上也会报 5 个 `load_image` 错,验 `.slint`
  改动一律用 `cargo test -p ui`。
- `just ci` 本地过不去(iOS 那条要 Xcode),逐条跑并绕开它,不要把构建命令
  接管道再看退出码。

`crates/ui` 与 `crates/api` 带 `#[cfg(target_arch = "wasm32")]` 分支的模块,
拆完额外跑一次 wasm 构建确认两侧 `mod` 声明的 cfg 都对上。

---

# 阶段一:机械搬迁(低风险)

接缝清晰、调用点单一、不动任何可见性。

## 1. `server/src/bangdream.rs` 1198 → 根 ~90

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `bangdream.rs` | `pub mod proto`、`as_user`、`platform_name`、`non_empty`、子模块 `pub use` | 90 |
| `bangdream/tests.rs` | `as_user` 的 2 个测试 | 37 |
| `bangdream/lyric_split.rs` | `MAX_LINE_CHARS`、`BREAKS`、`split_long_lines` 及 5 个 helper | 194 |
| `bangdream/lyric_split/tests.rs` | 18 个长行切分测试 | 265 |
| `bangdream/track_refs.rs` | `refs_missing_from`、`keep_available` | 40 |
| `bangdream/track_refs/tests.rs` | 6 个集合运算测试 | 122 |
| `bangdream/dto.rs` | proto → contract 的 8 个映射函数 | 130 |
| `bangdream/dto/tests.rs` | track / play_source / lyric / artist / playlist 映射测试 | 310 |

`split_long_lines` 只被 `lyric_to_dto` 一处调用,输入输出都是 `contract::LyricLineDto`,
完全不碰 proto 类型——全仓最干净的一刀。

**风险**:`main.rs` 用 `use server::bangdream::{...}` 平铺导入一大列符号,
`bangdream.rs` 必须 `pub use` 全部子模块符号保住平铺路径。
`server/tests/live_bangdream.rs` 同样要核对。

## 2. `crates/api/src/lib.rs` 1681 → 根 ~120

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `lib.rs` | `mod` 声明、全量 `pub use`、`pub mod settings/session` | 120 |
| `error.rs` | `ApiError` + Display/Error、`server_error`、`base_url` | 68 |
| `catalog.rs` | `health`、`check_version`、搜索/播放/歌词/每日的 8 个请求 + URL 构造 | 145 |
| `auth.rs` | `register`、`login`、`logout` | 57 |
| `playlists.rs` | 歌单 CRUD 8 个 + 红心订阅 3 个 + `Named`/`TrackRefs`/`TrackRefDto` + URL 构造 | 178 |
| `history.rs` | `record_play`、`recent`、`stats` | 26 |
| `artwork.rs` | 封面门面 7 个符号 | 46 |
| `platform.rs` | cfg 开关根:按 target 声明 `native` 或 `web` 并 `pub(crate) use` | 20 |
| `platform/native.rs` | 原生 transport + 磁盘 IO | 422 |
| `platform/native/tests.rs` | `sweep_dir` 的 96 行测试 | 96 |
| `platform/web.rs` | localStorage + fetch 版同名实现 | 174 |
| 各模块 `tests.rs` | lib.rs 现有 318 行测试按被测模块重分配 | 318 |

两个 `platform` 是 cfg 互斥的同名平行实现,接口是对齐的 15 个符号——作者已经
把这条缝画出来了,只是没落成文件。`platform.rs` 做 cfg 开关可让上层
`platform::get_json(...)` 的调用路径一字不改。

**风险**:约 40 个请求函数是 crate 公共 API,`crates/ui` 直接调用。
拆完 `lib.rs` 必须 `pub use` 全量回来,**逐个核对**,漏一个 ui 就编译不过。
`session::TOKEN` 是全局 static,位置不能动。

## 3. `crates/app-core/src/queue.rs` 744 → 根 ~242

生产代码 229 行的 `Queue` 是不可分的游标状态机(`order`/`cursor`/`shuffled`/
`loop_mode` 被 15 个方法中的 12 个同时读写),**原地不动**。这个文件的拆分
就是把 502 行测试搬进子模块。

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `queue.rs` | `LoopMode`、`Queue`、`splitmix` | 242 |
| `queue/tests.rs` | 夹具 `track`/`batch`/`id_of` + 游标推进 7 个 + replace 1 个 + 子 `mod` 声明 | 122 |
| `queue/tests/shuffle.rs` | 洗牌 9 个测试 | 165 |
| `queue/tests/loop_mode.rs` | 循环模式 11 个测试 | 200 |

## 4. `crates/render3d/src/cloud.rs` 1142 → 根 ~233

文件内所有符号都是 `pub(crate)`,crate 外部面为零,拆分风险最低。

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `cloud.rs` | 几何常量、预设、`CloudParams`、`CloudMaterial`、`build_cloud_mesh`、子模块 `pub(crate) use` | 233 |
| `cloud/tests.rs` | 预设与缩放的 5 个测试 | 59 |
| `cloud/spin.rs` | `Spin` + `clamp_rate` + 5 个 SPIN_* 常量 | 86 |
| `cloud/spin/tests.rs` | 拖拽惯性 4 个测试 | 94 |
| `cloud/transition.rs` | `TrackTransition` + 2 个常量 | 55 |
| `cloud/transition/tests.rs` | 换歌过渡 4 个测试 | 71 |
| `cloud/mesh.rs` | `CloudVertices`、`CUBE_*` 表、`cloud_vertices`、`hash01`、`band_levels` | 196 |
| `cloud/mesh/tests.rs` | 顶点生成 7 个 + 分带 2 个测试 | 251 |

`CloudParams` 的字段顺序与 `.wgsl` 的 uniform 布局手工对齐,**必须与
`CloudMaterial` 同文件**,不拆。`ALPHA_CUTOFF` 有两个 cfg 互斥定义,别漏。
根用 `pub(crate) use` 回来后 `lib.rs` 里的 `cloud::Spin` 等路径不变。

## 5. `crates/render3d/src/lib.rs` 1362 → 根 ~40

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `lib.rs` | `mod` 声明 + 4 组 `pub use` | 40 |
| `seam.rs` | `SharedTexture`、`Pointer`、`CoverUpdate`、`VizFrame` | 58 |
| `camera.rs` | `spawn_camera`、`spawn_occluder_camera`、`anchor_ndc`、`anchor_viewport`、`occluder_depth` | 142 |
| `camera/tests.rs` | 相机投影数学的 248 行测试 | 248 |
| `scene.rs` | `struct Scene`(22 字段)、场景常量、`device`/`queue`、`Default` | 120 |
| `scene/setup.rs` | `Scene::new`、`Scene::new_async` | 157 |
| `scene/viz.rs` | `render_viz_frame`、`drive_and_finish`、`frame_images`、`extract_texture` | 213 |
| `scene/cover.rs` | `clear_cover`、`apply_cover`、`rebuild_viz_content` | 146 |
| `scene/target.rs` | `resize`、`make_target`、自由函数 `extract_texture` | 75 |
| `scene/input.rs` | `apply_pointer`、`render_wall_frame` | 45 |

同一 crate 内允许多个 inherent `impl Scene` 块,**字段可保持私有**——这个文件
拆分不需要放宽任何可见性。`render_viz_frame` 一个函数触碰 15 个字段,不可再分。

## 6. `crates/ui/src/music.rs` 2421 → 根 ~200

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `music.rs` | `Deck`、`Prefetched`、`bind`(native/wasm)、`WASM_NOTICE`、子模块声明 | 200 |
| `music/tests.rs` | Deck 层行为测试 | 337 |
| `music/rules.rs` | `join_artists`、`format_duration`、`describe_*`、`should_*`、`to_rows` 等纯函数 + 6 个常量 | 294 |
| `music/rules/tests.rs` | 纯函数测试(含 `playback_copy_only_uses_subset_glyphs` 与 `CJK_SUBSET`) | 330 |
| `music/feed.rs` | `CoverFeed`、`LyricFeed`(native + wasm stub) | 124 |
| `music/list.rs` | `Section`、`bind_search`、`bind_list`、`load_section`、`fetch_daily`、歌单开合 | 355 |
| `music/transport.rs` | `toggle_play`、`play_current`、`start_auto_advance`、`push_progress` | 320 |
| `music/advance.rs` | `take_prefetched`、`start_prefetch`、`advance`、`advance_auto`、`after_advance` | 80 |
| `music/controls.rs` | `bind_play`、`bind_controls`、`apply_loop`、`bind_volume`、`bind_seek`、`push_seek_state` | 242 |
| `music/notice.rs` | `report_stream_loss`、`startup_check` | 52 |
| `music/report.rs` | `play_to_report`、`prepare`、`emit` | 80 |

`Deck` 是 21 个 `Rc<RefCell<…>>` 字段的共享把手,跨全部子模块。留在 `music.rs`
根,字段转 `pub(crate)`——这是本文件唯一的可见性放宽,范围限于 `music` 模块树内。
`bind` 是唯一装配点,留在根做门面,各子模块导出自己的 `bind_*`。

**风险**:`#[cfg(not(target_arch = "wasm32"))]` 遍布全文件,每个新 `mod` 声明
都要带对应 cfg,否则 wasm 构建炸。参照 `lib.rs` 里 `mod playlist` 的写法。

---

# 阶段二:需要调整可见性或跨文件状态(中风险)

## 7. `crates/audio/src/stream_source.rs` 1035 → 根 ~250

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `stream_source.rs` | 常量、`SeekRequest` 别名、`buffered`、`buffered_with`、`ChannelSource` 及其 `Iterator`/`Source` impl | 250 |
| `stream_source/fixtures.rs` | `#[cfg(test)]` 的 `Marker`、`Tape` 两套假 Source 与 `pull_until` 等 helper | 206 |
| `stream_source/tests.rs` | channel 基本行为 4 个测试 | 102 |
| `stream_source/seek_state.rs` | `SeekState`、`Phase` + 6 个方法 | 58 |
| `stream_source/seek_state/tests.rs` | 跳转状态机 6 个测试 | 174 |
| `stream_source/retry.rs` | `apply_seek`、`seek_with_retry`、`discard` | 84 |
| `stream_source/retry/tests.rs` | 重试逻辑 4 个测试 | 104 |

`buffered_with` 与 `ChannelSource` 是一对不可分的生产/消费,同去同留。

**风险**:`SeekRequest` 是横跨三区的通道类型别名,必须留在根。
`SeekState` 是 `pub` 导出类型(`music.rs` 用 `audio::SeekState`),根要 `pub use` 回来。
`ChannelSource::try_seek` 的忙等窗口与 `SeekState::take_failure` 在时序上耦合,
拆开后两处都要留注释指向对方。

## 8. `crates/audio/src/lib.rs` 1354 → 根 ~60

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `lib.rs` | `mod` 声明 + `pub use` | 60 |
| `error.rs` | `AudioError` + Display/Error/From、`full_cause` | 72 |
| `error/tests.rs` | 错误链测试 | 46 |
| `runtime.rs` | `RUNTIME` static + `runtime()` | 12 |
| `loader.rs` | `Source` trait + blanket impl、`Loaded`、`load`、`load_with` | 110 |
| `loader/tuning.rs` | `Tuning`、`PRODUCTION`、`StreamHealth`、`PREFETCH_BYTES` | 45 |
| `loader/decode.rs` | `decode` | 50 |
| `loader/decode/tests.rs` | wav 夹具 + 编解码边界测试 | 145 |
| `loader/tests.rs` | 夹具 + 子 `mod` 声明 | 40 |
| `loader/tests/stall.rs` | `StallingSource` + 卡顿模拟 8 个测试 | 378 |
| `loader/tests/reconnect.rs` | 两个真 TCP 服务器夹具 + 重连 4 个测试 | 314 |
| `player.rs` | `Player` + 12 个方法 + `clamped_volume` | 123 |
| `player/tests.rs` | 音量钳制测试 | 24 |

**风险**:`RUNTIME: OnceLock<Runtime>` 是进程级单例,`load_with` 与 `Player`
双方使用,拆分后只能有一个定义点。`pub use stream_source::{...}` 五个符号
原样保留——`crates/ui/src/music.rs` 直接用 `audio::SeekState` 和 `audio::StreamHealth`。

## 9. `server/src/main.rs` 1414 → 根 ~205

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `main.rs` | 7 个常量、`Upstream`、`AppState`、`FromRef`、`fail`、`conn`、`main`(路由表) | 205 |
| `routes.rs` | 子模块声明 | 15 |
| `routes/auth.rs` | `health`、`register`、`login`、`logout` | 91 |
| `routes/search.rs` | `SearchQuery` + 搜索/发现 5 个 handler | 175 |
| `routes/catalog_cache.rs` | `cached_tracks`、`fill_details`、`netease_name`、`detail_tracks_of`、`track_refs_of` | 120 |
| `routes/likes.rs` | `PageQuery`、`liked_ids`、`liked`、`liked_playlist_id` + 红心订阅切换 6 个 | 240 |
| `routes/playlists.rs` | 歌单读 3 个 + 写 6 个 + `NameBody`/`TracksBody`/`TrackRefDto` | 320 |
| `routes/play.rs` | `play` | 26 |
| `routes/history.rs` | `record_play`、`recent`、`stats` | 112 |
| `routes/lyric.rs` | `lyric` | 22 |

`cached_tracks` / `fill_details` / `detail_tracks_of` / `track_refs_of` 在红心与
歌单两组之间有四条交叉边,单独提成 `catalog_cache.rs` 是比「红心歌单一起搬」
更好的一刀。

**风险**:`AppState`、`Upstream` 要转 `pub(crate)`(axum 的 `State<AppState>`
提取器要求各 handler 模块可见)。`fail` 与 `conn` 被 30+ 处调用,留在 `main.rs`
由子模块 `use crate::{fail, conn}`。注意 `main.rs` 与 `crates/api/src/lib.rs`
**各定义一个同名 `TrackRefDto`**,别搞混。main.rs 的 handler 不被任何测试直接
引用,拆分零测试成本。

## 10. `apps/desktop/src/mpris.rs` 833 → 根 ~290

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `mpris.rs` | 跨平台 `start` shim、D-Bus 常量、`Shared`、`Mpris` + `MediaControls` impl、`serve`、`Player` zbus 接口 | 290 |
| `mpris/tests.rs` | 夹具 `TempBus` + 真实 D-Bus 端到端 2 个测试 | 211 |
| `mpris/map.rs` | `status_name`、`loop_status_name`、`loop_mode_of`、`track_path`、`metadata_of`、`put` | 127 |
| `mpris/map/tests.rs` | 映射 5 个测试 | 82 |
| `mpris/root_iface.rs` | `Root` zbus 接口(10 个常量应答) | 56 |

`Mpris` + `Player` + `Shared` 是 250 行的强连通块,同去同留。

**风险**:整个 `mod linux` 包在 `#[cfg(target_os = "linux")]` 下,子模块声明
要带同样的 cfg。`Player` 的 27 个方法名由 `#[zbus(interface)]` 绑定到 D-Bus
属性名,**只搬不改**,改了等于改协议。D-Bus 端到端测试需要访问 `linux::`
私有符号,不能外移。

## 11. `crates/ui/src/media.rs` 854 → 根 ~180

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `media.rs` | `Bridge`、`bind`、`dispatch`、`push` + 子模块 `pub use` | 180 |
| `media/tests.rs` | 夹具 `Spy` + 快照/去重/dispatch 测试 | 303 |
| `media/seam.rs` | `MediaStatus`、`NowPlaying`、`MediaCommand`、`MediaHooks`、`MediaControls`、`NoControls` | 151 |
| `media/rules.rs` | `toggles`、`flips_shuffle`、`wants_loop`、`loop_index`、`loop_from_index`、`seek_target`、`seek_ratio` | 90 |
| `media/rules/tests.rs` | seek 换算 3 个测试 | 44 |

`media/seam.rs` 里那 6 个类型正是 `lib.rs` `pub use media::{...}` 导出的全部内容,
是 ui ↔ 平台层的契约。根 `pub use seam::*` 后 `lib.rs` 一字不改。

**风险**:`Bridge` 被 `music.rs` 的 `Deck.media` 直接持有,路径变化牵动 music.rs
(阶段一已拆过,注意顺序)。`NowPlaying::render` 是 `pub(crate)` 而 struct 是 `pub`,
可见性梯度要保持。`MediaControls` 的实现者在 `apps/desktop` 与 `apps/android`,
只搬定义不改签名。

## 12. `server/tests/cache.rs` 738 → 根 ~70

集成测试每个 `tests/*.rs` 是独立 crate,夹具跨文件要么重复要么走 `common/mod.rs`。
**两条都不走**:让 `cache.rs` 保持单一测试二进制,子模块落在 `tests/cache/` 下。
夹具留在根,子模块 `use super::*` 拿到,零重复、不增加数据库连接池。

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `tests/cache.rs` | 7 个共享夹具 + 子模块声明 | 70 |
| `tests/cache/membership.rs` | 曲目往返与成员关系 7 个 + 红心即普通歌单 1 个 | 270 |
| `tests/cache/details.rs` | 详情缺失/复用 5 个 | 153 |
| `tests/cache/added_at.rs` | `added_at` 排序语义 4 个 + 迁移兼容 1 个 | 252 |

**风险**:`make_account` 的 `INVITE` 常量与 `server/tests/accounts.rs` 大概率重复,
提夹具时一并核对。需要真 Postgres,`DEFAULT_DATABASE_URL` 不可用时整组跳过。

---

# 阶段三:结构性改动(高风险)

## 13. `crates/ui/src/lib.rs` 881 → 根 ~170

`run_with_renderers` 一个函数 550 行,是整个函数体作为 `set_rendering_notifier`
闭包、`move` 捕获 7 组跨帧 `let mut` 状态。拆它必须先把状态打包成 `FrameState`
struct 传进闭包——这是前置动作,不是可选项。`music.rs` 的 `Deck` 已是同一模式的先例。

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `lib.rs` | 20 个 `mod` 声明(注释密度极高,原样保留)、`pub use`、`build_ui`、`run`、`platform_name`、`fps_enabled`、`MAX_TAB` | 170 |
| `render_loop.rs` | `FrameState` + `run_with_renderers`(含 `GREENS` 常量) | 300 |
| `frame_stats.rs` | `FRAME_ACCT_WINDOW`、`FrameAccounting`、`mod fps` | 107 |
| `lyric_push.rs` | 歌词推送去重(原 `lyric_shown`/`lyric_window_shown`) | 60 |
| → 并入已有 `nav_glass.rs` | 导航选中器省电门(原 `nav_last_*` 四个变量) | 80 |
| → 并入已有 `aurora_btn.rs` | 光带按钮动画(原 `btn_*` 五个变量,该文件已有 `ButtonAnim`) | 100 |

播放页时钟与标注卡锚点(`viz_time`/`viz_last`/`viz_anchor`)与帧计时同处一个
渲染通知回调的时序里,留在 `render_loop.rs`。

**风险**:`GREENS` 常量埋在函数体中段,容易漏。20 个 `mod` 里 8 个带
`#[cfg(not(target_arch = "wasm32"))]`,新增模块要判断落在哪一侧。

## 14. `crates/ui/slint/app.slint` 2004 → ~250

分两步,中间提交一次。

**14a 属性面入 global**。当前 MainWindow 有 200+ 个属性/回调,是 Rust 侧 20 个
`bind_*` 的唯一 API 面。按域拆成 6 个 `export global`:

| 新文件 | global | 内容 | ≈行 |
|---|---|---|---|
| `globals/player.slint` | `Player` | 播放文案、进度、音量、缓冲、洗牌循环、传输 callback | 105 |
| `globals/viz.slint` | `Viz` | 场景纹理、播放页几何、预设、指针、封面/歌词 | 69 |
| `globals/library.slint` | `Library` | 歌单读写与改名删除确认态、搜索三页签、红心 | 72 |
| `globals/session.slint` | `Session` | 登录态、个人页 | 42 |
| `globals/shell.slint` | `Shell` | 调试开关、版式与 tab、同播、二级导航、卡墙、封面取色、GPU 纹理槽 | 96 |
| `nav.slint` | `Nav` | 现有 `global Nav` 原样移出 | 24 |

同一次改动里 Rust 侧全部 `ui.set_xxx()` / `ui.on_xxx()` 改成
`ui.global::<X>().set_xxx()`。这一步不动视图树,改完 UI 行为应完全一致。

**14b 页面拆组件**。子组件直接读 global,不做属性转发。

| 新文件 | 内容 | ≈行 |
|---|---|---|
| `playpage.slint` | 播放页整块(场景/锚点卡/歌词块/进度/控制条) | 417 |
| `musicpage.slint` | 音乐页壳 + 搜索框 + 各列表 + 空状态 | 240 |
| `playlistdetail.slint` | 歌单详情头 + 批量添加行 + 新建行 | 138 |
| `wallview.slint` | 卡墙开关与 `wall-area` | 130 |
| `home.slint` | Home 页 | 148 |
| `navshell.slint` | 导航选中器几何(15 个派生 length + 3 个 animate)+ 宽版侧栏 + 紧凑底栏 | 173 |
| → 并入已有 `controls.slint` | 宽版与紧凑两套控制条 | 196 |
| `app.slint` | import、`MainWindow` 壳、登录页与横幅装配 | 250 |

**风险**:

- 纯视觉的派生几何**不进 global**,跟着它的组件走;global 只放 Rust ↔ UI 的
  状态契约。导航选中器那 15 个属性是唯一例外——其中 `nav-bg` 由 Rust 侧渲染
  读取,那几个留 `Shell`,其余跟 `navshell.slint`。
- `compact` 由 `changed` 回调命令式赋值以剪断布局求值环。移进子组件时若改成
  `in property` 再回传,环会重新出现,必须保持命令式赋值这一侧。
- `renaming` / `confirming-delete` 存的是歌单 id 而非布尔,为的是让状态随歌单
  切换自动作废。搬进 `playlistdetail.slint` 时不能简化成布尔。
- `wall-visible` 是 `wall-supported` + `view-wall` + `current-tab` +
  `open-playlist-name` 的复合守卫,留在外层。
- `controls.slint`(530)与 `widgets.slint`(429)吸收内容后会涨,在同一步里
  按组件切成子文件。
- **验证只能靠真机**:桌面窗口比手机扁、safe-area inset 恒 0,贴边几何的问题
  桌面看不见。这一步收尾必须在 Android 真机上复核播放页与音乐页。

---

# 阶段四:边缘文件(400–700 行)

这 9 个尚未违反硬线,拆分方案在动手前用与阶段一相同的方法现场确定接缝,
不预先纸上谈兵。按现有认识,预期形态:

| 文件 | 行 | 预期 |
|---|---|---|
| `crates/ui/src/playlist.rs` | 650 | 读写分离 + 测试外移到 `playlist/tests.rs` |
| `crates/ui/src/wall.rs` | 558 | 布局计算与绑定分离 |
| `crates/ui/slint/controls.slint` | 530 | 阶段三已吸收内容,同步按组件切 |
| `crates/ui/src/syncplay.rs` | 453 | 信令与 UI 绑定分离 |
| `crates/audio/src/codec.rs` | 436 | 测试外移 |
| `xtask/src/android.rs` | 429 | 构建步骤按阶段切 |
| `crates/ui/slint/widgets.slint` | 429 | 按组件切 |
| `crates/render3d/src/warp.rs` | 428 | 管线与参数分离 |
| `crates/contract/src/lib.rs` | 402 | 按 DTO 域切 |

`crates/contract` 是跨端线格式契约,拆分只动文件布局不动任何字段,
`pub use` 保住全部路径。

---

# 提交与追踪

一个总跟踪 issue 带 23 项 checklist,每拆完一个文件验证通过即提一个
`refactor(<scope>): ...` commit 并引用 issue,全部完成后关闭。
在 `dev` 分支上做,提交后即推。
