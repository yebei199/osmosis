//! 遥控器模式的界面接线:输出设备、被控端的锁定态、命令与上报。
//!
//! 遥控器**不**出声、被控端自己拉直链自己播(`docs/adr/0030`)。信令连接、名册与
//! 本机身份在 [`crate::sync::link`],这里只管遥控自己的状态。
//!
//! 事件回调跑在信令自己的后台线程上。命令却必须在 UI 线程上执行 —— 它要碰
//! `Deck`,而 `Deck` 全是 `Rc`,不是 `Send`。所以走两步:后台线程把命令塞进
//! 收件箱,再叫一声 UI 线程;真正执行的那段挂在 `Shell.remote-command` 上,
//! 由音乐页去接(见 `crate::music`)。

mod rules;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use app_core::{
    Cue, Draft, Effect, Group, GroupPlanDto, GroupRole,
    OperationAckDto, Output, Plan, Refused, RemoteCommand,
    RemoteStateDto, RemoteView, Session, Verdict,
};
use slint::ComponentHandle;
use syncplay::{Client, DeviceDto};

pub(crate) use rules::{
    accepts_control, describe_claim_failed,
    describe_controlled, describe_copy_fault,
    describe_group, describe_lost, describe_master_lost,
    describe_media_fault, describe_missing_entry,
    describe_move, describe_output, describe_remote,
    describe_revoked, describe_too_large,
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
    /// 信令客户端还没接上(启动期那一微秒的空窗)。
    NoClient,
    /// 这一条太大,**发出去会撞掉整条连接**,所以在这里拒掉。
    TooLarge { bytes: usize, limit: usize },
}

/// 主端的心跳：计划没变时多久原样再发一次。跟随端三次收不到算主端失联
/// (`app_core::MASTER_SILENT_MS`)。
const HEARTBEAT_MS: u64 = 1_000;

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
    /// 信令客户端,由 `crate::sync::link` 建好交过来。
    ///
    /// `OnceLock` 而不是直接持有:事件回调要在 [`Client::start`] **之前**
    /// 就交出去,而客户端要等它返回才拿得到。这一微秒的空窗里到达的事件
    /// 发不出东西 —— 那没关系,每秒一次的上报下一拍就把状态补齐了。
    client: OnceLock<Arc<Client>>,
    /// 播放组会话:声音此刻从哪台设备出来,以及进行中的迁移(#137 ③)。
    ///
    /// 「输出」不再是一个随手改的标志:改它要走一次迁移,确认之前它不动。
    session: Mutex<Session>,
    /// 本机作为播放组成员的那一侧(#137 ⑤):组、最近那份共同计划、自己是不是主端。
    group: Mutex<Group>,
    /// 主端最近一次真的发出去的计划:任期、序号、发出的时刻 —— 心跳按它节流。
    published_plan: Mutex<Option<(u64, u64, u64)>>,
    /// 最近一次随计划发出去的播放次序(与它的任期)。没变就不再带。
    sent_order: Mutex<Option<(u64, Option<Vec<i64>>)>>,
    /// 组里各成员报上来的故障(取不到媒体、跳不到位置),按设备 id。报好了就划掉。
    faults: Mutex<HashMap<String, String>>,
    /// 各成员报上来的输出路由，按设备 id。蓝牙、有线的标「未校准」。
    routes:
        Mutex<HashMap<String, app_core::OutputRouteDto>>,
    /// 上一次推到界面上的组那一行 —— 变了才记一笔日志。
    group_text: Mutex<String>,
    /// 会话交回来、要在 UI 线程上做的本机那一步(准备 / 停止 / 开始 / 取消)。
    ///
    /// 走收件箱而不是当场做,理由同 [`Self::inbox`]:回话可能在信令的后台线程上
    /// 到,而本机那一步要碰 `Deck`。
    local_effects: Mutex<VecDeque<Effect>>,
    /// 上一次发给远端的那一批:发给谁、是哪些歌、服务端给的那一版。
    ///
    /// 同一台同一批再点一首,原样用这一版,不再发一个一模一样的新版本(#137 ③)。
    published: Mutex<Option<Published>>,
    /// 上一次失败说明已经说过的那一句 —— 同一句只弹一次。
    told: Mutex<Option<String>>,
    /// 被控端报来的那份状态 —— 本机作遥控器时才有东西。
    view: Mutex<RemoteView>,
    /// 正在遥控本机的那台设备 —— 本机作被控端时才有东西。
    controlled_by: Mutex<Option<DeviceDto>>,
    /// 遥控器发来、还没执行的命令。
    inbox: Mutex<VecDeque<RemoteCommand>>,
    /// 封面已经取到哪一首了 —— 上报每秒一条,按它的频率取图等于每秒一次下载。
    cover_id: Mutex<String>,
    /// 输出刚被收回本机(失权、失联、接管失败),本机播放还没按停。
    ///
    /// 只由收回那一条路置位:迁移回本机时本机是**被叫去开始**的,按停它
    /// 就等于把刚迁过来的那一首掐掉。
    rest_pending: std::sync::atomic::AtomicBool,
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
    /// 测试里记下迁移发给了哪台设备什么命令、对服务端做了哪几样组操作。
    ///
    /// 理由同 `sent`:空壳客户端当场丢掉通道,不记下来就看不到迁移走到了哪一步。
    #[cfg(test)]
    routed: Mutex<Vec<(String, RemoteCommand)>>,
    #[cfg(test)]
    group_ops: Mutex<Vec<String>>,
    weak: slint::Weak<MainWindow>,
}

impl Remote {
    /// 本机此刻把声音交给别的设备了。
    pub fn is_remote(&self) -> bool {
        lock(&self.inner.session)
            .output()
            .target()
            .is_some()
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
        lock(&self.inner.session)
            .output()
            .target()
            .map(str::to_owned)
    }

    /// 正在迁移:输出还没定下来。
    pub fn is_moving(&self) -> bool {
        lock(&self.inner.session).moving().is_some()
    }

    /// 控制命令该不该先压着:新主端还没确认开始、或者主端正在交接(#137 ⑤)。
    /// 组里有留下的主端、只是加人减人时不压。
    pub fn holds_transport(&self) -> bool {
        lock(&self.inner.session).holds_transport()
    }

    /// 已经确认的成员。只有本机时是 `[Local]`,一台都没有时是空的。
    pub fn members(&self) -> Vec<Output> {
        lock(&self.inner.session).members().to_vec()
    }

    /// 成员的设备 id,本机是空串 —— 名册那一排芯片按它标出谁在组里。
    pub fn member_ids(&self) -> Vec<String> {
        lock(&self.inner.session)
            .members()
            .iter()
            .map(|output| {
                output
                    .target()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    // ── 本机作为播放组成员(#137 ⑤)──

    /// 本机在组里的身份。
    pub fn group_role(&self) -> GroupRole {
        lock(&self.inner.group).role()
    }

    /// 本机开始跟着组放了(被叫「开始」的那一刻)。
    pub fn group_join(&self) {
        lock(&self.inner.group).join();
    }

    /// 手上最新的共同计划。
    pub fn group_plan(&self) -> Option<GroupPlanDto> {
        lock(&self.inner.group).plan().cloned()
    }

    /// 此刻该怎么出声。校时还没有结论时，服务端时刻算不出来，按「还在等」处理。
    pub fn group_verdict(&self) -> Verdict {
        let group = lock(&self.inner.group);
        match self.server_now_us() {
            Some(now_us) => group.verdict(now_us, now_ms()),
            None if group.role() == GroupRole::Solo => {
                Verdict::Solo
            }
            None => Verdict::Waiting,
        }
    }

    /// 主端写一份计划，内容变了或者心跳到点了就发出去。
    pub fn publish_group(&self, draft: Draft, cue: Cue) {
        let Some(now_us) = self.server_now_us() else {
            return;
        };
        let now = now_ms();
        let Some((term, plan)) = lock(&self.inner.group)
            .publish(draft, cue, now_us, now)
        else {
            return;
        };
        let mut sent = lock(&self.inner.published_plan);
        let due = match *sent {
            Some((held_term, seq, at)) => {
                held_term != term
                    || seq != plan.seq
                    || now.saturating_sub(at)
                        >= HEARTBEAT_MS
            }
            None => true,
        };
        if !due {
            return;
        }
        *sent = Some((term, plan.seq, now));
        drop(sent);
        // 次序只在变了(或换了任期)时带上;跟随端没收到就沿用手上那份。
        let mut plan = plan;
        {
            let mut order = lock(&self.inner.sent_order);
            let fresh = (term, plan.play_order.clone());
            if order.as_ref() == Some(&fresh) {
                plan.play_order = None;
            } else {
                *order = Some(fresh);
            }
        }
        if let Some(client) = self.inner.client.get() {
            client.publish_plan(term, plan);
        }
    }

    /// 服务端时钟此刻的读数(微秒),按校时换算。还没校过时是 `None`。
    pub fn server_now_us(&self) -> Option<u64> {
        self.to_server_us(audio::clock::monotonic_ns())
    }

    /// 本机单调时刻换算成服务端时钟(微秒)。
    pub fn to_server_us(
        &self,
        local_ns: i64,
    ) -> Option<u64> {
        let client = self.inner.client.get()?;
        let clock = client.clock();
        let clock = clock.lock().ok()?;
        clock.to_server_us(local_ns)
    }

    /// 服务端纪元 `epoch` 上的时刻换算成本机单调时钟(纳秒)。纪元对不上就是 `None`。
    pub fn to_local_ns(
        &self,
        epoch: u64,
        server_us: u64,
    ) -> Option<i64> {
        let client = self.inner.client.get()?;
        let clock = client.clock();
        let clock = clock.lock().ok()?;
        clock.to_local_ns(epoch, server_us)
    }

    /// 校时所在的纪元(服务端这一次启动)。主端把它写进计划。
    pub fn clock_epoch(&self) -> Option<u64> {
        let client = self.inner.client.get()?;
        let clock = client.clock();
        let clock = clock.lock().ok()?;
        clock.epoch()
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
        let Some(target) = lock(&self.inner.session)
            .output()
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
            lock(&self.inner.session).output(),
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
        describe_unavailable(
            lock(&self.inner.session).output(),
        )
    }

    /// 一条命令大到发不出去时,界面该说的那句话。
    pub fn too_large_notice(&self) -> String {
        describe_too_large(
            lock(&self.inner.session).output(),
        )
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
        self.inner
            .rest_pending
            .swap(false, Ordering::Relaxed)
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
            let session = lock(&self.inner.session);
            // 迁移进行中由迁移自己的超时与「待确认」管(#137 ③)。源被冻住或断网时
            // 这里本来会按失联收回本机,把「待确认」与处理入口一并丢掉 —— 而那正是
            // 该交给用户处理的时候。
            if session.moving().is_some() {
                return false;
            }
            if !lost_remote(
                session.output(),
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
        let message =
            describe(lock(&self.inner.session).output());
        let abandoned = lock(&self.inner.session)
            .moving()
            .map(|moving| moving.operation_id.clone());
        lock(&self.inner.session).come_home();
        // 收回本机时若正在迁移,把服务端那一次也作罢:不然被拉进来的那台一直锁着。
        // 已经失权的话服务端会拒掉这一条,无害。
        if let (Some(operation_id), Some(client)) =
            (abandoned, self.inner.client.get())
        {
            client.abort_outputs(&operation_id);
        }
        self.inner
            .rest_pending
            .store(true, Ordering::Relaxed);
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

    /// 把当前播放迁到 `to`(#137 ③)。`plan` 是要迁过去的那一份,什么都没在放
    /// 时是 `None`。
    ///
    /// 不再是乐观地改一个标志:输出要等目标确认开始之后才换过去,这几秒里
    /// 控制条照旧显示迁过去的那一首,状态行说「正在切到 xx」。
    pub fn begin_move(
        &self,
        to: Output,
        plan: Option<Plan>,
    ) {
        self.change_outputs(vec![to], plan);
    }

    /// 把成员集合换成 `set`(#137 ⑤):改在这些设备播放、加入一台、移出一台都走这里。
    pub fn change_outputs(
        &self,
        set: Vec<Output>,
        plan: Option<Plan>,
    ) {
        let operation_id =
            crate::sync::link::fresh_operation_id();
        log::info!(
            "换输出开始: 操作 {operation_id} -> [{}]{}",
            set.iter()
                .map(|output| output
                    .name()
                    .unwrap_or("本机"))
                .collect::<Vec<_>>()
                .join(", "),
            plan.as_ref()
                .map(|plan| format!(
                    "(队列 {}@{}, 条目 {}, {}ms)",
                    plan.queue_id,
                    plan.revision,
                    plan.entry_id,
                    plan.position_ms
                ))
                .unwrap_or_else(
                    || "(没有在放的,只停源)".to_owned()
                )
        );
        let outcome = lock(&self.inner.session).change(
            operation_id,
            set,
            plan,
            now_ms(),
        );
        match outcome {
            Ok(effects) => self.apply(effects),
            Err(Refused::AlreadyThere) => {}
            Err(Refused::Busy) => {
                let _ = self
                    .inner
                    .weak
                    .upgrade_in_event_loop(|ui| {
                        crate::notice::show(
                            &ui,
                            "上一次切换还没确认完"
                                .to_owned(),
                        );
                    });
            }
        }
    }

    /// 本机那一步做完了的回话,交还会话。
    pub fn local_ack(&self, ack: OperationAckDto) {
        let effects = lock(&self.inner.session).on_ack(
            &Output::Local,
            &ack,
            now_ms(),
        );
        self.apply(effects);
    }

    /// 每秒一拍:迁移等过了头没有。
    pub fn tick(&self) {
        let effects =
            lock(&self.inner.session).tick(now_ms());
        self.apply(effects);
    }

    /// 「待确认」上的重试。
    pub fn retry(&self) {
        let effects =
            lock(&self.inner.session).retry(now_ms());
        self.apply(effects);
    }

    /// 「待确认」上的放弃。
    pub fn abandon(&self) {
        let effects = lock(&self.inner.session).abandon();
        self.apply(effects);
    }

    /// 会话交回来的本机那一步,UI 线程上取。
    pub fn take_local_effect(&self) -> Option<Effect> {
        lock(&self.inner.local_effects).pop_front()
    }

    /// 迁移那几秒控制条上画什么:迁过去的那一首、状态行那句话、要不要出
    /// 「待确认」那两颗键。没在迁移时是 `None`。
    pub fn moving_view(
        &self,
    ) -> Option<(
        Option<app_core::TrackDto>,
        u64,
        String,
        bool,
    )> {
        let session = lock(&self.inner.session);
        let moving = session.moving()?;
        Some((
            moving
                .plan
                .as_ref()
                .map(|plan| plan.track.clone()),
            moving.anchor_ms().unwrap_or(0),
            describe_move(moving),
            matches!(
                moving.phase,
                app_core::Phase::Unconfirmed(_)
            ),
        ))
    }

    /// 执行会话交回来的一串动作:发给服务端的、发给远端设备的,当场发;本机那一步
    /// 进收件箱交给 UI 线程。
    fn apply(&self, effects: Vec<Effect>) {
        let client = self.inner.client.get();
        let mut local = false;
        for effect in effects {
            log::info!("迁移动作: {effect:?}");
            #[cfg(test)]
            self.record(&effect);
            match (effect, client) {
                (
                    Effect::Begin {
                        operation_id,
                        outputs,
                        master,
                    },
                    Some(client),
                ) => client.begin_outputs(
                    &operation_id,
                    outputs,
                    master,
                ),
                (
                    Effect::Commit {
                        operation_id,
                        outputs,
                    },
                    Some(client),
                ) => {
                    client.commit_outputs(
                        &operation_id,
                        outputs,
                    );
                    self.settle();
                }
                (
                    Effect::Abort { operation_id },
                    Some(client),
                ) => {
                    client.abort_outputs(&operation_id);
                }
                (effect, client) => {
                    match remote_command(&effect) {
                        Some((to, cmd)) => {
                            if let Some(client) = client {
                                client.command_to(&to, cmd);
                            }
                        }
                        None => {
                            lock(&self.inner.local_effects)
                                .push_back(effect);
                            local = true;
                        }
                    }
                }
            }
        }
        if local {
            let _ = self.inner.weak.upgrade_in_event_loop(
                |ui| {
                    ui.global::<Shell>()
                        .invoke_session_effects();
                },
            );
        }
        self.tell_failure();
        self.refresh();
    }

    #[cfg(test)]
    fn record(&self, effect: &Effect) {
        match effect {
            Effect::Begin {
                operation_id,
                outputs,
                ..
            } => lock(&self.inner.group_ops).push(format!(
                "begin {operation_id} {outputs:?}"
            )),
            Effect::Commit { operation_id, .. } => {
                lock(&self.inner.group_ops)
                    .push(format!("commit {operation_id}"));
            }
            Effect::Abort { operation_id } => {
                lock(&self.inner.group_ops)
                    .push(format!("abort {operation_id}"));
            }
            other => {
                if let Some(routed) = remote_command(other)
                {
                    lock(&self.inner.routed).push(routed);
                }
            }
        }
    }

    /// 测试里问:迁移发给了哪台设备什么命令。
    #[cfg(test)]
    pub(crate) fn routed(
        &self,
    ) -> Vec<(String, RemoteCommand)> {
        lock(&self.inner.routed).clone()
    }

    /// 测试里问:对服务端做了哪几样组操作。
    #[cfg(test)]
    pub(crate) fn group_ops(&self) -> Vec<String> {
        lock(&self.inner.group_ops).clone()
    }

    /// 迁移提交了:输出已经换过去,镜像与封面清掉(那是上一台的)。
    ///
    /// 不另要快照:新输出此刻已经被锁上、每秒都在报;而持权记录要等服务端回了
    /// 提交才换到新那台,这时候要的快照会发给刚被换下来的那台、换回一句
    /// `not_controller`。
    fn settle(&self) {
        lock(&self.inner.view).clear();
        lock(&self.inner.cover_id).clear();
        lock(&self.inner.pending_play).take();
    }

    /// 迁移没成的那句话,说一次。
    fn tell_failure(&self) {
        let failure = lock(&self.inner.session)
            .failure()
            .map(str::to_owned);
        let mut told = lock(&self.inner.told);
        if failure == *told {
            return;
        }
        told.clone_from(&failure);
        if let Some(message) = failure {
            let _ = self.inner.weak.upgrade_in_event_loop(
                move |ui| crate::notice::show(&ui, message),
            );
        }
    }

    /// 测试里直接把输出放到这台设备上,当作一次已经确认过的迁移。
    ///
    /// 走的是会话自己的路(没有东西可迁的那种:停源、确认),不另开后门。
    #[cfg(test)]
    pub(crate) fn assume_output(
        &self,
        id: &str,
        name: &str,
    ) {
        {
            let mut session = lock(&self.inner.session);
            let from = session.output().clone();
            let _ = session.begin(
                "test-assume".to_owned(),
                Output::Remote(DeviceDto {
                    id: id.to_owned(),
                    name: name.to_owned(),
                }),
                None,
                0,
            );
            let _ = session.on_ack(
                &from,
                &OperationAckDto {
                    operation_id: "test-assume".to_owned(),
                    phase:
                        app_core::OperationPhase::Stopped,
                    position_ms: None,
                    reason: None,
                },
                0,
            );
        }
        lock(&self.inner.view).clear();
        self.refresh();
    }

    /// 同一台、同一批上一次发布的那一版。
    pub fn published_for(
        &self,
        target: &str,
        tracks: &[app_core::TrackDto],
    ) -> Option<api::QueueRefDto> {
        let held = lock(&self.inner.published);
        let held = held.as_ref()?;
        (held.target == target
            && held.keys == keys_of(tracks))
        .then(|| held.queue.clone())
    }

    /// 记下这一次发布,下一次同一台同一批直接用。
    pub fn note_published(
        &self,
        target: &str,
        tracks: &[app_core::TrackDto],
        queue: api::QueueRefDto,
    ) {
        *lock(&self.inner.published) = Some(Published {
            target: target.to_owned(),
            keys: keys_of(tracks),
            queue,
        });
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
        // 退出被遥控也就退出了播放组:服务端那边把本机从组里摘掉,本机不再跟着谁放。
        lock(&self.inner.group).leave();
        self.group_changed();
        self.refresh();
    }

    /// 把信令客户端交给它。只认第一次 —— 它是启动期的接线,不是状态。
    pub fn attach(&self, client: &Arc<Client>) {
        let _ = self.inner.client.set(client.clone());
    }

    /// 组或计划变了：叫 UI 线程按新的样子重新对准本机播放(见 `music::playback::group`)。
    fn group_changed(&self) {
        let _ =
            self.inner.weak.upgrade_in_event_loop(|ui| {
                ui.global::<Shell>().invoke_group_changed();
            });
    }

    /// 把输出设备与被遥控横幅这两行推到界面上。
    ///
    /// 进度与播放状态不走这里 —— 它们搭自动续播那趟每秒轮询的车,
    /// 免得出现第二套「现在放到哪」的说法(见 `crate::music`)。
    fn refresh(&self) {
        let id = lock(&self.inner.session)
            .output()
            .target()
            .unwrap_or_default()
            .to_owned();
        let output = describe_output(
            lock(&self.inner.session).output(),
        );
        let controlled = describe_controlled(
            lock(&self.inner.controlled_by)
                .as_ref()
                .map(|device| device.name.as_str()),
        );
        let (move_text, move_doubt) =
            self.moving_view().map_or(
                (String::new(), false),
                |(_, _, text, doubt)| (text, doubt),
            );
        let (group_text, member_ids) = {
            let session = lock(&self.inner.session);
            let faults = lock(&self.inner.faults);
            let mut routes =
                lock(&self.inner.routes).clone();
            // 本机也在组里时，本机的路由自己查(本机不给自己上报)。
            if let Some(route) = audio::route() {
                routes.insert(
                    String::new(),
                    route_dto(route),
                );
            }
            (
                describe_group(&session, &faults, &routes),
                session
                    .members()
                    .iter()
                    .map(|output| {
                        output
                            .target()
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect::<Vec<_>>(),
            )
        };
        // 组那一行变了记一笔:谁在组里、哪台待确认、哪台报了故障,事后查得到是哪一刻变的。
        {
            let mut told = lock(&self.inner.group_text);
            if *told != group_text {
                log::info!("组那一行: {group_text}");
                told.clone_from(&group_text);
            }
        }
        let _ = self.inner.weak.upgrade_in_event_loop(
            move |ui| {
                ui.global::<Shell>()
                    .set_move_text(move_text.into());
                ui.global::<Shell>()
                    .set_move_doubt(move_doubt);
                ui.global::<Shell>()
                    .set_output_text(output.into());
                ui.global::<Shell>()
                    .set_output_id(id.into());
                ui.global::<Shell>()
                    .set_controlled_text(controlled.into());
                ui.global::<Shell>()
                    .set_group_text(group_text.into());
                mark_members(&ui, &member_ids);
            },
        );
    }
}

/// 名册那一排芯片上标出谁在组里(空串是本机)。「加入 / 移出」那颗小键照它显示。
pub(crate) fn mark_members(
    ui: &MainWindow,
    member_ids: &[String],
) {
    use slint::Model as _;

    let rows = ui.global::<Shell>().get_devices();
    for index in 0..rows.row_count() {
        let Some(mut row) = rows.row_data(index) else {
            continue;
        };
        let member = member_ids
            .iter()
            .any(|id| *id == row.id.as_str());
        if row.member != member {
            row.member = member;
            rows.set_row_data(index, row);
        }
    }
    ui.global::<Shell>().set_local_member(
        member_ids.iter().any(String::is_empty),
    );
}

/// 一个还没接上客户端的把手。
///
/// 分两步是因为事件回调要在 [`Client::start`] 之前就交出去,而客户端要等它
/// 返回 —— 先有这个,再 [`Remote::attach`]。
pub fn new(ui: &MainWindow, me: &str) -> Remote {
    Remote {
        inner: Arc::new(Inner {
            client: OnceLock::new(),
            session: Mutex::new(Session::with_me(me)),
            group: Mutex::new(Group::new(me)),
            published_plan: Mutex::new(None),
            sent_order: Mutex::new(None),
            faults: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            group_text: Mutex::new(String::new()),
            local_effects: Mutex::new(VecDeque::new()),
            published: Mutex::new(None),
            told: Mutex::new(None),
            view: Mutex::new(RemoteView::default()),
            controlled_by: Mutex::new(None),
            inbox: Mutex::new(VecDeque::new()),
            cover_id: Mutex::new(String::new()),
            rest_pending:
                std::sync::atomic::AtomicBool::new(false),
            epoch: now_ms() as i64,
            state_seq: AtomicU64::new(1),
            pending_play: Mutex::new(None),
            #[cfg(test)]
            play_submits: AtomicU64::new(0),
            #[cfg(test)]
            releases: AtomicU64::new(0),
            #[cfg(test)]
            sent: Mutex::new(Vec::new()),
            #[cfg(test)]
            routed: Mutex::new(Vec::new()),
            #[cfg(test)]
            group_ops: Mutex::new(Vec::new()),
            weak: ui.as_weak(),
        }),
    }
}

/// 把遥控接到界面上。
pub fn bind(ui: &MainWindow, remote: &Remote) {
    // 输出设备的选择接在音乐页(`music::playback::migrate`):选设备等于迁移
    // 当前播放,要从本机的播放里凑出迁过去的那一份,而那一份住在 `Deck` 里。

    let exiting = remote.clone();
    ui.global::<Shell>().on_exit_controlled(move || {
        exiting.exit_controlled();
    });

    remote.refresh();
}

/// 一个谁也不连的把手,给测试用。
///
/// 真的 [`crate::sync::link::bind`] 会当场把客户端连去 `api::base_url()` 的信令地址,
/// 而那个地址是**编译期**决定的:不设 `OSMOSIS_API_BASE` 时是本机 3000 —— 开发服务器
/// 正好在那儿,测试于是会因为本机有没有开 server-dev 而表现不同;设成集群地址跑一次
/// `cargo test`,那就是拿生产环境当测试靶子。
#[cfg(test)]
pub(crate) fn detached(ui: &MainWindow) -> Remote {
    let remote = new(ui, "me");
    remote.attach(&Arc::new(Client::detached()));
    remote
}

/// 名册里这台设备叫什么。查不到就用 id —— 总比一行空白强。
pub(crate) fn device_name(
    ui: &MainWindow,
    id: &str,
) -> String {
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
            if lock(&inner.session).output().target()
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
        // 撤锁**不是**离组(#137 ⑤):遥控器满租约时服务端同样撤锁,而那时组不散、主端照常
        // 发计划。离组看组的通告里还有没有本机(被移出时服务端也通告一份),或者本机自己按了
        // 「退出被遥控」。断线也不离组:断着的时候计划过不来,跟随端照已确认的放到有效期末尾。
        syncplay::Event::NotControlled
        | syncplay::Event::Disconnected => {
            if lock(&inner.controlled_by).take().is_some() {
                remote.refresh();
            }
        }
        syncplay::Event::Group {
            term,
            master,
            members,
        } => {
            log::info!(
                "播放组: 任期 {term}, 主端 {master:?}, 成员 {members:?}"
            );
            lock(&inner.group).on_group(
                *term,
                master.clone(),
                members.clone(),
                now_ms(),
            );
            // 组变了(有新来的、换了任期):主端下一份整份再发，次序也带上 —— 新来的手上还没有。
            *lock(&inner.published_plan) = None;
            *lock(&inner.sent_order) = None;
            remote.group_changed();
            remote.refresh();
        }
        syncplay::Event::GroupPlan { from, term, plan } => {
            let fresh = lock(&inner.group).on_plan(
                *term,
                (**plan).clone(),
                now_ms(),
            );
            if fresh {
                log::info!(
                    "共同计划: {from} 任期 {term} 第 {} 份(条目 {}, {}µs@{}µs, {})",
                    plan.seq,
                    plan.entry_id,
                    plan.position_us,
                    plan.anchor_us,
                    if plan.playing {
                        "播放"
                    } else {
                        "暂停"
                    }
                );
                remote.group_changed();
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
        //
        // 迁移那几秒里,源与目标报来的都要看:它们的回话(`operation`)是迁移
        // 往前走的唯一依据。镜像仍只收已确认的那一台 —— 目标确认之前,控制条
        // 上画的是迁过去的那一首,不是目标手上原来那一首。
        syncplay::Event::RemoteState { from, state } => {
            let fault_changed = {
                let mut faults = lock(&inner.faults);
                match &state.fault {
                    Some(why) => {
                        faults
                            .insert(
                                from.clone(),
                                why.clone(),
                            )
                            .as_ref()
                            != Some(why)
                    }
                    None => faults.remove(from).is_some(),
                }
            };
            let route_changed = {
                let mut routes = lock(&inner.routes);
                match state.route {
                    Some(route) => {
                        routes.insert(from.clone(), route)
                            != Some(route)
                    }
                    None => routes.remove(from).is_some(),
                }
            };
            if fault_changed || route_changed {
                remote.refresh();
            }
            if let Some(ack) = &state.operation {
                // 提交之后才到的回话也交给会话:逐台「待确认」靠它划掉。
                let party =
                    lock(&inner.session).output_of(from);
                if let Some(party) = party {
                    let effects = lock(&inner.session)
                        .on_ack(&party, ack, now_ms());
                    remote.apply(effects);
                }
            }
            if lock(&inner.session).output().target()
                != Some(from.as_str())
            {
                return;
            }
            lock(&inner.view)
                .accept((**state).clone(), now_ms());
        }
        syncplay::Event::OutputsCommitted {
            operation_id,
            term,
        } => {
            log::info!(
                "换输出提交: 操作 {operation_id}, 任期 {term}"
            );
            lock(&inner.session)
                .committed(operation_id, *term);
        }
        syncplay::Event::OutputsFailed {
            operation_id,
            reason,
        } => {
            log::warn!(
                "换输出没登记上: 操作 {operation_id}: {reason}"
            );
            lock(&inner.session)
                .rejected(operation_id, reason);
            remote.tell_failure();
            remote.refresh();
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

/// 只推进度那两样(比例与读数),按被控端最近那份上报推算此刻的位置。
///
/// 进度的快档那一趟用它(#137 ⑥):上报每秒一次,两次之间靠
/// `RemoteView::position_ms` 按本地时钟推 —— 只在 Playing 且上报新鲜时往前走。
pub fn push_progress(ui: &MainWindow, remote: &Remote) {
    let Some((track, position)) =
        remote.with_view(|view, now| {
            Some((
                view.track()?.clone(),
                view.position_ms(now),
            ))
        })
    else {
        return;
    };
    let seconds = position as f64 / 1_000.0;
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
    // 本来就没在渲染;要它们的话把 `decode_off_thread` 给的 `pixels` 与 `colors` 接上去即可。
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

/// 上一次发给远端的那一批。
struct Published {
    target: String,
    /// 每一首的 `(平台, 曲目 id)`,按顺序 —— 比对「是不是同一批」用。
    keys: Vec<(String, String)>,
    queue: api::QueueRefDto,
}

fn keys_of(
    tracks: &[app_core::TrackDto],
) -> Vec<(String, String)> {
    tracks
        .iter()
        .map(|track| {
            (track.platform.clone(), track.id.clone())
        })
        .collect()
}

/// 会话的一个动作若是发给远端设备的,翻成那条命令;本机那一步与服务端那三样
/// 返回 `None`。
fn remote_command(
    effect: &Effect,
) -> Option<(String, RemoteCommand)> {
    let (end, cmd) = match effect {
        Effect::Prepare {
            operation_id,
            to,
            plan,
        } => (
            to,
            RemoteCommand::Prepare {
                operation_id: operation_id.clone(),
                queue_id: plan.queue_id,
                revision: plan.revision,
                entry_id: plan.entry_id,
                position_ms: plan.position_ms,
            },
        ),
        Effect::Stop { operation_id, from } => (
            from,
            RemoteCommand::Stop {
                operation_id: operation_id.clone(),
            },
        ),
        Effect::Start {
            operation_id,
            to,
            position_ms,
            playing,
        } => (
            to,
            RemoteCommand::Start {
                operation_id: operation_id.clone(),
                position_ms: *position_ms,
                playing: *playing,
            },
        ),
        Effect::Cancel { operation_id, to } => (
            to,
            RemoteCommand::Cancel {
                operation_id: operation_id.clone(),
            },
        ),
        Effect::Begin { .. }
        | Effect::Commit { .. }
        | Effect::Abort { .. } => return None,
    };
    end.target().map(|id| (id.to_owned(), cmd))
}

/// 迁移那几秒把控制条画成迁过去的那一首(#137 ③)。
///
/// 控制条**不消失**:从前选完设备镜像一清,下一拍 `has-track` 就被置假,
/// 控制条连同抽屉里刚点的设备芯片一起销毁,要等对面整条链走完才回来。现在
/// 迁移期间画的是会话里那份计划 —— 用户点下去那一刻听的是哪首,这几秒就一直是
/// 哪首,进度停在锚点上,状态行说在等谁。
pub fn push_moving(ui: &MainWindow, remote: &Remote) {
    let Some((track, anchor_ms, text, _)) =
        remote.moving_view()
    else {
        return;
    };
    ui.global::<Player>().set_playback_text(text.into());
    ui.global::<Player>().set_is_playing(false);
    ui.global::<Player>().set_buffering(true);
    let Some(track) = track else {
        // 没有东西可迁(源本来就没在放):控制条本来就不在,不必凭空变出来。
        return;
    };
    sync_cover(ui, remote, &track);
    ui.global::<crate::Viz>()
        .set_now_title(track.title.clone().into());
    ui.global::<crate::Viz>().set_now_artists(
        crate::music::join_artists(&track.artists).into(),
    );
    let seconds = anchor_ms as f64 / 1_000.0;
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
        // 解码在后台线程上;排队轮到时已经切走就不解(#137 ⑥)
        let wanted = {
            let (remote, id) = (remote.clone(), id.clone());
            move || remote.cover_is_current(&id)
        };
        let Some(decoded) =
            crate::imagery::cover::decode_off_thread(
                bytes, wanted,
            )
            .await
        else {
            return;
        };
        let image = slint::Image::from_rgba8(decoded.full);
        // 连着切歌时先发的请求可能后回来,那时它已经不是当前这首。
        if !remote.cover_is_current(&id) {
            return;
        }
        if let Some(ui) = weak.upgrade() {
            ui.global::<crate::Viz>().set_cover_art(image);
        }
    });
}

/// 音频层查到的输出路由换成线上格式(#137 ⑤)。
pub(crate) fn route_dto(
    route: audio::Route,
) -> app_core::OutputRouteDto {
    match route {
        audio::Route::Speaker => {
            app_core::OutputRouteDto::Speaker
        }
        audio::Route::Bluetooth => {
            app_core::OutputRouteDto::Bluetooth
        }
        audio::Route::Wired => {
            app_core::OutputRouteDto::Wired
        }
    }
}
