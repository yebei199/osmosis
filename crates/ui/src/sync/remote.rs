//! 遥控器模式的界面接线:输出设备、被控端的锁定态、命令与上报。
//!
//! 与 [`crate::sync::syncplay`] 分两个模块,因为它们是两种会话形态(`docs/adr/0030`):
//! 同播是主控出声、听众收流;遥控器是遥控器**不**出声、被控端自己拉直链自己播。
//! 两者共用同一条信令连接(所以共用一个 [`Client`]),但状态与界面毫无重叠。
//!
//! 事件回调跑在同播自己的后台线程上。命令却必须在 UI 线程上执行 —— 它要碰
//! `Deck`,而 `Deck` 全是 `Rc`,不是 `Send`。所以走两步:后台线程把命令塞进
//! 收件箱,再叫一声 UI 线程;真正执行的那段挂在 `Shell.remote-command` 上,
//! 由音乐页去接(见 `crate::music`)。

mod rules;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use app_core::{
    Output, RemoteCommand, RemoteStateDto, RemoteView,
};
use slint::ComponentHandle;
use syncplay::{Client, DeviceDto};

pub(crate) use rules::{
    accepts_control, describe_claim_failed,
    describe_controlled, describe_lost, describe_output,
    describe_remote, describe_revoked, describe_too_large,
    describe_unavailable, lost_remote,
};

use crate::{MainWindow, Player, Shell};

/// 本机挂钟的毫秒。
///
/// `app-core` 不碰时钟(它要编到 wasm,见 `docs/adr/0002`),插值与过期判定
/// 因此都收一个「现在几点」。这一层是它唯一的出处。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 一次提交的下场。
///
/// 分这么细是因为**出路各不相同**:输出在本机该走本机路径,过期与没接上客户端
/// 该说「控制暂不可用」,而超限要说「队列太长」—— 混成一个布尔的话,界面只能
/// 对四种毫不相干的情况说同一句话,而用户照着那句话做不了任何事。
#[derive(Debug, PartialEq, Eq)]
pub enum Submitted {
    /// 交给客户端了。**仅此而已** —— 后面三跳都可能丢掉它。
    Ok,
    /// 输出本来就在本机,这一下不该走信令。
    NotRemote,
    /// 手上那份被控端状态已经过期,照着它发命令等于蒙(`docs/adr/0030`)。
    Stale,
    /// 同播客户端还没接上(启动期那一微秒的空窗)。
    NoClient,
    /// 这一条太大,**发出去会撞掉整条连接**,所以在这里拒掉。
    TooLarge { bytes: usize, limit: usize },
}

/// 一次点播交出去之后,多久内还算「在路上」。
///
/// 真机上点下去到对面出声要一两秒(#121),上报一秒一条;十秒还没见对面
/// 报上这一首,多半是丢了,再点一下该放行。
const PENDING_PLAY_MS: u64 = 10_000;

/// 音乐页拿在手里的遥控把手。
#[derive(Clone)]
pub struct Remote {
    inner: Arc<Inner>,
}

struct Inner {
    /// 同播那个客户端,两种会话形态共用一条信令连接。
    ///
    /// `OnceLock` 而不是直接持有:事件回调要在 [`Client::start`] **之前**
    /// 就交出去,而客户端要等它返回才拿得到。这一微秒的空窗里到达的事件
    /// 发不出东西 —— 那没关系,每秒一次的上报下一拍就把状态补齐了。
    client: OnceLock<Arc<Client>>,
    /// 声音从哪台设备出来。
    output: Mutex<Output>,
    /// 被控端报来的那份状态 —— 本机作遥控器时才有东西。
    view: Mutex<RemoteView>,
    /// 正在遥控本机的那台设备 —— 本机作被控端时才有东西。
    controlled_by: Mutex<Option<DeviceDto>>,
    /// 遥控器发来、还没执行的命令。
    inbox: Mutex<VecDeque<RemoteCommand>>,
    /// 封面已经取到哪一首了 —— 上报每秒一条,按它的频率取图等于每秒一次下载。
    cover_id: Mutex<String>,
    /// 上一拍轮询看到的是不是「输出在别的设备上」,用来认出回到本机那一下。
    was_remote: Mutex<bool>,
    /// 这一次执行会话的标识:进程启动时的毫秒挂钟。
    ///
    /// 与服务端 `play_queue_reports.epoch` 是同一个数。跨重启比大小要靠它 ——
    /// 只比序号的话,重启之后的第 1 条永远排在重启前的第 900 条后面
    /// (见 `app_core::RemoteView::accept`)。
    ///
    // ponytail: 挂钟倒退时新进程会拿到更小的 epoch,那一次的上报会被对端
    // 全丢掉。真出现再换单调时钟加持久计数器
    epoch: i64,
    /// 本次会话里已经报到第几条。每报一次加一。
    state_seq: AtomicU64,
    /// 交出去、被控端还没报上来的那次点播:曲目 id 与交出去的时刻。
    ///
    /// 连点去重要它:点下去到对面上报「在放这一首」之间有一两秒,这段
    /// 时间里上报还是上一首,光看上报挡不住第二下(#113)。
    pending_play: Mutex<Option<(String, u64)>>,
    /// 走到远端分支的点播有几下(见 [`Remote::note_play_submitted`])。
    #[cfg(test)]
    play_submits: AtomicU64,
    /// 交给客户端去忘掉的持权有几次(见 [`Remote::release_claim`])。
    ///
    /// [`Client::release_control`] 只往一条通道里塞一条指令,测试里的
    /// [`Client::detached`] 连接收端都没有 —— 不记下来就观察不到它发没发。
    #[cfg(test)]
    releases: AtomicU64,
    /// 测试里记下真的交出去了哪些命令。
    ///
    /// [`Client::detached`] 当场丢掉通道的接收端,而 [`Client::command`] 本来
    /// 就不回报结果 —— 于是「这一下到底发没发出去」在测试里根本观察不到,
    /// 而遥控器侧要钉的恰好就是它(全仓此前没有一条测试走过这条路)。
    #[cfg(test)]
    sent: Mutex<Vec<RemoteCommand>>,
    weak: slint::Weak<MainWindow>,
}

impl Remote {
    /// 本机此刻把声音交给别的设备了。
    pub fn is_remote(&self) -> bool {
        lock(&self.inner.output).target().is_some()
    }

    /// 本机此刻正被别的设备遥控(锁定态)。
    ///
    /// 锁定期间本机上的播放动作一概不生效,直到用户按「退出被遥控」——
    /// 少了这道锁,pc1 前面的人随手按一下暂停,手机上的进度条就开始撒谎。
    pub fn is_controlled(&self) -> bool {
        lock(&self.inner.controlled_by).is_some()
    }

    /// 输出指着哪台设备。本机输出时是 `None`。
    ///
    /// 点播要拿它当**队列的归属** —— 队列归播放会话 / 输出设备,不是遥控器
    /// 自己这台,也不是账号(`docs/adr/0031` 二)。
    pub fn target_id(&self) -> Option<String> {
        lock(&self.inner.output).target().map(str::to_owned)
    }

    /// 记一次「点播交出去了」。
    ///
    /// 点播与别的命令不同:它要先经 HTTP 把队列发布出去,命令是那次往返之后
    /// 才发的。于是「这一下有没有走到远端分支」在测试里再也不能靠
    /// [`Self::sent_commands`] 观察 —— 那里要等一个测试环境里不存在的服务端。
    /// 这个计数器记的正是那件事,而且是**同步**记的。
    pub fn note_play_submitted(&self, track_id: &str) {
        *lock(&self.inner.pending_play) =
            Some((track_id.to_owned(), now_ms()));
        #[cfg(test)]
        self.inner
            .play_submits
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 那次点播没交到对面(发布失败、命令没发出去):下一下不该被当成多余。
    pub fn forget_pending_play(&self) {
        lock(&self.inner.pending_play).take();
    }

    /// 交出去不满 [`PENDING_PLAY_MS`] 的那次点播点的是哪一首。
    pub fn pending_play(&self) -> Option<String> {
        lock(&self.inner.pending_play)
            .as_ref()
            .filter(|(_, at)| {
                now_ms().saturating_sub(*at)
                    < PENDING_PLAY_MS
            })
            .map(|(id, _)| id.clone())
    }

    /// 测试里问:到此为止有几下点播走到了远端分支。
    #[cfg(test)]
    pub(crate) fn play_submits(&self) -> u64 {
        self.inner.play_submits.load(Ordering::Relaxed)
    }

    /// 把一条命令发给被控端。
    ///
    /// 返回**为什么**,不是一个布尔:调用方要据此说不同的话 —— 过期是「控制
    /// 暂不可用」,超限是「队列太长」,而输出在本机根本不该走这条路。
    pub fn send(&self, cmd: RemoteCommand) -> Submitted {
        let Some(target) = lock(&self.inner.output)
            .target()
            .map(str::to_owned)
        else {
            log::info!(
                "遥控提交: {} 未发出(输出在本机)",
                cmd.summary()
            );
            return Submitted::NotRemote;
        };

        // 判据仍走 rules 里那一条(它连「刚接管、快照还在路上要放行」
        // 一起管着,且在那里测得到)。这里只是**先**把「输出在本机」摘出去,
        // 好让两种拦法各说各的话 —— 目标已经确定存在,所以剩下唯一能让
        // 它为假的就是过期。
        if !accepts_control(
            &lock(&self.inner.output),
            &lock(&self.inner.view),
            now_ms(),
        ) {
            log::info!(
                "遥控提交: {} -> {target} 未发出(状态已过期)",
                cmd.summary()
            );
            return Submitted::Stale;
        }

        // **发之前**量,不发出去让它失败:超限的消息会让服务端读循环跳出,
        // 整条信令连接断掉,遥控器连自己的控制权都一起丢
        // (见 `contract::MAX_SIGNAL_BYTES`)。量的是连外层 `to` 一起的那一份,
        // 因为服务端数的就是那一份。
        let bytes =
            syncplay::command_wire_len(&target, &cmd);
        if bytes > app_core::MAX_SIGNAL_BYTES {
            log::warn!(
                "遥控提交: {} -> {target} 拒发({bytes} 字节 > 上限 {} 字节)",
                cmd.summary(),
                app_core::MAX_SIGNAL_BYTES
            );
            return Submitted::TooLarge {
                bytes,
                limit: app_core::MAX_SIGNAL_BYTES,
            };
        }

        let Some(client) = self.inner.client.get() else {
            log::info!(
                "遥控提交: {} -> {target} 未发出(客户端还没接上)",
                cmd.summary()
            );
            return Submitted::NoClient;
        };
        // 提交成功**只说明本地交出去了**:队列、服务端转发、被控端执行都还在
        // 后面,任何一跳都可能悄悄丢掉它。真放起来了以被控端的上报为准。
        log::info!(
            "遥控提交: {} -> {target} 已交给客户端({bytes} 字节)",
            cmd.summary()
        );
        #[cfg(test)]
        lock(&self.inner.sent).push(cmd.clone());
        client.command(cmd);
        Submitted::Ok
    }

    /// 测试里问:到此为止交出去了哪些命令。
    #[cfg(test)]
    pub(crate) fn sent_commands(
        &self,
    ) -> Vec<RemoteCommand> {
        lock(&self.inner.sent).clone()
    }

    /// 测试里塞一份**指定到达时刻**的上报。
    ///
    /// [`handle`] 那条路把 `now_ms()` 写进去,于是「一份已经过期的上报」在
    /// 测试里根本造不出来 —— 而过期恰恰是 `docs/adr/0030` 那条「禁用控制但
    /// 不切回本机」唯一生效的时刻。
    #[cfg(test)]
    pub(crate) fn accept_report_at(
        &self,
        state: RemoteStateDto,
        now_ms: u64,
    ) {
        lock(&self.inner.view).accept(state, now_ms);
    }

    /// 目标在别的设备、而这一下没提交出去时,界面该说的那句话。
    ///
    /// 文案要点出是哪台设备,所以它得读 `output` —— 那一份不出这个模块。
    pub fn unavailable_notice(&self) -> String {
        describe_unavailable(&lock(&self.inner.output))
    }

    /// 一条命令大到发不出去时,界面该说的那句话。
    pub fn too_large_notice(&self) -> String {
        describe_too_large(&lock(&self.inner.output))
    }

    /// 遥控器发来的下一条命令,没有则 `None`。
    pub fn take_command(&self) -> Option<RemoteCommand> {
        lock(&self.inner.inbox).pop_front()
    }

    /// 这一拍是不是刚从别的设备回到本机。**只答一次** —— 问过就落回平地。
    ///
    /// 遥控期间本机播放器是空的,而本机那台状态机还停在进遥控之前的
    /// `Playing`。回到本机的那一拍不把它按停,自动续播就会当成「这一首放完了」
    /// 而接上下一首 —— 用户什么也没点,歌却从 0:00 响起来(#102 之四)。
    pub fn took_local_edge(&self) -> bool {
        let now = self.is_remote();
        let was = core::mem::replace(
            &mut *lock(&self.inner.was_remote),
            now,
        );
        was && !now
    }

    /// 被控端失联太久就把输出收回本机。收回了返回 `true`。
    ///
    /// 撤权丢了就没有第二次(`server::syncplay::control` 的 `send` 是 `try_send`
    /// 且不重发),而本机这条 socket 好好的、不会重连,重连那条自愈也就走不到。
    /// 少了这一条,遥控器永久停在「遥控: 状态已过期」,芯片还亮在那台设备上,
    /// 本机也放不了歌 —— 用户唯一的出路是自己去点一下「本机」(#102 F-003)。
    ///
    /// 走的是与撤权**同一条**收尾:输出回本机、镜像清掉、提示一句。自动续播
    /// 那趟轮询下一步就会看到 [`Self::took_local_edge`],把本机按停,
    /// 所以这里不会顺手起播。
    pub fn give_up_if_lost(&self) -> bool {
        self.give_up_if_lost_at(now_ms())
    }

    /// 同上,但时钟由调用方给 —— 测试要在这条十五秒的判断上说话。
    pub(crate) fn give_up_if_lost_at(
        &self,
        now_ms: u64,
    ) -> bool {
        {
            let output = lock(&self.inner.output);
            if !lost_remote(
                &output,
                &lock(&self.inner.view),
                now_ms,
            ) {
                return false;
            }
        }

        // 持权记录一起交出去:留着的话,遥控器自己的信令哪天重连一次就拿它去
        // 续权,把被控端重新锁上 —— 而这头已经是本机输出了(#118)。
        self.release_claim();
        self.come_home(describe_lost);
        true
    }

    /// 输出收回本机:镜像清掉、提示一句。
    ///
    /// 文案由调用方按「收回之前指着的是哪台设备」算,所以收的是个函数 ——
    /// 下一行就把输出改回本机了。撤权、失联、接管失败三条路共用这一段。
    fn come_home(
        &self,
        describe: impl FnOnce(&Output) -> String,
    ) {
        let message = describe(&lock(&self.inner.output));
        *lock(&self.inner.output) = Output::Local;
        lock(&self.inner.view).clear();
        lock(&self.inner.cover_id).clear();
        lock(&self.inner.pending_play).take();
        let _ = self.inner.weak.upgrade_in_event_loop(
            move |ui| {
                crate::notice::show(&ui, message);
            },
        );
        self.refresh();
    }

    /// 让客户端忘掉本机的持权记录。不发信令 —— 被控端仍然该接着放。
    fn release_claim(&self) {
        #[cfg(test)]
        self.inner.releases.fetch_add(1, Ordering::Relaxed);
        if let Some(client) = self.inner.client.get() {
            client.release_control();
        }
    }

    /// 测试里问:到此为止交出去几次持权。
    #[cfg(test)]
    pub(crate) fn releases(&self) -> u64 {
        self.inner.releases.load(Ordering::Relaxed)
    }

    /// 这一拍该不该去取封面 —— 曲目 id 与上次取的那一首不同时才算数。
    ///
    /// 边沿触发,不是每拍触发:上报每秒一条,跟着它取图就是每秒一次下载。
    /// 问过就算数,所以它同时是「正在取的是哪一首」的那份记录。
    fn claim_cover(&self, track_id: &str) -> bool {
        let mut held = lock(&self.inner.cover_id);
        if *held == track_id {
            return false;
        }
        held.clear();
        held.push_str(track_id);
        true
    }

    /// 封面正在取的是不是这一首。连着切歌时先发的请求可能后回来。
    fn cover_is_current(&self, track_id: &str) -> bool {
        *lock(&self.inner.cover_id) == track_id
    }

    /// 这一条上报的顺序键:`(epoch, state_seq)`,序号取完即加一。
    ///
    /// 由报的这一端发号,而不是由凑快照的那一段:序号是**这条连接上报了
    /// 几次**,与快照里有什么无关。放到 `snapshot` 里的话,每加一个凑快照的
    /// 入口就多一个能把序号弄乱的地方。
    pub fn stamp(&self) -> (i64, u64) {
        (
            self.inner.epoch,
            self.inner
                .state_seq
                .fetch_add(1, Ordering::Relaxed),
        )
    }

    /// 被遥控时把本机状态报出去;没被遥控就什么也不做。
    ///
    /// 目标由服务端从控制权槽位查(见 `server::syncplay::control`),这里不指定发给谁。
    pub fn report(&self, state: RemoteStateDto) {
        // 量一下再决定发不发。**量的是每一条,不只是发出去的那些** ——
        // 这一行是 #109 F-002 那个洞唯一的哨兵:上报曾经拖着整个队列,
        // 977 首时 23 万字节,而超限的后果是整条连接断掉。现在它定长了,
        // 留着这行是为了哪天有人往小状态里塞回一个随用户数据增长的字段时,
        // 日志里先变的是它,而不是某台设备的连接开始莫名其妙地断。
        //
        // debug 级:每秒一条,info 会把日志淹掉。要读它就
        // `RUST_LOG=ui::sync::remote=debug`。
        let bytes = syncplay::report_wire_len(&state);
        log::debug!(
            "上报出栈: {bytes} 字节(队列 {} 首, 上限 {})",
            state.queue_len,
            app_core::MAX_SIGNAL_BYTES
        );

        if let Some(client) = self.inner.client.get()
            && self.is_controlled()
        {
            client.report(state);
        }
    }

    /// 遥控那一侧的曲目、队列与状态,交给调用方去画。
    pub fn with_view<T>(
        &self,
        read: impl FnOnce(&RemoteView, u64) -> T,
    ) -> T {
        read(&lock(&self.inner.view), now_ms())
    }

    /// 换输出设备。`id` 为空就是选回本机。
    ///
    /// 乐观更新:接管要一个来回,而按下去必须立刻有反应。真被拒了会有
    /// `ControlRevoked` 或一条失败提示把这一行改回去。
    pub fn select(&self, id: &str, name: &str) {
        let Some(client) = self.inner.client.get() else {
            return;
        };
        // 换目标前先把镜像清掉,否则下一台设备会先闪一眼上一台的歌名。
        lock(&self.inner.view).clear();
        lock(&self.inner.cover_id).clear();
        lock(&self.inner.pending_play).take();
        log::info!(
            "输出切到 {}",
            if id.is_empty() { "本机" } else { name }
        );
        if id.is_empty() {
            self.release_claim();
            *lock(&self.inner.output) = Output::Local;
        } else {
            client.claim(id);
            *lock(&self.inner.output) =
                Output::Remote(DeviceDto {
                    id: id.to_owned(),
                    name: name.to_owned(),
                });
        }
        self.refresh();
    }

    /// 被控端按了「退出被遥控」:解锁本机,并撤掉遥控器的控制权。
    ///
    /// 在途的取数由调用方作废(见 `music::bind_remote`):这一层碰不到
    /// `Deck`,而那份执行副本住在那边。
    pub fn exit_controlled(&self) {
        if let Some(client) = self.inner.client.get() {
            client.exit_controlled();
        }
        *lock(&self.inner.controlled_by) = None;
        self.refresh();
    }

    /// 把同播那个客户端交给它。只认第一次 —— 它是启动期的接线,不是状态。
    pub fn attach(&self, client: &Arc<Client>) {
        let _ = self.inner.client.set(client.clone());
    }

    /// 把输出设备与被遥控横幅这两行推到界面上。
    ///
    /// 进度与播放状态不走这里 —— 它们搭自动续播那趟每秒轮询的车,
    /// 免得出现第二套「现在放到哪」的说法(见 `crate::music`)。
    fn refresh(&self) {
        let id = lock(&self.inner.output)
            .target()
            .unwrap_or_default()
            .to_owned();
        let output =
            describe_output(&lock(&self.inner.output));
        let controlled = describe_controlled(
            lock(&self.inner.controlled_by)
                .as_ref()
                .map(|device| device.name.as_str()),
        );
        let _ = self.inner.weak.upgrade_in_event_loop(
            move |ui| {
                ui.global::<Shell>()
                    .set_output_text(output.into());
                ui.global::<Shell>()
                    .set_output_id(id.into());
                ui.global::<Shell>()
                    .set_controlled_text(controlled.into());
            },
        );
    }
}

/// 一个还没接上客户端的把手。
///
/// 分两步是因为事件回调要在 [`Client::start`] 之前就交出去,而客户端要等它
/// 返回 —— 先有这个,再 [`Remote::attach`]。
pub fn new(ui: &MainWindow) -> Remote {
    Remote {
        inner: Arc::new(Inner {
            client: OnceLock::new(),
            output: Mutex::new(Output::Local),
            view: Mutex::new(RemoteView::default()),
            controlled_by: Mutex::new(None),
            inbox: Mutex::new(VecDeque::new()),
            cover_id: Mutex::new(String::new()),
            was_remote: Mutex::new(false),
            epoch: now_ms() as i64,
            state_seq: AtomicU64::new(1),
            pending_play: Mutex::new(None),
            #[cfg(test)]
            play_submits: AtomicU64::new(0),
            #[cfg(test)]
            releases: AtomicU64::new(0),
            #[cfg(test)]
            sent: Mutex::new(Vec::new()),
            weak: ui.as_weak(),
        }),
    }
}

/// 把遥控接到界面上。
pub fn bind(ui: &MainWindow, remote: &Remote) {
    // 输出设备的选择。参数是设备 id,空串是本机;名字从名册那一份里查,
    // 免得界面上的写法与用户点过的那一行对不上。
    let selecting = remote.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_set_output(move |id| {
        let id = id.to_string();
        let name =
            weak.upgrade().map_or_else(String::new, |ui| {
                device_name(&ui, &id)
            });
        selecting.select(&id, &name);
    });

    let exiting = remote.clone();
    ui.global::<Shell>().on_exit_controlled(move || {
        exiting.exit_controlled();
    });

    remote.refresh();
}

/// 一个谁也不连的把手,给测试用。理由同 `syncplay::detached`。
#[cfg(test)]
pub(crate) fn detached(ui: &MainWindow) -> Remote {
    let remote = new(ui);
    remote.attach(&Arc::new(Client::detached()));
    remote
}

/// 名册里这台设备叫什么。查不到就用 id —— 总比一行空白强。
fn device_name(ui: &MainWindow, id: &str) -> String {
    use slint::Model as _;

    ui.global::<Shell>()
        .get_devices()
        .iter()
        .find(|row| row.id == id)
        .map_or_else(
            || id.to_owned(),
            |row| row.name.to_string(),
        )
}

/// 处理一条遥控事件。**在后台线程上**跑。
pub fn handle(event: &syncplay::Event, remote: &Remote) {
    let inner = &remote.inner;
    match event {
        // 接管成功。立刻要一次快照 —— 服务端不缓存状态(`docs/adr/0030`),
        // 「现在是什么样」只能问被控端本人,而不问就得干等一秒。
        syncplay::Event::ControlGranted {
            target, ..
        } => {
            log::info!("接管 {target} 成功");
            if let Some(client) = inner.client.get() {
                client.request_snapshot();
            }
        }
        // 失权:回到本机输出。不静默 —— 用户得知道自己手上这台不再管用了。
        // 被控端自己退出时 `by` 正是输出指着的那一台,文案因此要在收回之前算。
        syncplay::Event::ControlRevoked { by } => {
            log::info!("控制权被 {by} 收走,输出回本机");
            remote.come_home(|output| {
                describe_revoked(output, by)
            });
        }
        // 接管没成:按下去时输出已经乐观地切了过去,切回来(#118)。
        // 只认当前那一台 —— 失败的若是上一台,用户已经改选了别的。
        syncplay::Event::ClaimFailed { target, reason } => {
            if lock(&inner.output).target()
                != Some(target.as_str())
            {
                return;
            }
            log::warn!("接管 {target} 失败: {reason}");
            remote.come_home(describe_claim_failed);
        }
        syncplay::Event::ControlledBy { device } => {
            log::info!("本机被 {} 遥控", device.name);
            *lock(&inner.controlled_by) =
                Some(device.clone());
            remote.refresh();
        }
        // 服务端说没人在遥控本机:解锁、撤横幅。锁定态此前只有用户自己按
        // 「退出被遥控」才清,于是槽位一旦在本机不知情时没了(服务端重启、
        // 遥控关系被别处撤掉),这台就挂着假横幅、锁着本地播放,而横幅上
        // 那台设备早就不管它了(#102 F-004)。
        //
        // 信令断了也一样(#118):断着的时候服务端的消息过不来,锁留着的话
        // 本机在断网期间连歌都点不了。重连之后客户端自己退出被遥控,两端
        // 对得上账(见 `syncplay::client` 的 `serve`)。
        syncplay::Event::NotControlled
        | syncplay::Event::Disconnected => {
            if lock(&inner.controlled_by).take().is_some() {
                remote.refresh();
            }
        }
        // 命令进收件箱,再叫一声 UI 线程去执行(见模块头的两步)。
        syncplay::Event::Command { cmd } => {
            log::info!(
                "收到遥控命令: {} 进收件箱",
                cmd.summary()
            );
            lock(&inner.inbox).push_back(cmd.clone());
            let _ =
                inner.weak.upgrade_in_event_loop(|ui| {
                    ui.global::<Shell>()
                        .invoke_remote_command();
                });
        }
        // 只收当前那台设备报来的:换目标之后,上一台的残余还会飘几条过来。
        syncplay::Event::RemoteState { from, state } => {
            if lock(&inner.output).target()
                != Some(from.as_str())
            {
                return;
            }
            lock(&inner.view)
                .accept(state.clone(), now_ms());
        }
        // 遥控器要一次完整状态。立刻回 —— 那一份由音乐页凑,所以叫它一声。
        syncplay::Event::SnapshotRequest => {
            let _ =
                inner.weak.upgrade_in_event_loop(|ui| {
                    ui.global::<Shell>()
                        .invoke_remote_snapshot();
                });
        }
        _ => {}
    }
}

/// 遥控时把播放那几行改成被控端的状态。
///
/// 与本机路径共用同一批 Slint 属性:界面只认「输出设备」这一个抽象,
/// 不该为两种输出各画一套控制条(`docs/adr/0030`)。
pub fn push_playback(ui: &MainWindow, remote: &Remote) {
    let (text, playing, track, position, stale) = remote
        .with_view(|view, now| {
            (
                describe_remote(view, now),
                view.state()
                    == app_core::RemotePlayState::Playing,
                view.track().cloned(),
                view.position_ms(now),
                view.is_stale(now),
            )
        });

    ui.global::<Player>().set_playback_text(text.into());
    ui.global::<Player>().set_is_playing(playing);
    ui.global::<Shell>().set_output_stale(stale);
    // 「新版本待应用」与「状态已过期」是两件事,各占一位:过期说的是
    // **这份报告旧了**(连着三秒没来),待应用说的是**报告是新的,而它报的
    // 就是「我还没换上」**。混成一个的话,取数失败会被显示成掉线,
    // 而用户会去检查网络(`docs/adr/0031` 一)。
    ui.global::<Shell>().set_queue_pending(
        remote.with_view(|view, _| {
            view.has_pending_revision()
        }),
    );
    ui.global::<Player>().set_buffering(remote.with_view(
        |view, _| {
            view.state()
                == app_core::RemotePlayState::Buffering
        },
    ));

    let Some(track) = track else {
        ui.global::<Player>().set_has_track(false);
        return;
    };
    // 播放页那两行也跟着换,封面跟着走 —— 换歌那一拍取一次。
    // ponytail: 点云与极光不跟。它们要的是解码出来的裸像素,而遥控时播放页
    // 本来就没在渲染;要它们的话把 `decode` 的第二个返回值接上去即可。
    sync_cover(ui, remote, &track);
    ui.global::<crate::Viz>()
        .set_now_title(track.title.clone().into());
    ui.global::<crate::Viz>().set_now_artists(
        crate::music::join_artists(&track.artists).into(),
    );

    let seconds = position as f64 / 1_000.0;
    ui.global::<Player>().set_has_track(true);
    ui.global::<Player>()
        .set_now_id(track.id.clone().into());
    ui.global::<Player>().set_progress_ratio(
        crate::progress::ratio(seconds, track.duration_ms),
    );
    ui.global::<Player>().set_progress_text(
        crate::progress::progress_text(
            seconds,
            track.duration_ms,
        )
        .into(),
    );
}

/// 取锁。锁里只有赋值和克隆,中毒了就是别处出了大问题。
fn lock<T>(
    value: &Mutex<T>,
) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("遥控状态锁中毒")
}

/// 让控制条的封面跟着被控端的曲目走。
///
/// 只在换歌那一拍取一次:上报每秒一条,跟着它取图就是每秒一次下载
/// (边沿由 [`Remote::claim_cover`] 判)。旧封面立刻清掉 —— 新歌配旧图
/// 比空着更误导,与本机路径同一条规矩(见 `music::transport::play_current`)。
fn sync_cover(
    ui: &MainWindow,
    remote: &Remote,
    track: &app_core::TrackDto,
) {
    if !remote.claim_cover(&track.id) {
        return;
    }
    ui.global::<crate::Viz>()
        .set_cover_art(slint::Image::default());

    let Some(url) = track.cover.clone() else {
        return;
    };
    let id = track.id.clone();
    let remote = remote.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        // 取不到或解不出就留着空图:封面 CDN 会过期,失败是常态(见 cover.rs)。
        let Ok(bytes) = api::fetch_bytes(&url).await else {
            return;
        };
        let Some((image, _)) =
            crate::imagery::cover::decode(&bytes)
        else {
            return;
        };
        // 连着切歌时先发的请求可能后回来,那时它已经不是当前这首。
        if !remote.cover_is_current(&id) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            ui.global::<crate::Viz>().set_cover_art(image);
        }
    });
}
