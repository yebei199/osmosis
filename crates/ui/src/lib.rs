//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 本 crate 也是**组装点**:它把 [`api`] 的请求函数注入 [`app_core`],
//! 让二者互不相识。依赖方向单向,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

pub use media::{
    MediaCommand, MediaControls, MediaHooks, MediaStatus,
    NoControls, NowPlaying,
};
// 循环三态过 seam 原样透传:平台层(MPRIS/安卓)拿它翻成各自的方言。
pub use app_core::LoopMode;

mod viz;
pub use viz::{
    CoverUpdate, VIZ_AUDIO_BYTES, VizControls, VizCover,
    VizImages, VizPointer,
};

// 封面的解码与两套缓存。只在原生 target 上编,理由见模块开头。
#[cfg(not(target_arch = "wasm32"))]
mod imagery;

// 整页的绑定:登录、个人主页、搜索。
mod pages;
// 「我的库」:红心与歌单。
mod library;

// 播放进度的格式化。与列表里的时长同一条规矩:算在 Rust 侧,`.slint` 里只摆。
mod progress;

// 一次性提示的唯一出口。所有端都要 —— 报错的路各端都有。
mod notice;

mod media;
mod music;
// 下载的落点由平台入口注入,与系统媒体控件同一个接缝形状(docs/adr/0020)。
// 安卓那一份走 MediaStore;没人注入就是这一端不支持下载。
#[cfg(not(target_arch = "wasm32"))]
pub use music::{
    DownloadCommit, DownloadStore, install_download_store,
};

// 喂给 GPU 装饰层的 seam 数据与数学。
mod shader;
pub use shader::aurora_btn::{
    AuroraBtnControls, AuroraBtnSlotControls,
};
pub use shader::nav_glass::NavGlassControls;

// 卡墙的几何与交互动力学,纯数学、无 GPU 可测(adr/0025);
// 每帧驱动与 slint 绑定在 `wall::drive`,seam 类型也在那。
pub mod wall;
pub use wall::drive::{
    WallCardControls, WallControls, WallCoverControls,
    WallDrive,
};

// 明暗主题。颜色在 slint/theme.slint,这里只管那一位布尔值住在哪。
mod theme;
// 同播与遥控。只在原生上有(见 `Cargo.toml` 的条件依赖)。
#[cfg(not(target_arch = "wasm32"))]
mod sync;

use slint::{ComponentHandle, RenderingState};

// 帧循环这一侧。
mod runtime;

pub use runtime::render_loop::run_with_renderers;

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

/// 主线程卡顿探针开不开,见 `runtime::frame_stats::stall`。两条路的理由同 [`fps_enabled`]。
fn stall_enabled() -> bool {
    std::env::var("OSMOSIS_STALL").is_ok()
        || option_env!("OSMOSIS_STALL").is_some()
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
    pages::account::bind(&ui);

    // 主题要在别的绑定之前恢复:颜色是全局的,晚一步会让开局那一帧
    // 用错配色闪一下。
    theme::bind(&ui);
    pages::profile::bind(&ui);
    shader::aurora_btn::bind(&ui);

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

/// 会话、设置、封面与设备 id 的落点。与下载落点同一个接缝形状:
/// 安卓的应用私有目录只有平台入口那一层拿得到(`android_main` 的
/// `internal_data_path`),环境变量在那里给不出(#100)。
///
/// 必须赶在 [`run`] / [`run_with_renderers`] 之前 —— 登录态正是在那里恢复的。
/// 不调的端(桌面、web)照旧按 `XDG_STATE_HOME` / `HOME` / localStorage 走。
pub fn set_state_dir(dir: std::path::PathBuf) {
    api::set_state_dir(dir);
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
        let (frames, timer) =
            runtime::frame_stats::fps::start(&ui);
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
