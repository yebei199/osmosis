//! 个人页的网易云一节:绑定状态、二维码、扫码轮询、解绑。
//!
//! 凭据按**账号**分片存在上游(server 的 `docs/adr/0017`),所以这里绑的不是
//! 这台设备 —— 桌面绑好,手机上登同一个账号就已经是绑好的。
//!
//! 节奏在这一层:`api` 那侧两端都没有可用的定时器(native 的 tokio 只开了
//! 运行时,wasm 上压根没有 tokio),而 `slint::Timer` 三端都有。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{
    ComponentHandle, Rgb8Pixel, SharedPixelBuffer, Timer,
    TimerMode,
};

use crate::MainWindow;
use crate::Profile;

/// 两次询问之间隔多久。
///
/// 两秒:用户扫完码到界面反应过来的那段空白要短到察觉不出,而再密就是
/// 替上游去打网易云的接口。
const POLL_INTERVAL: core::time::Duration =
    core::time::Duration::from_secs(2);

/// 二维码每个模块画多少像素。
const MODULE_PX: u32 = 6;

/// 码四周留几个模块的空白。
///
/// 四个是 QR 规范定的静区。少了的话扫码器找不到定位图形 —— 而现象是
/// 「这张码扫不出来」,看起来像码本身画错了。
const QUIET_MODULES: u32 = 4;

/// 轮询这一侧的全部可变状态。
///
/// ponytail: `timer` 与它自己持有的回调绕成一个 Rc 环,于是这份状态要到进程
/// 退出才回收。全进程只有这一份,不为它再套一层 Weak。
#[derive(Clone, Default)]
struct Scan {
    /// 节拍。重进个人页时 restart 同一个,不会攒出第二路轮询。
    timer: Rc<Timer>,
    /// 此刻屏幕上这张码的 key。空串表示手里没有码。
    key: Rc<RefCell<String>>,
    /// 上一拍还没回来。慢网络下不叠请求。
    busy: Rc<Cell<bool>>,
}

/// 接上解绑回调,并交出「进了个人页」时要做的那件事。
///
/// 交出去而不是自己挂 `shown`:那个回调只有一个,统计那一半也要用
/// (见 [`super::bind`])。
pub(super) fn bind(
    ui: &MainWindow,
) -> impl Fn(&MainWindow) + 'static {
    let scan = Scan::default();

    let weak = ui.as_weak();
    let unbinding = scan.clone();
    ui.global::<Profile>().on_unbind_netease(move || {
        let weak = weak.clone();
        let scan = unbinding.clone();
        let _ = slint::spawn_local(async move {
            let done = api::netease_unbind().await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                // 解绑完立刻重问一次:那一问会顺手把新的二维码摆出来
                Ok(()) => refresh(&ui, &scan),
                Err(err) => {
                    if !crate::account::handle_session_expiry(
                        &ui, &err,
                    ) {
                        hint(&ui, format!("解绑失败: {err}"));
                    }
                }
            }
        });
    });

    move |ui: &MainWindow| refresh(ui, &scan)
}

/// 问一次绑定状态,并据此决定摆什么。
fn refresh(ui: &MainWindow, scan: &Scan) {
    // 先把上一轮的节拍停掉:这一页可能是被重新进入的,而那张码多半已经过期。
    scan.timer.stop();
    scan.key.borrow_mut().clear();

    let weak = ui.as_weak();
    let scan = scan.clone();
    let _ = slint::spawn_local(async move {
        let status = api::netease_status().await;
        let Some(ui) = weak.upgrade() else { return };
        match status {
            Ok(status) => show_status(&ui, &scan, &status),
            Err(err) => {
                if !crate::account::handle_session_expiry(
                    &ui, &err,
                ) {
                    hint(
                        &ui,
                        format!("查不到绑定状态: {err}"),
                    );
                }
            }
        }
    });
}

/// 把绑定状态摆上页面。没绑就顺手要一张码。
fn show_status(
    ui: &MainWindow,
    scan: &Scan,
    status: &api::NeteaseStatusDto,
) {
    ui.global::<Profile>().set_netease_bound(status.bound);
    ui.global::<Profile>().set_netease_nickname(
        status.nickname.clone().unwrap_or_default().into(),
    );

    if status.bound {
        // 绑好了就不必再画码,也不必再问
        ui.global::<Profile>()
            .set_netease_qr(slint::Image::default());
        hint(ui, String::new());
        return;
    }

    request_code(ui, scan);
}

/// 要一张新码,画出来,并把节拍起起来。
fn request_code(ui: &MainWindow, scan: &Scan) {
    hint(ui, "正在取二维码…".to_owned());

    let weak = ui.as_weak();
    let scan = scan.clone();
    let _ = slint::spawn_local(async move {
        let code = api::netease_qr().await;
        let Some(ui) = weak.upgrade() else { return };
        match code {
            Ok(code) => {
                show_code(&ui, &scan, &code);
                start_polling(&ui, &scan);
            }
            Err(err) => {
                if !crate::account::handle_session_expiry(
                    &ui, &err,
                ) {
                    hint(
                        &ui,
                        format!("取不到二维码: {err}"),
                    );
                }
            }
        }
    });
}

/// 把一张码摆到屏幕上,并记住它的 key。
fn show_code(
    ui: &MainWindow,
    scan: &Scan,
    code: &api::QrLoginDto,
) {
    *scan.key.borrow_mut() = code.key.clone();

    match render(&code.url) {
        Some(image) => {
            ui.global::<Profile>().set_netease_qr(image);
            hint(ui, "用手机上的网易云扫这张码".to_owned());
        }
        // 画不出来就别摆一张空图当码:用户会对着空白扫
        None => {
            ui.global::<Profile>()
                .set_netease_qr(slint::Image::default());
            hint(ui, "二维码画不出来,回头再试".to_owned());
        }
    }
}

/// 起节拍:每 [`POLL_INTERVAL`] 问一次这张码的进展。
fn start_polling(ui: &MainWindow, scan: &Scan) {
    let weak = ui.as_weak();
    let ticking = scan.clone();

    scan.timer.start(
        TimerMode::Repeated,
        POLL_INTERVAL,
        move || {
            // 上一拍还在路上就跳过这一拍。慢网络下不叠请求 ——
            // 叠起来的话回来的顺序不保证,界面会在两个状态之间跳。
            if ticking.busy.get() {
                return;
            }
            let key = ticking.key.borrow().clone();
            if key.is_empty() {
                return;
            }

            ticking.busy.set(true);
            let weak = weak.clone();
            let scan = ticking.clone();
            let _ = slint::spawn_local(async move {
                let step = api::qr_poll(&key).await;
                scan.busy.set(false);
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                advance(&ui, &scan, step);
            });
        },
    );
}

/// 一次轮询回来之后往下走一步。
fn advance(
    ui: &MainWindow,
    scan: &Scan,
    step: Result<api::QrStep, api::ApiError>,
) {
    match step {
        Ok(api::QrStep::Waiting) => {
            hint(ui, "用手机上的网易云扫这张码".to_owned());
        }
        Ok(api::QrStep::Scanned) => {
            hint(ui, "扫到了,在手机上按确认".to_owned());
        }
        // 绑好了:停下,并重问一次状态 —— 昵称在那一问里。
        Ok(api::QrStep::Bound) => {
            scan.timer.stop();
            refresh(ui, scan);
        }
        Ok(api::QrStep::Renewed(code)) => {
            show_code(ui, scan, &code);
        }
        Err(err) => {
            if !crate::account::handle_session_expiry(
                ui, &err,
            ) {
                hint(ui, format!("问不到扫码进展: {err}"));
            }
        }
    }
}

/// 这一节此刻的一句话。
fn hint(ui: &MainWindow, text: String) {
    ui.global::<Profile>().set_netease_hint(text.into());
}

/// 把二维码的内容画成一张图。
///
/// 自己画而不是让服务端渲成 PNG 传过来:那样 web 端还得再解一次码,而
/// 解码器只编进原生端(见 `contract::QrLoginDto`)。
///
/// 黑白定死,不跟主题走:扫码器认的是对比度,深色底上的浅色码有一半的
/// 机器扫不出来。
fn render(content: &str) -> Option<slint::Image> {
    let code = qrcode::QrCode::new(content).ok()?;
    let modules = u32::try_from(code.width()).ok()?;
    let side = (modules + QUIET_MODULES * 2) * MODULE_PX;

    let mut buffer =
        SharedPixelBuffer::<Rgb8Pixel>::new(side, side);
    let pixels = buffer.make_mut_slice();
    pixels.fill(Rgb8Pixel {
        r: 255,
        g: 255,
        b: 255,
    });

    for (i, module) in code.to_colors().iter().enumerate() {
        if *module == qrcode::Color::Light {
            continue;
        }
        let i = u32::try_from(i).ok()?;
        let left =
            (i % modules + QUIET_MODULES) * MODULE_PX;
        let top = (i / modules + QUIET_MODULES) * MODULE_PX;

        for y in top..top + MODULE_PX {
            for x in left..left + MODULE_PX {
                pixels[(y * side + x) as usize] =
                    Rgb8Pixel { r: 0, g: 0, b: 0 };
            }
        }
    }

    Some(slint::Image::from_rgb8(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 画出来的是一张方图,边长 = (模块数 + 两侧静区) × 每模块像素。
    ///
    /// 这条钉的是那两层循环的下标算术:算错一格不会 panic(缓冲是整块的),
    /// 只会让码歪掉 —— 而歪掉的码在屏幕上仍然像一张码,只是扫不出来。
    #[test]
    fn a_code_renders_to_a_square_with_its_quiet_zone() {
        let url =
            "https://music.163.com/login?codekey=abcdef";
        let code = qrcode::QrCode::new(url)
            .expect("这段内容该编得出来");
        let modules = code.width() as u32;

        let image = render(url).expect("该画得出来一张图");

        let expected =
            (modules + QUIET_MODULES * 2) * MODULE_PX;
        assert_eq!(image.size().width, expected);
        assert_eq!(image.size().height, expected);
    }

    /// 静区真的是白的,而定位图形真的是黑的。
    ///
    /// 只验尺寸的话,一张全白的图也能过 —— 而它在屏幕上也像一张卡片,
    /// 只是谁也扫不出来。
    #[test]
    fn the_quiet_zone_stays_white_and_the_finder_is_dark() {
        let image = render("https://example.com/qr")
            .expect("该画得出来一张图");
        let buffer = image
            .to_rgb8()
            .expect("刚画的就是 rgb8,取得回来");
        let side = buffer.width();
        let pixels = buffer.as_slice();

        let at = |x: u32, y: u32| {
            pixels[(y * side + x) as usize]
        };

        assert_eq!(
            at(0, 0),
            Rgb8Pixel {
                r: 255,
                g: 255,
                b: 255
            },
            "左上角是静区,必须留白"
        );
        // 静区之后第一个模块是左上定位图形的一角,一定是黑的
        let first = QUIET_MODULES * MODULE_PX;
        assert_eq!(
            at(first, first),
            Rgb8Pixel { r: 0, g: 0, b: 0 },
            "定位图形该是黑的 —— 它是扫码器找码的依据"
        );
    }
}
