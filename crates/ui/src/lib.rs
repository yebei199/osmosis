//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 本 crate 也是**组装点**:它把 [`api`] 的请求函数注入 [`app_core`],
//! 让二者互不相识。依赖方向单向,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

mod nav_glass;
pub use media::{
    MediaCommand, MediaControls, MediaHooks, MediaStatus,
    NoControls, NowPlaying,
};
// 循环三态过 seam 原样透传:平台层(MPRIS/安卓)拿它翻成各自的方言。
pub use app_core::LoopMode;
pub use nav_glass::NavGlassControls;

mod viz;
pub use viz::{
    CoverUpdate, VIZ_AUDIO_BYTES, VizControls, VizCover,
    VizImages, VizPointer,
};

// 封面解码用到 image,是原生 target 的依赖(web 的封面等播放链路通了一起做)。
#[cfg(not(target_arch = "wasm32"))]
mod cover;

// 登录页的绑定。所有端都要 —— 音乐相关的路由一律要登录态。
mod account;
// 歌单列表与详情。与 music 分开:那边管的是「一批歌」,这边管的是「哪一批」。
// 与 artwork 同一道门:歌单封面要它,而它是原生 target 的依赖。用它的地方
// (music 的 Deck、search)本来就都在门里。
#[cfg(not(target_arch = "wasm32"))]
mod playlist;
// 歌单封面:取一次、记住、下次直接给。
#[cfg(not(target_arch = "wasm32"))]
mod artwork;
// 曲目行的缩略图。与 artwork 分开是因为键不同(封面 URL vs 歌单 id),
// 因而缓存、去重与淘汰的规则全都不同。
#[cfg(not(target_arch = "wasm32"))]
mod thumbnail;
// 红心:哪些歌在红心里,以及点一下之后发生什么。
mod liked;
// 播放进度的格式化。与列表里的时长同一条规矩:算在 Rust 侧,`.slint` 里只摆。
mod progress;
// 搜索的三个页签。歌曲那一路借 music 的队列,歌手与歌单各自成列。
#[cfg(not(target_arch = "wasm32"))]
mod search;

// 一次性提示的唯一出口。所有端都要 —— 报错的路各端都有。
mod notice;

mod media;
mod music;
// 明暗主题。颜色在 slint/theme.slint,这里只管那一位布尔值住在哪。
mod aurora;
mod aurora_btn;
pub use aurora_btn::{
    AuroraBtnControls, AuroraBtnSlotControls,
};
// 卡墙的几何与交互动力学,纯数学、无 GPU 可测(adr/0025)。
pub mod wall;
// 卡墙的每帧驱动与 slint 绑定,seam 类型也在这。
mod wall_drive;
pub use wall_drive::{
    WallCardControls, WallControls, WallCoverControls,
    WallDrive,
};
mod profile;
mod theme;
// 同播只在原生上有:wasm 没有 WebRTC 之外的音频栈可推(见 `Cargo.toml` 的条件依赖)。
#[cfg(not(target_arch = "wasm32"))]
mod syncplay;

use slint::{ComponentHandle, RenderingState};

mod frame_stats;
mod lyric_push;
mod render_loop;

pub use render_loop::run_with_renderers;

/// 帧率读数开不开。`OSMOSIS_FPS` 设成任意值即开,与 `OSMOSIS_TAB` 同属调试开关。
///
/// 曾经是个 feature,但它不门控任何依赖 —— 关掉省下的只有一个 2Hz 定时器和每帧一次自增,
/// 却要在四个 manifest 里各声明一遍、还被三个 `bevy-3d` 隐含。开发时想不想看,本就不是
/// 编译期该管的事。
///
/// 两条路都要:桌面读运行期环境变量,拨开关不必重编;wasm 与 APK 读不到运行期环境变量
/// (页面由浏览器拉起、APK 由系统拉起),只能构建期烧进去 —— 同 `apps/android` 待
/// `SLINT_MCP_PORT` 的办法。
fn fps_enabled() -> bool {
    std::env::var("OSMOSIS_FPS").is_ok()
        || option_env!("OSMOSIS_FPS").is_some()
}

/// 最大页签下标:0=Home、1=Music。
///
/// 与 `app.slint` 里 `Nav.items` 的条数手工对齐 —— Slint 的全局属性不能当 Rust 常量用,
/// 加页时两处都要动。加漏了的症状是「`OSMOSIS_TAB=2` 静默停在 Music 页」。
const MAX_TAB: i32 = 3;

/// 创建窗口并完成所有领域状态绑定。[`run`] 与 [`run_with_renderers`] 的公共前半段。
///
/// 顺带交出可视化的数据源(频谱分析器句柄):它由 music 的播放器产出,
/// 而消费它的渲染通知回调装在 [`run_with_renderers`] 里 —— 两处只在这里相遇。
///
/// 调用前平台入口必须已经初始化好 slint 的渲染后端。
fn build_ui(
    media: impl FnOnce(MediaHooks) -> Box<dyn MediaControls>,
) -> (
    MainWindow,
    viz::Source,
    music::LyricFeed,
    music::CoverFeed,
) {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    // 先恢复上次的登录态,再绑界面 —— 绑定那一步会按登录与否决定先拉什么。
    // 恢复出来的 token 可能已被服务端吊销,那要等第一次请求 401 才知道。
    api::session::restore();
    // 接登录页。它按恢复出来的会话决定开局是登录页还是主界面。
    account::bind(&ui);

    // 主题要在别的绑定之前恢复:颜色是全局的,晚一步会让开局那一帧
    // 用错配色闪一下。
    theme::bind(&ui);
    profile::bind(&ui);
    aurora_btn::bind(&ui);

    let (viz_source, lyrics, cover) =
        music::bind(&ui, media);

    ui.global::<Shell>().set_show_fps(fps_enabled());
    ui.global::<Shell>()
        .set_platform(platform_name().into());
    // 设置页「关于」那一行。版本取本 crate 的(workspace 里同一个版本号)。
    ui.global::<Profile>().set_about_line(
        format!(
            "Osmosis {} · Slint + Bevy",
            env!("CARGO_PKG_VERSION")
        )
        .into(),
    );
    // 开局停在哪一页。默认 Home,`OSMOSIS_TAB` 覆盖它 —— 那是调试开关,
    // `just shot 420 1` 靠它直接截到 Music 页,不必再靠 MCP 模拟点击(那条路上有一串
    // 静默失败的坑,见 AGENTS.md)。没设或设歪了就留在 Home。
    if let Ok(tab) = std::env::var("OSMOSIS_TAB")
        && let Ok(tab) = tab.parse::<i32>()
    {
        ui.global::<Shell>()
            .set_current_tab(tab.clamp(0, MAX_TAB));
    }
    (ui, viz_source, lyrics, cover)
}

/// 创建窗口、绑定领域状态,然后运行事件循环直到窗口关闭。
///
/// 各平台入口在初始化好渲染后端后调用。不带 bevy 的端(web / ios)走这里:
/// 播放页覆层退回没有粒子与 warp 的形态,`.slint` 里零平台判断(见 [`VizImages`])。
pub fn run() {
    // 这条路上的端(web / iOS)还没有系统媒体控件的实现。
    let (ui, _viz_source, _lyrics, _cover) =
        build_ui(|_| Box::new(NoControls));
    // Timer 必须活到事件循环结束,否则会被立即析构、不再触发。
    // 关掉时连建都不建 —— 空转的 2Hz 唤醒在移动端是白耗电。
    let _fps_timer = fps_enabled().then(|| {
        let (frames, timer) = frame_stats::fps::start(&ui);
        // 无 bevy 的路径上没人装渲染通知,帧计数在这里自己接。
        ui.window()
            .set_rendering_notifier(move |state, _| {
                if matches!(
                    state,
                    RenderingState::BeforeRendering
                ) {
                    frames.set(frames.get() + 1);
                }
            })
            .ok();
        timer
    });

    ui.run().expect("event loop failed");
}

/// 当前编译目标的平台名,显示在标题里。
///
/// wasm 上 `std::env::consts::OS` 是 `"unknown"`,所以它得单独一支;其余各端
/// consts::OS 已经给出 android / ios / linux / windows / macos。
/// 全小写,与 cargo target 名对齐 —— 你看到的就是编译时选的那个 target。
fn platform_name() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "wasm"
    } else {
        std::env::consts::OS
    }
}
