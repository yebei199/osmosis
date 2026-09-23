//! 遥控器模式的控制权:每个账号一个槽位,以及照着它转发命令与上报。
//!
//! **服务端仍然不解释命令**(`docs/adr/0030`)。它只做两件事:记住"谁在遥控谁",
//! 以及照着这条记录把消息转到对的那一端。`RemoteCommand` 与 `RemoteStateDto`
//! 从这里原样穿过去,和同播的 SDP 载荷一个待遇(`docs/adr/0008`)。
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

/// 账号上当前那一条遥控关系。
struct Grant {
    /// 遥控器的设备 id。
    controller: String,
    /// 被控端的设备 id。
    target: String,
    generation: Generation,
    /// 遥控器的会话断掉的时刻。`None` 是在线;满一个租约就清槽。
    left_at: Option<Instant>,
}

/// 每个账号至多一台遥控器(产品规则),所以是**一个槽位**而不是一张表。
///
/// 遥控器下线不解锁被控端,但也不永远锁着:槽位替它留一个租约,租约内续上
/// 就当没断过,满了就清 —— 被控端下一条上报拿到 `NotControlled` 自己解锁。
/// 清的只是锁,被控端的播放不动(#109 AC-10 的续播)。
pub struct Control {
    slots: HashMap<AccountId, Grant>,
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

impl Control {
    /// 自定租约长度。测试要毫秒级的,生产用 [`LEASE`]。
    pub fn with_lease(lease: Duration) -> Self {
        Self {
            slots: HashMap::new(),
            next_generation: 0,
            lease,
        }
    }

    /// 一台设备的会话断了:出册,或者以新连接重新入册。它若正是这个账号的
    /// 遥控器,租约从 `now` 起算;已经在算的不重算。
    ///
    /// 被控端出册不归这里,走 [`Control::release`]。
    pub fn controller_left(
        &mut self,
        account: AccountId,
        device: &str,
        now: Instant,
    ) {
        if let Some(grant) = self.slots.get_mut(&account)
            && grant.controller == device
            && grant.left_at.is_none()
        {
            grant.left_at = Some(now);
            tracing::info!(
                account,
                controller = %device,
                target = %grant.target,
                "遥控器会话断过,控制权租约起算"
            );
        }
    }

    /// 遥控器下线满一个租约的,清掉它的槽位。
    ///
    /// 惰性判,不另起定时器:被控端每秒上报,每条消息进来先过这一道,
    /// 于是过期最迟一秒被发现,而服务端不必为每个下线的遥控器挂一个任务。
    pub fn expire(
        &mut self,
        account: AccountId,
        now: Instant,
    ) {
        let expired =
            self.slots.get(&account).is_some_and(|grant| {
                grant.left_at.is_some_and(|left| {
                    now.duration_since(left) >= self.lease
                })
            });
        if expired
            && let Some(grant) = self.slots.remove(&account)
        {
            tracing::info!(
                account,
                controller = %grant.controller,
                target = %grant.target,
                "遥控器下线满租约,清掉控制权"
            );
        }
    }

    /// 接管 `target`。
    ///
    /// `resume` 是重连时自动重发的那一次带回来的旧代次:对得上就原样续,
    /// 对不上只能认输。`None` 是用户**主动**按下的那一次,顶掉任何人。
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
            return match self.slots.get_mut(&account) {
                Some(grant)
                    if grant.generation == generation
                        && grant.controller
                            == controller
                        && grant.target == target =>
                {
                    // 续上了就是回来了,租约作废。
                    if grant.left_at.take().is_some() {
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
                // 槽位空着也算续不上:被控端期间退出过,遥控权是它撤的。
                // 谎报一个"续上了"会让遥控器对着一台已经解锁的设备发命令。
                Some(grant) => Claim::Stale {
                    by: grant.controller.clone(),
                },
                None => Claim::Stale {
                    by: target.to_owned(),
                },
            };
        }

        // 自己顶自己不算换人 —— 给自己发一条撤权,遥控器会把自己降级回本机。
        let revoked = self
            .slots
            .get(&account)
            .map(|grant| grant.controller.clone())
            .filter(|old| old != controller);

        let generation = self.next_generation;
        self.next_generation += 1;
        self.slots.insert(
            account,
            Grant {
                controller: controller.to_owned(),
                target: target.to_owned(),
                generation,
                left_at: None,
            },
        );

        Claim::Granted {
            generation,
            revoked,
        }
    }

    /// 被控端撤销这条遥控关系:自己按了退出,或者它下线了。
    /// 返回失权的那个遥控器,由调用方去通知。
    ///
    /// **只认被控端那一侧**。遥控器下线不解锁被控端 —— 手机没电不能让 pc1 停
    /// (产品规则)。这两条最容易写反,写反的症状是手机一锁屏 pc1 就自己解锁了。
    pub fn release(
        &mut self,
        account: AccountId,
        target: &str,
    ) -> Option<String> {
        let grant = self.slots.get(&account)?;
        if grant.target != target {
            return None;
        }
        self.slots
            .remove(&account)
            .map(|grant| grant.controller)
    }

    /// 这台设备现在被谁遥控。没人遥控则 `None`。
    pub fn controller_of(
        &self,
        account: AccountId,
        target: &str,
    ) -> Option<&str> {
        let grant = self.slots.get(&account)?;
        (grant.target == target)
            .then_some(grant.controller.as_str())
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
                    state: Box::new(state),
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
        // 握手与同播的信令归 `signaling::route`,到不了这里。
        ClientSignal::Hello { .. }
        | ClientSignal::Signal { .. } => None,
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
mod tests;
