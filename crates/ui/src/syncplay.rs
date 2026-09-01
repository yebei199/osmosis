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

/// 本机在同播里的身份。
///
/// 身份是**自报的在场证明**,不是账号(`docs/adr/0009`),所以不落盘、不校验,
/// 每次启动重新生成即可。
fn identity() -> DeviceDto {
    let host = std::fs::read_to_string(HOSTNAME_FILE)
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| UNKNOWN_HOST.to_owned());

    identity_from(&host, std::process::id())
}

/// 主机名加进程号 —— 拆出来是为了能在测试里给定两个进程号。
///
/// 带上进程号是因为**同一台机器上跑两个实例**是最常见的调试方式。都叫主机名的话,
/// 界面上两行一模一样,点哪一行都说不清推给了谁;更糟的是服务端按 id 入册,
/// 两个同 id 的实例会互相顶掉。
fn identity_from(host: &str, pid: u32) -> DeviceDto {
    DeviceDto {
        id: format!("{host}-{pid}"),
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

/// 把同播接到音乐页上。返回音乐页要用的把手。
pub fn bind(
    ui: &MainWindow,
    player: &SharedPlayer,
) -> Sync {
    let me = identity();
    // 名册与角色都由后台线程改、UI 线程读,所以是 `Mutex` 而不是 `RefCell`。
    let roster =
        Arc::new(Mutex::new(Roster::new(me.id.clone())));
    let role = Arc::new(Mutex::new(Role::Alone));

    let weak = ui.as_weak();
    let player = player.clone();
    let client = Arc::new(Client::start(
        &signalling_url(api::base_url()),
        me,
        {
            let roster = roster.clone();
            let role = role.clone();
            move |event| {
                handle(
                    event, &weak, &roster, &role, &player,
                )
            }
        },
    ));

    bind_push(ui, &client, &role);
    ui.global::<Shell>()
        .set_sync_text(describe_role(&Role::Alone).into());

    Sync {
        client,
        role,
        weak: ui.as_weak(),
    }
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
fn handle(
    event: Event,
    weak: &slint::Weak<MainWindow>,
    roster: &Arc<Mutex<Roster>>,
    role: &Arc<Mutex<Role>>,
    player: &SharedPlayer,
) {
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
            let message = format!("同播失败: {message}");
            let _ = weak.upgrade_in_event_loop(move |ui| {
                crate::notice::show(&ui, message);
            });
        }
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

#[cfg(test)]
mod tests;
