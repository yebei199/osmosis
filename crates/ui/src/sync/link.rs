//! 本机在设备之间的那条信令连接:本机叫什么、信令连到哪、名册与连接状态写成什么话。
//!
//! 连接只有一条,当前只承载遥控器模式(`docs/adr/0030`);遥控那一侧的状态在
//! [`crate::sync::remote`]。重连、接管、版本协商在 `syncplay::Client` 里,
//! 那一层有对着真服务端跑的测试(`crates/syncplay/tests/`)。
//! 同播(WebRTC 推流)已删(#137)。
//!
//! 事件回调跑在信令自己的后台线程上,所以凡是碰 Slint 的动作都要
//! `upgrade_in_event_loop` 切回 UI 线程。

use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, VecModel};
use syncplay::{Client, DeviceDto, Event, Roster};

use crate::Shell;
use crate::{DeviceRow, MainWindow};

/// 读不到主机名时用的名字。
const UNKNOWN_HOST: &str = "device";

/// Linux 上主机名的出处。读文件而不是引一个 crate:这里只需要一个能区分设备的
/// 标签,不需要 POSIX 的完整语义。读不到(安卓上就没有这个文件)自有兜底。
const HOSTNAME_FILE: &str = "/etc/hostname";

/// 信令地址由 API 地址推出来。
///
/// 两者是同一个服务端,配两遍必然有一天只改了一处 —— 而那时的症状是
/// 「歌能搜、遥控没反应」,得翻两处配置才看得出来。
pub fn signalling_url(api_base: &str) -> String {
    match api_base.split_once("://") {
        // 只有 https 要升级成 wss。其余(http、以及没写协议的裸地址)一律 ws。
        Some(("https", rest)) => format!("wss://{rest}"),
        Some((_, rest)) => format!("ws://{rest}"),
        None => format!("ws://{api_base}"),
    }
}

/// 本机这台设备的 id。
///
/// 队列归**播放会话 / 输出设备**,不是账号(`docs/adr/0031` 二)—— 同账号
/// 两台设备各自本机播放不该互相覆盖,所以本机发布队列时拿的是这个。
/// 与信令入册用的是同一个 id,两处不能各算各的。
pub(crate) fn local_device_id() -> String {
    identity().id
}

/// 本机在名册里的身份。
fn identity() -> DeviceDto {
    let host = std::fs::read_to_string(HOSTNAME_FILE)
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| UNKNOWN_HOST.to_owned());
    let pid = std::process::id();

    identity_from(
        &host,
        pid,
        api::session::device_id(|| format!("{host}-{pid}")),
    )
}

/// 落盘的 id 配上带进程号的名字 —— 拆出来是为了能在测试里给定两次启动。
///
/// 名字里仍带进程号:**同一台机器上跑两个实例**是最常见的调试方式,而两个实例
/// 共用同一个状态目录,拿到的是同一个 id,界面上只剩名字分得开。真要让它们在
/// 服务端也算两台设备(服务端按 id 入册,同 id 会互相顶掉),给第二个实例指一份
/// 自己的 `OSMOSIS_DEVICE_FILE`。
fn identity_from(
    host: &str,
    pid: u32,
    id: String,
) -> DeviceDto {
    DeviceDto {
        id,
        name: format!("{host} #{pid}"),
    }
}

/// 连上信令,把名册与遥控接到界面上。返回音乐页要用的遥控把手。
pub fn bind(
    ui: &MainWindow,
) -> crate::sync::remote::Remote {
    let me = identity();
    // 名册由后台线程改、UI 线程读,所以是 `Mutex` 而不是 `RefCell`。
    let roster =
        Arc::new(Mutex::new(Roster::new(me.id.clone())));

    let remote = crate::sync::remote::new(ui);

    let weak = ui.as_weak();
    let client = Arc::new(Client::start(
        &signalling_url(api::base_url()),
        me,
        // 每次建连现取:开机时多半还没登录,而登录之后信令要能自己接上。
        api::session::token,
        {
            let remote = remote.clone();
            move |event| {
                handle(event, &weak, &roster, &remote)
            }
        },
    ));
    remote.attach(&client);

    crate::sync::remote::bind(ui, &remote);

    remote
}

/// 处理一条信令事件。**在后台线程上**跑。
///
/// `pub(crate)` 只为让 `music::tests` 够得着:建一个测试用主窗口的脚手架在那边
/// (`music::fixtures`)。
pub(crate) fn handle(
    event: Event,
    weak: &slint::Weak<MainWindow>,
    roster: &Arc<Mutex<Roster>>,
    remote: &crate::sync::remote::Remote,
) {
    // 遥控那几条归 `crate::sync::remote`,这里只管名册与连接本身的状态。
    crate::sync::remote::handle(&event, remote);

    match event {
        Event::Roster(devices) => {
            let others = {
                let mut roster = lock(roster);
                roster.update(devices);
                roster.others().to_vec()
            };
            show_devices(weak, others);
        }
        Event::Failed(message) => {
            // 走提示:失败是**这一刻**的事,写进常驻的状态行就没人会重算它
            // (见 `crate::notice`)。
            let message = describe_link_failure(&message);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                crate::notice::show(&ui, message);
            });
        }
        // 版本不对与掉线**分开说**,这正是那道握手协商的意义(`docs/adr/0031`)。
        //
        // 混成 `Event::Failed` 那一句的话,用户看到的是一句等一等就好了的话,
        // 而实际上等多久都不会好 —— 得去升级其中一端。走横幅不走提示:提示几秒
        // 就没了,而这是一个**持续为真**的状态,升级之前它一直成立。
        Event::Incompatible { ours, theirs } => {
            let message =
                describe_incompatible(ours, theirs);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                crate::notice::banner_without_link(
                    &ui, message,
                );
            });
        }
        // 与 HTTP 那侧拿到 401 是同一件事,善后也走同一处。
        // 信令自己不会重试,下一个 token 到位时它会自己接上。
        Event::Unauthorized(rejected) => {
            if !api::session::expire_if_current(Some(
                &rejected,
            )) {
                return;
            }
            let _ = weak.upgrade_in_event_loop(|ui| {
                crate::pages::account::to_login_page(
                    &ui,
                    "遥控信令被服务端拒绝",
                );
            });
        }
        // 遥控那几条上面已经处理过了。
        _ => {}
    }
}

/// 把设备列表推到界面上。
fn show_devices(
    weak: &slint::Weak<MainWindow>,
    devices: Vec<DeviceDto>,
) {
    // 转成 Slint 的行是在 UI 线程里做的:`DeviceDto` 是纯字符串,跨线程没问题,
    // 而 Slint 的模型只能在它自己的线程上建。
    let _ = weak.upgrade_in_event_loop(move |ui| {
        let rows: Vec<DeviceRow> = devices
            .iter()
            .map(|device| DeviceRow {
                id: device.id.clone().into(),
                name: device.name.clone().into(),
            })
            .collect();
        ui.global::<Shell>().set_devices(ModelRc::new(
            VecModel::from(rows),
        ));
    });
}

/// 取锁。名册的锁里只有赋值和克隆,不会 panic,所以中毒了就是别处出了大问题 ——
/// 那时候继续用一个来路不明的状态比直接停下来更糟。
fn lock<T>(
    value: &Arc<Mutex<T>>,
) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("名册锁中毒")
}

/// 一次普通失败怎么说。**等一等会自己好**,所以不叫人去做任何事。
fn describe_link_failure(message: &str) -> String {
    format!("遥控连接失败: {message}")
}

/// 版本对不上怎么说。
///
/// 两个版本号都要在里面:少了它,用户只知道用不了,不知道该升哪一端。
/// 对端旧到不报版本时说「太旧」而不是编一个号 —— 编出来的号会被拿去
/// 找一个不存在的版本。
fn describe_incompatible(
    ours: u32,
    theirs: Option<u32>,
) -> String {
    format!(
        "版本对不上,遥控用不了:本机协议 {ours},服务端 {}。升级其中一端。",
        theirs.map_or_else(
            || "太旧,报不出版本".to_owned(),
            |version| version.to_string()
        )
    )
}

#[cfg(test)]
mod tests;
