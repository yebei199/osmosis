//! 设置页「关于」卡里的档位与应用内升级(#129)。
//!
//! 装是平台的事:安卓入口注入一个安装函数(见 [`install_updater`]),没人注入的端
//! 只显示版本与档位 —— 桌面的版本由 nixos_config 管。debug 包连本机后端,
//! 不参与升级:它和 Release 上的包不是同一个档,装上去就换了后端。
//!
//! 只在按钮上查,不在启动时自动查:GitHub 未登录的 API 一小时 60 次,
//! 而发版一周也就几次,没必要每次冷启动都问一遍。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use api::update::{Check, Update};
use slint::ComponentHandle;

use crate::{MainWindow, Profile};

/// 把一个核对过的 APK 交给系统安装器。`Ok` 只代表交出去了,装不装由用户在系统界面上点。
pub type Install = fn(&Path) -> Result<(), String>;

/// 平台注入的 (APK 落点目录, 安装函数)。`OnceLock` 的理由同 `install_download_store`。
static UPDATER: OnceLock<(PathBuf, Install)> =
    OnceLock::new();

/// 接上安装器。平台入口在 `run*` 之前调一次;不调就是这一端没有应用内升级。
pub fn install_updater(dir: PathBuf, install: Install) {
    if UPDATER.set((dir, install)).is_err() {
        log::warn!("安装器被接了第二次,后一次没有生效");
    }
}

const CHECK: &str = "检查更新";
const INSTALL: &str = "下载并安装";

/// 档位那一行。
fn tier_line(release: bool) -> String {
    if release {
        "release · 生产后端".to_owned()
    } else {
        format!("debug · 本机后端 {}", api::base_url())
    }
}

pub(crate) fn bind(ui: &MainWindow) {
    let release = api::is_release();
    let profile = ui.global::<Profile>();
    profile.set_tier_line(tier_line(release).into());

    let updater = UPDATER.get();
    let Some(&(ref dir, install)) =
        updater.filter(|_| release)
    else {
        profile.set_update_status(
            if !release {
                "debug 包不参与应用内升级"
            } else {
                "这一端的版本由系统包管理"
            }
            .into(),
        );
        return;
    };

    profile.set_update_action(CHECK.into());
    // 查到的那一版,等用户再点一下去装。
    let found: Rc<RefCell<Option<Update>>> = Rc::default();
    let weak = ui.as_weak();
    profile.on_update_requested(move || {
        let Some(ui) = weak.upgrade() else { return };
        match found.borrow_mut().take() {
            None => check(&ui, Rc::clone(&found)),
            Some(update) => {
                fetch(&ui, update, dir, install)
            }
        }
    });
}

/// 写这一节的两样。`action` 为空时按钮不上屏 —— 进行中不给按钮,
/// 连点两下会开出两条下载抢同一个文件。
fn say(ui: &MainWindow, status: &str, action: &str) {
    let profile = ui.global::<Profile>();
    profile.set_update_status(status.into());
    profile.set_update_action(action.into());
}

fn check(
    ui: &MainWindow,
    found: Rc<RefCell<Option<Update>>>,
) {
    say(ui, "正在检查…", "");
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let outcome = api::update::check().await;
        let Some(ui) = weak.upgrade() else { return };
        match outcome {
            Ok(Check::UpToDate) => say(
                &ui,
                &format!(
                    "已是最新版 {}",
                    api::update::current_version()
                ),
                CHECK,
            ),
            Ok(Check::NotReady(version)) => say(
                &ui,
                &format!(
                    "{version} 的安装包还没传上来,稍后再试"
                ),
                CHECK,
            ),
            Ok(Check::Available(update)) => {
                say(
                    &ui,
                    &format!("发现新版 {}", update.version),
                    INSTALL,
                );
                *found.borrow_mut() = Some(update);
            }
            Err(err) => {
                say(&ui, &format!("检查失败: {err}"), CHECK)
            }
        }
    })
    .ok();
}

fn fetch(
    ui: &MainWindow,
    update: Update,
    dir: &Path,
    install: Install,
) {
    say(ui, "下载中…", "");
    let dir = dir.to_path_buf();
    let reporting = ui.as_weak();
    let progress = move |done: u64, total: Option<u64>| {
        let Some(total) = total.filter(|&t| t > 0) else {
            return;
        };
        let text =
            format!("下载中 {}%", done * 100 / total);
        // 回调跑在 tokio 的工作线程上 —— 界面状态只有事件循环碰得。
        let _ = reporting.upgrade_in_event_loop(
            move |ui: MainWindow| {
                ui.global::<Profile>()
                    .set_update_status(text.into());
            },
        );
    };

    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let outcome =
            match api::update::fetch(&update, &dir, progress)
                .await
            {
                // 写进安装会话要把一百多 MB 拷一遍,不放在 UI 线程上。
                Ok(apk) => api::off_thread(move || install(&apk))
                    .await
                    .unwrap_or_else(|| {
                        Err("安装器中途出错".to_owned())
                    }),
                Err(err) => Err(err),
            };
        let Some(ui) = weak.upgrade() else { return };
        match outcome {
            // 首次升级系统会先要「安装未知应用」的许可;允许之后返回,
            // 系统安装器会接着问要不要更新。
            Ok(()) => say(
                &ui,
                "已交给系统安装器,按提示完成升级。首次会先要求允许安装未知应用",
                CHECK,
            ),
            Err(err) => {
                say(&ui, &format!("没装上: {err}"), CHECK)
            }
        }
    })
    .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单测不烘后端地址,跑的正是 debug 档:标出 debug,不给升级按钮。
    #[test]
    fn a_debug_build_is_marked_and_offers_no_update() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        bind(&ui);

        let profile = ui.global::<Profile>();
        assert!(!api::is_release());
        assert!(
            profile.get_tier_line().starts_with("debug"),
            "debug 档要标出来: {}",
            profile.get_tier_line()
        );
        assert_eq!(
            profile.get_update_action(),
            "",
            "debug 包不该有升级按钮"
        );
        assert_eq!(
            profile.get_update_status(),
            "debug 包不参与应用内升级"
        );
    }

    #[test]
    fn the_release_tier_names_the_production_backend() {
        assert_eq!(tier_line(true), "release · 生产后端");
    }
}
