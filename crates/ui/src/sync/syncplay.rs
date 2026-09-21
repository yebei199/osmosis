//! 音乐页的同播接线:列出在线设备,点一下把正在放的这首推过去。
//!
//! 本模块只做三件界面的事 —— 本机叫什么、信令连到哪、状态写成什么话。
//! 谁当主控、候选往哪转、轨绑给谁,都在 `syncplay::Client` 里,那一层有
//! 对着真服务端跑的测试(`crates/syncplay/tests/client.rs`)。
//!
//! 事件回调跑在同播自己的后台线程上,所以凡是碰 Slint 的动作都要
//! `upgrade_in_event_loop` 切回 UI 线程;唯独播放不用 —— [`audio::Player`]
//! 是 `Send + Sync` 的,听众收到的声音直接在后台线程上就能出。

use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, VecModel};
use syncplay::{Client, DeviceDto, Event, Role, Roster};

use crate::Player;
use crate::Shell;
use crate::{DeviceRow, MainWindow};

/// 读不到主机名时用的名字。
const UNKNOWN_HOST: &str = "device";

/// Linux 上主机名的出处。读文件而不是引一个 crate:同播只需要一个能区分设备的
/// 标签,不需要 POSIX 的完整语义。读不到(安卓上就没有这个文件)自有兜底。
const HOSTNAME_FILE: &str = "/etc/hostname";

/// 播放器句柄的类型。开不出设备不是致命错误,所以是个 `Result`。
type SharedPlayer =
    Arc<Result<audio::Player, audio::AudioError>>;

/// 信令地址由 API 地址推出来。
///
/// 两者是同一个服务端,配两遍必然有一天只改了一处 —— 而那时的症状是
/// 「歌能搜、推送没反应」,得翻两处配置才看得出来。
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
/// 与同播入册用的是同一个 id,两处不能各算各的。
pub(crate) fn local_device_id() -> String {
    identity().id
}

/// 本机在同播里的身份(正身)。
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

/// 把同播状态翻译成一行人类可读的文案。
pub fn describe_role(role: &Role) -> String {
    match role {
        Role::Alone => "同播: 未开始".to_owned(),
        Role::Host { listeners } => {
            format!("同播: 推给 {} 台设备", listeners.len())
        }
        Role::Listener { host } => {
            format!("同播: 正在收听 {host}")
        }
    }
}

/// 音乐页拿在手里的同播把手:交采样、推设备、查角色、退出。
///
/// [`Client`] 管连接,这里多出来的是**界面侧的角色状态** —— 自动续播要靠
/// `is_listening` 决定要不要闭嘴(收听时切歌会捣掉对面推来的声音),
/// 播放键要靠 [`Sync::leave`] 实现「按一下就退出收听」。
#[derive(Clone)]
pub struct Sync {
    client: Arc<Client>,
    role: Arc<Mutex<Role>>,
    weak: slint::Weak<MainWindow>,
}

impl Sync {
    /// 把本机正在放的采样交给同播。
    ///
    /// 采样就是 `f32`(rodio 的 `Sample` 别名)—— 写成 `f32` 免得 `ui`
    /// 为一个类型别名直接依赖 rodio。
    pub fn feed(
        &self,
        samples: std::sync::mpsc::Receiver<f32>,
    ) {
        self.client.feed(samples);
    }

    /// 本机此刻是不是听众。
    pub fn is_listening(&self) -> bool {
        matches!(&*lock(&self.role), Role::Listener { .. })
    }

    /// 退出同播,回到单机。角色与状态行同步复位。
    pub fn leave(&self) {
        self.client.leave();
        *lock(&self.role) = Role::Alone;
        show_role(&self.weak, describe_role(&Role::Alone));
        // 播放行从「收听中…」退回空闲文案。退出后紧接着播自己的歌时,
        // Loading 会立刻盖掉它,这里只兜"退出后什么都不放"的那条路。
        let _ = self.weak.upgrade_in_event_loop(|ui| {
            ui.global::<Player>().set_playback_text(
                crate::music::describe_playback(
                    &app_core::PlaybackState::Idle,
                )
                .into(),
            );
        });
    }
}

/// 把同播与遥控接到音乐页上。返回音乐页要用的两个把手。
///
/// 两种会话形态共用同一条信令连接(`docs/adr/0030`),所以共用一个 [`Client`] ——
/// 各建一条的话,同一台设备会在名册里出现两次,而第二条的 id 谁也认不出来。
pub fn bind(
    ui: &MainWindow,
    player: &SharedPlayer,
) -> (Sync, crate::sync::remote::Remote) {
    let me = identity();
    // 名册与角色都由后台线程改、UI 线程读,所以是 `Mutex` 而不是 `RefCell`。
    let roster =
        Arc::new(Mutex::new(Roster::new(me.id.clone())));
    let role = Arc::new(Mutex::new(Role::Alone));

    let remote = crate::sync::remote::new(ui);

    let weak = ui.as_weak();
    let player = player.clone();
    let client = Arc::new(Client::start(
        &signalling_url(api::base_url()),
        me,
        // 每次建连现取:开机时多半还没登录,而登录之后同播要能自己接上。
        api::session::token,
        {
            let roster = roster.clone();
            let role = role.clone();
            let remote = remote.clone();
            move |event| {
                handle(
                    event, &weak, &roster, &role, &player,
                    &remote,
                )
            }
        },
    ));
    remote.attach(&client);

    bind_push(ui, &client, &role);
    crate::sync::remote::bind(ui, &remote);
    ui.global::<Shell>()
        .set_sync_text(describe_role(&Role::Alone).into());

    (
        Sync {
            client,
            role,
            weak: ui.as_weak(),
        },
        remote,
    )
}

/// 一个谁也不连的把手,给测试用。
///
/// [`bind`] 会当场把客户端连去 `api::base_url()` 的信令地址。那个地址是
/// **编译期**决定的(见 `api::base_url`),于是测试连去哪里取决于构建时的
/// `OSMOSIS_API_BASE`:不设时是本机 3000 —— 而开发服务器正好在那儿,测试
/// 于是会因为本机有没有开 server-dev 而表现不同;设成集群地址跑一次
/// `cargo test`,那就是拿生产环境当测试靶子。两种都不能要。
///
/// 需要 `Deck` 的测试并不关心同播,给它一个空壳即可。
#[cfg(test)]
pub(crate) fn detached(ui: &MainWindow) -> Sync {
    Sync {
        client: Arc::new(Client::detached()),
        role: Arc::new(Mutex::new(Role::Alone)),
        weak: ui.as_weak(),
    }
}

/// 处理一条同播事件。**在后台线程上**跑。
///
/// `pub(crate)` 只为让 `music::tests` 够得着:它是这一层唯一有分支的函数,
/// 而建一个测试用主窗口的脚手架在那边(`music::fixtures`)。
pub(crate) fn handle(
    event: Event,
    weak: &slint::Weak<MainWindow>,
    roster: &Arc<Mutex<Roster>>,
    role: &Arc<Mutex<Role>>,
    player: &SharedPlayer,
    remote: &crate::sync::remote::Remote,
) {
    // 遥控那几条归 `crate::sync::remote`:同播与遥控共用这条连接,但状态毫无重叠。
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
        Event::Listening { host, source } => {
            // 直接出声,不切 UI 线程:切过去反而会让音频的起播等在
            // 下一帧上,而界面正忙时那可能是几十毫秒之后。
            if let Ok(player) = player.as_ref() {
                player.play(source);
            }
            // 存的是名字而非 id:这一份 Role 只服务于状态行,
            // 而状态行上的写法必须和用户点过的那一行一致。
            *lock(role) = Role::Listener {
                host: display_name(roster, &host),
            };
            show_role(weak, describe_role(&lock(role)));
            // 有声音在出,控制键该画 ⏸ —— 此刻按它的语义是「退出收听」。
            // 播放状态行一并接管:上面可能还挂着本机上一首的「正在播放 X」,
            // 而扬声器里已经是推来的流,那行等于在撒谎。曲名主控没发过来,
            // 写"收听中"是诚实的全部。
            let _ = weak.upgrade_in_event_loop(|ui| {
                ui.global::<Player>().set_is_playing(true);
                ui.global::<Player>()
                    .set_playback_text("收听中…".into());
            });
        }
        Event::Failed(message) => {
            // 走提示,不写角色那一行:连不上的时候角色一动没动,那一行此刻
            // 依然为真,而失败是**这一刻**的事。写进去就没人会重算它,那句话
            // 会一直挂到角色碰巧变一次为止(见 `crate::notice`)。
            let message = describe_sync_failure(&message);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                crate::notice::show(&ui, message);
            });
        }
        // 版本不对与掉线**分开说**,这正是那道握手协商的意义(`docs/adr/0031`)。
        //
        // 混成 `Event::Failed` 那一句「同播失败: …」的话,用户看到的是一句
        // 等一等就好了的话,而实际上等多久都不会好 —— 得去升级其中一端。
        // 走横幅不走提示:提示几秒就没了,而这是一个**持续为真**的状态,
        // 升级之前它一直成立。
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
        // 同播自己不会重试,下一个 token 到位时它会自己接上。
        Event::Unauthorized => {
            let _ = weak.upgrade_in_event_loop(|ui| {
                crate::pages::account::to_login_page(
                    &ui,
                    "同播信令被服务端拒绝",
                );
            });
        }
        // 遥控那几条上面已经处理过了。
        _ => {}
    }
}

/// 一台设备在界面上该怎么称呼。
///
/// 信令只带 id,而 id 是给机器认的(`主机名-进程号`)。名册里有对端自报的名字,
/// 就用它 —— 用户在列表上点的是那个名字,状态行里出现另一个写法只会让人以为
/// 推给了别的设备。名册还没到就退回 id,总比一行空白强。
fn display_name(
    roster: &Arc<Mutex<Roster>>,
    id: &str,
) -> String {
    lock(roster)
        .others()
        .iter()
        .find(|device| device.id == id)
        .map_or_else(
            || id.to_owned(),
            |device| device.name.clone(),
        )
}

/// 点一台设备就把当前这首推过去。
fn bind_push(
    ui: &MainWindow,
    client: &Arc<Client>,
    role: &Arc<Mutex<Role>>,
) {
    let client = client.clone();
    let role = role.clone();
    let weak = ui.as_weak();

    ui.global::<Shell>().on_push_to(move |id| {
        let id = id.to_string();
        client.push(&id);

        // 乐观更新:连接建起来要几百毫秒,而按下去必须立刻有反应。
        // 真失败了会有 `Event::Failed` 把这一行改掉。
        let mut role = lock(&role);
        let listeners = match &*role {
            Role::Host { listeners } => {
                let mut listeners = listeners.clone();
                if !listeners.contains(&id) {
                    listeners.push(id);
                }
                listeners
            }
            _ => vec![id],
        };
        *role = Role::Host { listeners };
        // 这里已经在 UI 线程上,直接改 —— 走 `show_role` 会让文案晚一轮事件循环才出来。
        if let Some(ui) = weak.upgrade() {
            ui.global::<Shell>()
                .set_sync_text(describe_role(&role).into());
        }
    });
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

/// 把状态行推到界面上。
fn show_role(weak: &slint::Weak<MainWindow>, text: String) {
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.global::<Shell>().set_sync_text(text.into());
    });
}

/// 取锁。同播的锁里只有赋值和克隆,不会 panic,所以中毒了就是别处出了大问题 ——
/// 那时候继续用一个来路不明的状态比直接停下来更糟。
fn lock<T>(
    value: &Arc<Mutex<T>>,
) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("同播状态锁中毒")
}

/// 一次普通失败怎么说。**等一等会自己好**,所以不叫人去做任何事。
fn describe_sync_failure(message: &str) -> String {
    format!("同播失败: {message}")
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
        "版本对不上,遥控与同播都用不了:本机协议 {ours},服务端 {}。升级其中一端。",
        theirs.map_or_else(
            || "太旧,报不出版本".to_owned(),
            |version| version.to_string()
        )
    )
}

#[cfg(test)]
mod tests;
