//! 遥控器模式的控制权:每个账号一个槽位,以及照着它转发命令与上报。
//!
//! **服务端仍然不解释命令**(`docs/adr/0030`)。它只做两件事:记住"谁在遥控谁",
//! 以及照着这条记录把消息转到对的那一端。`RemoteCommand` 与 `RemoteStateDto`
//! 从这里原样穿过去,服务端不读它们。
//!
//! 单独成模块而不是塞进 [`crate::syncplay::signaling`]:那个文件已经五百多行,而"谁能
//! 控制谁"是纯逻辑 —— 恰恰也是会出错的地方,值得离开 WebSocket 被测。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use contract::{ClientSignal, ServerSignal};

use crate::syncplay::roster::Roster;
use crate::syncplay::signaling::{AccountId, Sink};

/// 一次控制权的代次。
///
/// 全局递增,不按账号分 —— 它只需要互不相同。断线的遥控器重连时带着手上
/// 这个数回来,服务端据此认得出它早就被顶替了(见 [`Control::claim`])。
pub type Generation = u64;

/// 遥控器下线之后,槽位替它留多久(#111,用户 2026-09-23 定为 30 秒)。
///
/// 要盖住的是「断了又带代次续上」那一段。遥控器连着 15 秒收不到上报就自己回本机、
/// 交出持权(`app_core::output` 的 `LOST_AFTER_MS`),之后的重连不再续,所以值得
/// 等的续上都落在断线后二十秒上下:重连退避 1、2、4、8 秒,各带 ±25% 抖动。
/// 30 秒比它多一半余量。再长保护不到任何一次能续上的重连,只会让横幅挂得更久:
/// force-stop 之后横幅 30 秒撤;没有 FIN 的半开连接还要先等探活出册(30 秒一次、
/// 两次不回,即 60~90 秒),再加这 30 秒。
pub const LEASE: Duration = Duration::from_secs(30);

/// 账号上的播放组:谁在遥控、哪几台在出声、进行中的换输出操作(#137 ③)。
///
/// 三种身份分开记:遥控器(`controller`)、组的成员(`members`,本轮至多一台,
/// 它也就是主端)、以及一次操作里新来的设备(`pending`)。从前这里只有一条
/// 「谁遥控谁」,于是选别的设备只能把旧目标直接顶掉 —— 它不知道自己该停,
/// 横幅也一直挂着(#137 ① F5)。
struct Group {
    /// 遥控器。满租约清成 `None`,**组不散**:成员照旧在放,下一位遥控器
    /// 接上时它们还在组里。
    controller: Option<Controller>,
    /// 已确认的输出。
    members: Vec<String>,
    /// 主端任期:成员集合每提交一次加一。
    term: u64,
    /// 主端:持有组时间线、决定下一首的那一台(#137 ⑤)。只有它发的共同计划才转。
    /// 它下线时**不**另选(产品规则:跟随端把已确认的计划放完再停)。
    master: Option<String>,
    /// 进行中的那一次:它确认之后的输出集合。
    pending: Option<Pending>,
}

struct Controller {
    device: String,
    generation: Generation,
    /// 遥控器的会话断掉的时刻。`None` 是在线;满一个租约就清掉遥控器。
    left_at: Option<Instant>,
}

struct Pending {
    operation_id: String,
    outputs: Vec<String>,
    /// 提交之后谁当主端。
    master: Option<String>,
}

impl Group {
    fn empty() -> Self {
        Self {
            controller: None,
            members: Vec::new(),
            term: 0,
            master: None,
            pending: None,
        }
    }

    /// 此刻有权发共同计划的那一台:已确认的主端;还没有主端(组正在成形)时，是进行中那一次
    /// 指定的主端 —— 迁移的第三步就要它开始发计划，等不到提交。
    fn publisher(&self) -> Option<&str> {
        self.master.as_deref().or_else(|| {
            self.pending
                .as_ref()
                .and_then(|pending| pending.master.as_deref())
        })
    }

    /// 组里每一台(含进行中那一次拉进来的),按先成员后新来的顺序，不重复。
    fn everyone(&self) -> Vec<String> {
        let mut all = self.members.clone();
        if let Some(pending) = &self.pending {
            for id in &pending.outputs {
                if !all.contains(id) {
                    all.push(id.clone());
                }
            }
        }
        all
    }

    /// 这台设备在不在组里 —— 已确认的成员,或者正被一次操作拉进来。
    fn includes(&self, device: &str) -> bool {
        self.members.iter().any(|id| id == device)
            || self.pending.as_ref().is_some_and(
                |pending| {
                    pending
                        .outputs
                        .iter()
                        .any(|id| id == device)
                },
            )
    }

    /// 没有成员、也没有进行中的操作:这个组已经不存在了。
    fn is_vacant(&self) -> bool {
        self.members.is_empty() && self.pending.is_none()
    }

    fn controlled_by(&self, device: &str) -> bool {
        self.controller.as_ref().is_some_and(|controller| {
            controller.device == device
        })
    }

    /// 进行中那一次拉进来、却不会留下的设备 —— 它们要撤锁。
    fn stranded(&self, keep: &[String]) -> Vec<String> {
        self.pending
            .as_ref()
            .map(|pending| {
                pending
                    .outputs
                    .iter()
                    .filter(|id| {
                        !self.members.contains(id)
                            && !keep.contains(id)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 每个账号至多一个受控组(产品规则:不做多个并发的受控组),所以是一张
/// 按账号的表,每格一个组。
///
/// 遥控器下线不解锁成员,但也不永远锁着:替它留一个租约,租约内续上就当没断过,
/// 满了就清掉遥控器 —— 成员下一条上报拿到 `NotControlled` 自己解锁。清的只是锁,
/// 成员的播放不动(#109 AC-10 的续播),组也不散。
pub struct Control {
    groups: HashMap<AccountId, Group>,
    next_generation: Generation,
    lease: Duration,
}

impl Default for Control {
    fn default() -> Self {
        Self::with_lease(LEASE)
    }
}

/// [`Control::claim`] 的下场。
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// 拿到了。`revoked` 是被顶掉的那个遥控器 —— 它得知道自己失权了。
    Granted {
        generation: Generation,
        revoked: Option<String>,
    },
    /// 续不上:手上那个代次早就被顶替了,现在持权的是 `by`。
    Stale { by: String },
}

/// [`Control::begin`] 的下场。
#[derive(Debug, PartialEq, Eq)]
pub struct Begun {
    pub generation: Generation,
    /// 被这一次接管顶掉的遥控器。
    pub revoked: Option<String>,
    /// 上一次操作拉进来、这一次不要了的设备 —— 要撤锁。
    pub dropped: Vec<String>,
}

/// [`Control::commit`] 的下场。
#[derive(Debug, PartialEq, Eq)]
pub enum Committed {
    Done {
        term: u64,
        /// 换下来的成员 —— 要撤锁。
        removed: Vec<String>,
        /// 换上之后的成员与主端，要通告给组里每一台与遥控器。
        members: Vec<String>,
        master: Option<String>,
    },
    NotController,
    /// 进行中的不是这一次(或者根本没有进行中的)。
    Mismatch,
}

impl Control {
    /// 自定租约长度。测试要毫秒级的,生产用 [`LEASE`]。
    pub fn with_lease(lease: Duration) -> Self {
        Self {
            groups: HashMap::new(),
            next_generation: 0,
            lease,
        }
    }

    fn fresh_generation(&mut self) -> Generation {
        let generation = self.next_generation;
        self.next_generation += 1;
        generation
    }

    /// 一台设备的会话断了:出册,或者以新连接重新入册。它若正是这个账号的
    /// 遥控器,租约从 `now` 起算;已经在算的不重算。
    ///
    /// 成员出册不归这里,走 [`Control::release`]。
    pub fn controller_left(
        &mut self,
        account: AccountId,
        device: &str,
        now: Instant,
    ) {
        if let Some(group) = self.groups.get_mut(&account)
            && let Some(controller) =
                group.controller.as_mut()
            && controller.device == device
            && controller.left_at.is_none()
        {
            controller.left_at = Some(now);
            tracing::info!(
                account,
                controller = %device,
                members = ?group.members,
                "遥控器会话断过,控制权租约起算"
            );
        }
    }

    /// 遥控器下线满一个租约的,清掉遥控器 —— **组不散**。
    ///
    /// 进行中的操作一并作罢:没有遥控器,它永远等不到提交。被它拉进来的设备
    /// 下一条上报拿到 `NotControlled` 自己撤锁。
    ///
    /// 惰性判,不另起定时器:成员每秒上报,每条消息进来先过这一道,
    /// 于是过期最迟一秒被发现,而服务端不必为每个下线的遥控器挂一个任务。
    pub fn expire(
        &mut self,
        account: AccountId,
        now: Instant,
    ) {
        let lease = self.lease;
        let Some(group) = self.groups.get_mut(&account)
        else {
            return;
        };
        let expired = group
            .controller
            .as_ref()
            .is_some_and(|controller| {
                controller.left_at.is_some_and(|left| {
                    now.duration_since(left) >= lease
                })
            });
        if !expired {
            return;
        }
        if let Some(controller) = group.controller.take() {
            tracing::info!(
                account,
                controller = %controller.device,
                members = ?group.members,
                "遥控器下线满租约,清掉遥控器(组不散)"
            );
        }
        group.pending = None;
        if group.is_vacant() {
            self.groups.remove(&account);
        }
    }

    /// 接管 `target`。
    ///
    /// `resume` 是重连时自动重发的那一次带回来的旧代次:对得上就原样续,
    /// 对不上只能认输。`None` 是用户**主动**按下的那一次,顶掉任何人,
    /// 组的成员直接换成 `target`(不经迁移;协议 5 的遥控器改走
    /// `BeginOutputs`,这一条留给续权与旧测试)。
    /// 不分这两种的话,断线的手机一恢复网络就把接管者顶掉了 ——
    /// 而接管者那边什么都没做过(产品规则:旧遥控器自动重连不夺回)。
    pub fn claim(
        &mut self,
        account: AccountId,
        controller: &str,
        target: &str,
        resume: Option<Generation>,
    ) -> Claim {
        if let Some(generation) = resume {
            let Some(group) = self.groups.get_mut(&account)
            else {
                return Claim::Stale {
                    by: target.to_owned(),
                };
            };
            let includes = group.includes(target);
            return match group.controller.as_mut() {
                Some(held)
                    if held.generation == generation
                        && held.device == controller
                        && includes =>
                {
                    // 续上了就是回来了,租约作废。
                    if held.left_at.take().is_some() {
                        tracing::info!(
                            account,
                            controller = %controller,
                            "遥控器租约内续上"
                        );
                    }
                    Claim::Granted {
                        generation,
                        revoked: None,
                    }
                }
                // 组还在、遥控器却不是它:续不上。组里没有 `target` 也算续不上 ——
                // 期间它退出过,遥控权是它撤的。谎报一个「续上了」会让遥控器对着
                // 一台已经解锁的设备发命令。
                Some(held) => Claim::Stale {
                    by: held.device.clone(),
                },
                None => Claim::Stale {
                    by: target.to_owned(),
                },
            };
        }

        let generation = self.fresh_generation();
        let group = self
            .groups
            .entry(account)
            .or_insert_with(Group::empty);
        // 自己顶自己不算换人 —— 给自己发一条撤权,遥控器会把自己降级回本机。
        let revoked = group
            .controller
            .as_ref()
            .map(|held| held.device.clone())
            .filter(|old| old != controller);
        group.controller = Some(Controller {
            device: controller.to_owned(),
            generation,
            left_at: None,
        });
        if group.members != [target] {
            group.members = vec![target.to_owned()];
            group.term += 1;
        }
        group.master = Some(target.to_owned());
        group.pending = None;

        Claim::Granted {
            generation,
            revoked,
        }
    }

    /// 遥控器开始一次「改在这些设备播放」。只登记,不换成员。
    ///
    /// 不是当前遥控器的那台发来,就是接管:旧遥控器被顶掉(与主动接管同一条
    /// 产品规则)。当前遥控器自己发来,代次不变。
    pub fn begin(
        &mut self,
        account: AccountId,
        controller: &str,
        operation_id: &str,
        outputs: Vec<String>,
        master: Option<String>,
    ) -> Begun {
        let fresh = self.fresh_generation();
        let group = self
            .groups
            .entry(account)
            .or_insert_with(Group::empty);

        let (generation, revoked) =
            match group.controller.as_mut() {
                Some(held) if held.device == controller => {
                    held.left_at = None;
                    (held.generation, None)
                }
                _ => {
                    let revoked = group
                        .controller
                        .replace(Controller {
                            device: controller.to_owned(),
                            generation: fresh,
                            left_at: None,
                        })
                        .map(|old| old.device);
                    (fresh, revoked)
                }
            };

        let dropped = group.stranded(&outputs);
        // 指定的主端不在集合里就取第一台：不留一个没有主端的组。
        let master = master
            .filter(|id| outputs.contains(id))
            .or_else(|| outputs.first().cloned());
        group.pending = Some(Pending {
            operation_id: operation_id.to_owned(),
            outputs,
            master,
        });

        Begun {
            generation,
            revoked,
            dropped,
        }
    }

    /// 进行中那一次确认完了:成员换成它的输出集合,任期加一。
    ///
    /// 成员集合变空就是组散了(改回本机):整格清掉。
    /// `outputs` 是真正跟上的那几台(#137 ⑤),必须是登记那一份的子集;`None` 就是整份。
    /// 登记了却没跟上的新来者与换下来的成员一起撤锁。
    pub fn commit(
        &mut self,
        account: AccountId,
        controller: &str,
        operation_id: &str,
        outputs: Option<Vec<String>>,
    ) -> Committed {
        let Some(group) = self.groups.get_mut(&account)
        else {
            return Committed::NotController;
        };
        if !group.controlled_by(controller) {
            return Committed::NotController;
        }
        let matches = group.pending.as_ref().is_some_and(|pending| {
            pending.operation_id == operation_id
                && outputs.as_ref().is_none_or(|subset| {
                    subset.iter().all(|id| pending.outputs.contains(id))
                })
        });
        let Some(pending) = group.pending.take_if(|_| matches) else {
            return Committed::Mismatch;
        };
        let kept = outputs.unwrap_or_else(|| pending.outputs.clone());

        let removed = group
            .members
            .iter()
            .chain(pending.outputs.iter().filter(|id| !group.members.contains(id)))
            .filter(|id| !kept.contains(id))
            .cloned()
            .collect();
        group.master = pending
            .master
            .filter(|id| kept.contains(id))
            .or_else(|| kept.first().cloned());
        group.members = kept;
        group.term += 1;
        let term = group.term;
        let members = group.members.clone();
        let master = group.master.clone();
        if group.is_vacant() {
            self.groups.remove(&account);
        }
        Committed::Done {
            term,
            removed,
            members,
            master,
        }
    }

    /// 放弃进行中那一次:返回它拉进来、要撤锁的设备。对不上就什么都不动。
    pub fn abort(
        &mut self,
        account: AccountId,
        controller: &str,
        operation_id: &str,
    ) -> Vec<String> {
        let Some(group) = self.groups.get_mut(&account)
        else {
            return Vec::new();
        };
        let matches = group.controlled_by(controller)
            && group.pending.as_ref().is_some_and(
                |pending| {
                    pending.operation_id == operation_id
                },
            );
        if !matches {
            return Vec::new();
        }
        let dropped = group.stranded(&[]);
        group.pending = None;
        if group.is_vacant() {
            self.groups.remove(&account);
        }
        dropped
    }

    /// 一台设备离开组:自己按了退出,或者它下线了。组因此空了的话,
    /// 返回失权的那个遥控器,由调用方去通知。
    ///
    /// **只认成员那一侧**。遥控器下线不解锁成员 —— 手机没电不能让 pc1 停
    /// (产品规则)。这两条最容易写反,写反的症状是手机一锁屏 pc1 就自己解锁了。
    pub fn release(
        &mut self,
        account: AccountId,
        device: &str,
    ) -> Option<String> {
        let group = self.groups.get_mut(&account)?;
        if !group.includes(device) {
            return None;
        }
        group.members.retain(|id| id != device);
        if let Some(pending) = group.pending.as_mut() {
            pending.outputs.retain(|id| id != device);
            if pending.master.as_deref() == Some(device) {
                pending.master = pending.outputs.first().cloned();
            }
        }
        // 主端自己按了退出：它不再往下发计划。这是它主动走的，不是失联，但同样不替谁另选。
        if group.master.as_deref() == Some(device) {
            group.master = None;
        }
        if !group.members.is_empty() {
            return None;
        }
        self.groups
            .remove(&account)
            .and_then(|group| group.controller)
            .map(|controller| controller.device)
    }

    /// 这台设备现在被谁遥控。不在组里、或者组此刻没有遥控器则 `None`。
    pub fn controller_of(
        &self,
        account: AccountId,
        target: &str,
    ) -> Option<&str> {
        let group = self.groups.get(&account)?;
        if !group.includes(target) {
            return None;
        }
        group
            .controller
            .as_ref()
            .map(|controller| controller.device.as_str())
    }

    /// 这个账号的组此刻的成员(已确认的输出)。
    pub fn members(
        &self,
        account: AccountId,
    ) -> Vec<String> {
        self.groups
            .get(&account)
            .map(|group| group.members.clone())
            .unwrap_or_default()
    }

    /// 这个账号的组此刻的主端。
    pub fn master(&self, account: AccountId) -> Option<String> {
        self.groups.get(&account)?.master.clone()
    }

    /// 组现在的样子(任期、有权发计划的那一台、全部成员),给通告用。没有组时 `None`。
    pub fn shape(
        &self,
        account: AccountId,
    ) -> Option<(u64, Option<String>, Vec<String>)> {
        let group = self.groups.get(&account)?;
        Some((
            group.term,
            group.publisher().map(str::to_owned),
            group.everyone(),
        ))
    }

    /// 一份共同计划该转给谁：发信人得是此刻有权发计划的那一台、任期得是当前任期。
    /// 对上了返回收件人(组里其余每一台与遥控器),对不上返回错误码。
    pub fn plan_recipients(
        &self,
        account: AccountId,
        from: &str,
        term: u64,
    ) -> Result<Vec<String>, &'static str> {
        let Some(group) = self.groups.get(&account) else {
            return Err("not_master");
        };
        if group.publisher() != Some(from) {
            return Err("not_master");
        }
        if group.term != term {
            return Err("stale_term");
        }
        let mut to: Vec<String> = group
            .everyone()
            .into_iter()
            .filter(|id| id != from)
            .collect();
        if let Some(controller) = &group.controller
            && controller.device != from
            && !to.contains(&controller.device)
        {
            to.push(controller.device.clone());
        }
        Ok(to)
    }

    /// 这个账号的组的主端任期。没有组时是 0。
    pub fn term(&self, account: AccountId) -> u64 {
        self.groups
            .get(&account)
            .map_or(0, |group| group.term)
    }
}

/// 处理一条遥控相关的消息。返回要发回给发信人自己的应答(没有则 `None`)。
///
/// 与 [`crate::syncplay::signaling::route`] 同一个形状、同一条纪律:**同步**函数,
/// 发往别人的消息就地 `try_send`,发回自己的走返回值。握着名册的锁 await
/// 会把所有人的名册一起卡住。
pub fn route(
    roster: &Roster<Sink>,
    control: &mut Control,
    account: AccountId,
    from: &str,
    message: ClientSignal,
) -> Option<ServerSignal> {
    control.expire(account, Instant::now());
    match message {
        ClientSignal::ClaimControl { target, resume } => {
            claim(
                roster, control, account, from, &target,
                resume,
            )
        }
        // 查不到槽位时回一条 `NotControlled`,不静默吞掉:被控端的锁定态
        // 只有它自己清,而槽位可能早就没了(服务端重启、遥控关系被别处撤掉)。
        // 吞掉的话那一下「退出被遥控」石沉大海,横幅撤了、锁定态却留着(#102 F-004)。
        ClientSignal::ExitControlled => {
            let Some(controller) =
                control.release(account, from)
            else {
                return Some(ServerSignal::NotControlled);
            };
            send(
                roster,
                account,
                &controller,
                ServerSignal::ControlRevoked {
                    by: from.to_owned(),
                },
            );
            None
        }
        ClientSignal::Command { to, cmd } => {
            // 这一跳是遥控链路上唯一能同时看见两端的位置:命令到底有没有
            // 到过服务端、槽位对不对得上,只有这里答得了。少了它,一次
            // 点歌失败时「遥控器没发」与「服务端没转」在两边都查不出来。
            let summary = cmd.summary();
            let outcome = forward(
                roster,
                control,
                account,
                from,
                &to,
                ServerSignal::Command { cmd },
            );
            match &outcome {
                None => tracing::info!(
                    account,
                    from = %from,
                    to = %to,
                    cmd = %summary,
                    "遥控命令已转发"
                ),
                Some(ServerSignal::Error {
                    code, ..
                }) => tracing::info!(
                    account,
                    from = %from,
                    to = %to,
                    cmd = %summary,
                    code = %code,
                    "遥控命令没转出去"
                ),
                Some(_) => {}
            }
            outcome
        }
        // 上报的目标由槽位定,不由被控端指定:让它自己写目标的话,
        // 它能把自己的播放位置每秒推给任何一台设备。
        ClientSignal::State { state } => {
            // 没有槽位却还在上报 = 被控端挂着一条假的锁定态。告诉它一声,
            // 它就能自己解锁 —— 上报每秒一条,所以最迟一秒纠正过来。
            let Some(controller) =
                control.controller_of(account, from)
            else {
                return Some(ServerSignal::NotControlled);
            };
            // 借用要在 send 之前还掉 —— roster 与 control 是两个对象,
            // 但这个 &str 借的是 control。
            let controller = controller.to_owned();
            send(
                roster,
                account,
                &controller,
                ServerSignal::State {
                    from: from.to_owned(),
                    state,
                },
            );
            None
        }
        ClientSignal::SnapshotRequest { to } => forward(
            roster,
            control,
            account,
            from,
            &to,
            ServerSignal::SnapshotRequest,
        ),
        // 握手与校时归 `signaling::route`,到不了这里。
        ClientSignal::Hello { .. }
        | ClientSignal::TimePing { .. } => None,
        ClientSignal::GroupPlan { term, plan } => {
            match control.plan_recipients(account, from, term) {
                Ok(to) => {
                    for device in to {
                        send(
                            roster,
                            account,
                            &device,
                            ServerSignal::GroupPlan {
                                from: from.to_owned(),
                                term,
                                plan: plan.clone(),
                            },
                        );
                    }
                    None
                }
                Err(code) => {
                    tracing::info!(
                        account,
                        from = %from,
                        term,
                        code,
                        "共同计划没转:不是当前主端或任期不对"
                    );
                    Some(ServerSignal::Error {
                        code: code.to_owned(),
                        message: "只有当前主端、当前任期的计划才转"
                            .to_owned(),
                    })
                }
            }
        }
        ClientSignal::BeginOutputs {
            operation_id,
            outputs,
            master,
        } => begin_outputs(
            roster,
            control,
            account,
            from,
            &operation_id,
            outputs,
            master,
        ),
        ClientSignal::CommitOutputs {
            operation_id,
            outputs,
        } => commit_outputs(
            roster,
            control,
            account,
            from,
            &operation_id,
            outputs,
        ),
        ClientSignal::AbortOutputs { operation_id } => {
            let dropped =
                control.abort(account, from, &operation_id);
            tracing::info!(
                account,
                from = %from,
                operation = %operation_id,
                dropped = ?dropped,
                "换输出作罢"
            );
            // 被拉进来又作罢的:撤锁,再告诉它组现在的样子(里面没有它)—— 它登记时收到过
            // 一份把自己算在内的通告,不更正的话它一直以为自己在组里(#137 ⑤)。
            let (term, master, members) = control
                .shape(account)
                .unwrap_or((0, None, Vec::new()));
            for device in dropped.iter().filter(|id| *id != from) {
                send(
                    roster,
                    account,
                    device,
                    ServerSignal::NotControlled,
                );
                send(
                    roster,
                    account,
                    device,
                    ServerSignal::Group {
                        term,
                        master: master.clone(),
                        members: members.clone(),
                    },
                );
            }
            None
        }
    }
}

/// 接管:校验目标,写槽位,通知被控端与被顶掉的那台。
fn claim(
    roster: &Roster<Sink>,
    control: &mut Control,
    account: AccountId,
    from: &str,
    target: &str,
    resume: Option<Generation>,
) -> Option<ServerSignal> {
    // 选本机作输出设备走的是本地那条路,一条信令都不该发出来。
    if target == from {
        return Some(ServerSignal::Error {
            code: "cannot_control_self".to_owned(),
            message: "本机输出不必经过服务端".to_owned(),
        });
    }
    // 跨账号一律取不到,于是与真的不在线一个说法 —— 分两种说法等于
    // 白送一条枚举信道(理由同 `signaling::route`)。
    if roster.device(account, target).is_none() {
        return Some(ServerSignal::Error {
            code: "device_offline".to_owned(),
            message: format!("设备 {target} 不在线"),
        });
    }

    match control.claim(account, from, target, resume) {
        Claim::Stale { by } => {
            Some(ServerSignal::ControlRevoked { by })
        }
        Claim::Granted {
            generation,
            revoked,
        } => {
            // 被控端非知道不可:它要据此锁住本地播放动作、挂出横幅。
            // 名字带过去而不只是 id —— 横幅上写的是人看得懂的那个。
            let me = roster
                .device(account, from)
                .cloned()
                .unwrap_or(contract::DeviceDto {
                    id: from.to_owned(),
                    name: from.to_owned(),
                });
            send(
                roster,
                account,
                target,
                ServerSignal::ControlledBy { device: me },
            );
            if let Some(old) = revoked {
                send(
                    roster,
                    account,
                    &old,
                    ServerSignal::ControlRevoked {
                        by: from.to_owned(),
                    },
                );
            }
            Some(ServerSignal::ControlGranted {
                generation,
            })
        }
    }
}

/// 把一条消息转给被控端,前提是发信人此刻真的持权。
///
/// 不查这一条的话,同账号下任意一台设备都能让 pc1 切歌 ——
/// 而 pc1 前面的人只会看到歌自己跳了。
/// 开始一次换输出:先查输出都用得上,再登记,再锁上新来的、撤掉被顶掉的。
fn begin_outputs(
    roster: &Roster<Sink>,
    control: &mut Control,
    account: AccountId,
    from: &str,
    operation_id: &str,
    outputs: Vec<String>,
    master: Option<String>,
) -> Option<ServerSignal> {
    // 整次校验在先:锁上一半再发现另一半不在线,就得回头一台台撤。
    // 遥控器本机可以和别的设备一起在集合里(#137 ⑤);只有它自己时是单机输出，不经服务端。
    let alone = outputs.len() < 2;
    for output in &outputs {
        if output == from && !alone {
            continue;
        }
        if output == from {
            return Some(ServerSignal::Error {
                code: "cannot_control_self".to_owned(),
                message: "本机输出不必经过服务端"
                    .to_owned(),
            });
        }
        // 跨账号一律取不到,于是与真的不在线一个说法(理由同 `claim`)。
        if roster.device(account, output).is_none() {
            return Some(ServerSignal::Error {
                code: "device_offline".to_owned(),
                message: format!("设备 {output} 不在线"),
            });
        }
    }

    let begun = control.begin(
        account,
        from,
        operation_id,
        outputs.clone(),
        master,
    );
    tracing::info!(
        account,
        from = %from,
        operation = %operation_id,
        outputs = ?outputs,
        generation = begun.generation,
        "换输出开始"
    );
    let me = roster
        .device(account, from)
        .cloned()
        .unwrap_or(contract::DeviceDto {
            id: from.to_owned(),
            name: from.to_owned(),
        });
    // 集合里每一台都(重新)锁上:原本就在组里、却在遥控器离线满租约时撤过锁的
    // 那台,也得重新开始听命令、开始上报。
    for output in outputs.iter().filter(|id| *id != from) {
        send(
            roster,
            account,
            output,
            ServerSignal::ControlledBy {
                device: me.clone(),
            },
        );
    }
    for device in begun.dropped.iter().filter(|id| *id != from) {
        send(
            roster,
            account,
            device,
            ServerSignal::NotControlled,
        );
    }
    if let Some(old) = begun.revoked {
        send(
            roster,
            account,
            &old,
            ServerSignal::ControlRevoked {
                by: from.to_owned(),
            },
        );
    }
    // 告诉有权发计划的那一台与新来的：组要变成这样了。主端据此把计划再发一遍，新来的
    // 据此知道该听谁的(#137 ⑤)。原来就在的普通成员不必知道，提交时一起通告。
    if let Some((term, publisher, everyone)) = control.shape(account) {
        let members = control.members(account);
        for device in &everyone {
            let newcomer = !members.contains(device);
            if newcomer || publisher.as_deref() == Some(device.as_str()) {
                send(
                    roster,
                    account,
                    device,
                    ServerSignal::Group {
                        term,
                        master: publisher.clone(),
                        members: everyone.clone(),
                    },
                );
            }
        }
    }
    Some(ServerSignal::OutputsBegun {
        operation_id: operation_id.to_owned(),
        generation: begun.generation,
    })
}

/// 提交一次换输出:成员换过去,换下来的撤锁。
fn commit_outputs(
    roster: &Roster<Sink>,
    control: &mut Control,
    account: AccountId,
    from: &str,
    operation_id: &str,
    outputs: Option<Vec<String>>,
) -> Option<ServerSignal> {
    match control.commit(account, from, operation_id, outputs) {
        Committed::Done {
            term,
            removed,
            members,
            master,
        } => {
            tracing::info!(
                account,
                from = %from,
                operation = %operation_id,
                term,
                removed = ?removed,
                "换输出提交"
            );
            // 遥控器本机被移出组时不给自己撤锁：它从来没锁过自己。
            for device in removed.iter().filter(|id| *id != from) {
                send(
                    roster,
                    account,
                    device,
                    ServerSignal::NotControlled,
                );
            }
            // 组的新样子通告给每一台成员与遥控器:新主端从这一刻起有权往下发(#137 ⑤)。
            // 被换下来的也通告一份:它据此看出自己不在里面、离组。撤锁不能代替这一条 ——
            // 遥控器满租约时同样撤锁,而那时组不散。
            let announcement = ServerSignal::Group {
                term,
                master,
                members: members.clone(),
            };
            let mut told = members.clone();
            for id in removed.iter().chain(std::iter::once(&from.to_owned())) {
                if !told.contains(id) {
                    told.push(id.clone());
                }
            }
            for device in &told {
                send(roster, account, device, announcement.clone());
            }
            Some(ServerSignal::OutputsCommitted {
                operation_id: operation_id.to_owned(),
                term,
            })
        }
        Committed::NotController => {
            Some(ServerSignal::Error {
                code: "not_controller".to_owned(),
                message: "没有这个播放组的控制权"
                    .to_owned(),
            })
        }
        Committed::Mismatch => Some(ServerSignal::Error {
            code: "operation_mismatch".to_owned(),
            message: format!(
                "进行中的不是操作 {operation_id}"
            ),
        }),
    }
}

fn forward(
    roster: &Roster<Sink>,
    control: &Control,
    account: AccountId,
    from: &str,
    to: &str,
    message: ServerSignal,
) -> Option<ServerSignal> {
    if control.controller_of(account, to) != Some(from) {
        return Some(ServerSignal::Error {
            code: "not_controller".to_owned(),
            message: format!("没有 {to} 的控制权"),
        });
    }
    let Some(sink) = roster.sink(account, to) else {
        return Some(ServerSignal::Error {
            code: "device_offline".to_owned(),
            message: format!("设备 {to} 不在线"),
        });
    };
    match sink.try_send(message) {
        Ok(()) => None,
        Err(_) => Some(ServerSignal::Error {
            code: "device_unreachable".to_owned(),
            message: format!("设备 {to} 收不下消息"),
        }),
    }
}

/// 尽力送一条消息。送不到就算了 —— 那条连接已经死了,它自己会出册。
fn send(
    roster: &Roster<Sink>,
    account: AccountId,
    to: &str,
    message: ServerSignal,
) {
    if let Some(sink) = roster.sink(account, to) {
        let _ = sink.try_send(message);
    }
}

#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod tests;
