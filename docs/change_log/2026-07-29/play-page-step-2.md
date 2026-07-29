# 播放页第二步:音频响应粒子场、封面深度卡与真聚焦门

## 1. Change Purpose

第一步(见 `docs/change_log/2026-07-28/play-page-step-1.md`)交付了 warp 视觉与沉浸
覆层,但本项目相对 Mineradio 的结构优势 —— UI 像素与 3D 像素同一合成体系、可互相
遮挡 —— 还停留在设施层面没有兑现;省电门的「聚焦」条件也因上游 API 缺口用可见近似
顶着。本次把两件事都落地:封面卡成为第一张深度卡片,粒子从它前后掠过;slint fork
透出窗口激活状态,门收紧为真聚焦。歌词链按 2026-07-29 的范围决定拆到下一轮。

## 2. Change Scope

- `yebei199/slint` fork(11642f2):`internal/core/api.rs` 加 `Window::is_active()`
  只读 getter,8 行。
- 本仓 `crates/ui/src/lib.rs`:门第三条 `is_visible()` → `is_active()`;Cargo.lock
  重锁全部 patch 包。
- `crates/render3d`:新 `src/particles.rs`(纯计算 + 六条测试);`src/lib.rs` 加
  `Content` 枚举、`render_viz_frame`、`rebuild_viz_content`、`set_camera_clear`,
  并把 `render_frame` 的后半段抽成公共的 `drive_and_finish`;`Cargo.toml` 补
  similar-asserts dev 依赖;`README.md` 加节。
- `crates/ui`:`viz.rs` 加 `VizImages` 三图结构;`lib.rs` viz 闭包扩签名并推三个
  image 属性;`app.slint` 覆层从两层扩成五层合成,封面卡改显式定位(`viz-card-*`
  为唯一真相);README 同步。
- `apps/desktop`、`apps/android`:Scene 改 `Rc<RefCell>` 由演示页闭包与 viz 闭包
  共享,viz 闭包驱动 `render_viz_frame` + `WarpPass` 返回三图。

## 3. Implementation Process

三个子单元(issue #12/#13/#14,父 #11),四个提交外加一笔 lock 补录:

1. **聚焦门**(fork 11642f2 + 本仓 f3981a0):`WindowActiveChanged` 上游本就
   分发进 `WindowInner::set_active`,只缺公开 getter;加在 `is_visible` 旁边。
   本仓重锁时按根 Cargo.toml 的注释点名每个 patch 包,避免静默 patch.unused。
2. **粒子场**(d0ccea5):纯计算与 ECS 解耦 —— `particle_pose` 只回答「第 i 个
   粒子此刻在哪多大」,金角序列铺三层轨道壳,低频撑轨道呼吸、各壳绑各自频段撑
   缩放脉动、时间只推方位角;每帧直写 Transform,不走 bevy system。与演示页共用
   同一 App/相机/双目标,`Content` 枚举管切换重建,viz 模式主相机清屏透明。
   本单元按完整 TDD 走:骨架 + todo! 先跑出 6/6 RED,再实现到 12/12 GREEN。
3. **五层合成**(e06b1a1):warp → 粒子场景(透明)→ 封面卡 → 遮挡层(裁到
   卡片矩形、负偏移对齐,抄演示页「转盘中心」卡的机关)→ 控制簇。封面卡从布局
   居中改成显式 `viz-card-*` 定位,遮挡裁剪才能逐像素对齐;无封面时留玻璃底,
   深度效果不依赖 CDN。

## 4. Key Diff / Core Functions

- `particles::band_levels`:512 字节频谱行 → 低/中/高三段均值;短载荷给静音,
  不 panic(跨 crate 外部输入)。
- `particles::particle_pose(i, time, levels)`:输入电平先消毒(NaN→0、clamp),
  输出位置与缩放有硬上限 —— 坏一帧数据不许把场景炸飞。
- `Scene::render_viz_frame`:resize → 内容切换重建 → 逐粒子写位姿 → 摘玻璃组件 →
  设遮挡深度门槛 → `drive_and_finish`。与 `render_frame` 互斥由 ui 的门保证
  (覆层开着时 `render-active` 恒假)。
- `ui::VizImages`:viz 闭包的出参从单图变三图;无 bevy 端场景与遮挡为空图,
  覆层自动退回第一步形态,.slint 零平台判断。
- apps 的 `Rc<RefCell<Scene>>`:同一事件循环内两个闭包顺序调用,借用不重叠。

## 5. Verification

- 单元测试:particles 六条先 RED(todo! 上 6/6 失败)后 GREEN,render3d 全量
  12/12;ui + app-desktop 30 条全绿;seam 全链 `cargo check`(bevy-3d)通过。
- 真机验收(桌面 linux,niri 真实像素):粒子铺满三层轨道壳、颜色与 warp 背景
  同调;**封面卡上压着粒子** —— 按 .slint 层序,场景图在卡片之下,能画到卡上的
  只可能是遮挡层,即近侧粒子盖卡、远侧被卡挡住,深度闭环成立;歌名/封面正常。
- 聚焦门:切走焦点(播放中、覆层开),相隔 3 秒两张截图 md5 一致 —— 失焦零重绘;
  切回焦点两张不同 —— 动画恢复。
- 已知未覆盖:android 端代码与桌面逐字相同但未做 APK 构建与真机验证(发热读数
  仍欠着);粒子在演示页↔播放页反复切换的重建路径只靠 Content 枚举的对称性保证,
  未做交互回归;同播听众端未实测。

## 6. Difficulties

- 第一次跑粒子测试撞了两个编译错:render3d 此前没有 dev-dependencies(补
  similar-asserts),以及 `Vec3::xz()` 需要显式引入 `Vec3Swizzles` trait。
- slint 重锁触发全依赖树重编,两次测试跑在了旧 manifest 上,靠后台任务日志
  区分「编译错」与「真 RED」。
- `cargo add --dev` 与 workspace 依赖的 registry 覆写冲突,改手写 manifest。

## 7. Final Result

播放页现在是完整的第二步形态:warp 背景之上悬着一片跟着频段呼吸脉动的粒子场,
封面卡是一张真正的深度卡片 —— 近侧粒子从它脸上掠过,远侧从它身后绕行,这是
参照物(Mineradio/projectM 的「UI 后面一块矩形」)在结构上做不到的画面。省电门
三条全部到位且第三条是真聚焦。fork 的 is_active 是 8 行的上游可提案改动。

## 8. Risks And Follow-ups

- android 构建与真机发热读数欠账,下次装机一并做(粒子数 219 是按真机可承受
  预估的,读数出来再调)。
- 粒子材质是 unlit + Blend,数百个透明体的排序开销在桌面无感,web(第三步)
  的 wasm 上要重新量。
- 歌词链(bang-dream 接口 → contract → api → core → 时间轴 → 深度卡片)是
  下一轮,「深度卡片首个真载荷」地位不变。
- fork 的 is_active 可考虑向上游提 PR(通用 API 缺口,非本项目特化)。
