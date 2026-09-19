//! 遥控器模式的控制权:每个账号一个槽位,以及照着它转发命令与上报。
//!
//! **服务端仍然不解释命令**(`docs/adr/0030`)。它只做两件事:记住"谁在遥控谁",
//! 以及照着这条记录把消息转到对的那一端。`RemoteCommand` 与 `RemoteStateDto`
//! 从这里原样穿过去,和同播的 SDP 载荷一个待遇(`docs/adr/0008`)。
//!
//! 单独成模块而不是塞进 [`crate::signaling`]:那个文件已经五百多行,而"谁能
//! 控制谁"是纯逻辑 —— 恰恰也是会出错的地方,值得离开 WebSocket 被测。

use std::collections::HashMap;

use contract::{ClientSignal, ServerSignal};

use crate::roster::Roster;
use crate::signaling::{AccountId, Sink};

/// 一次控制权的代次。
///
/// 全局递增,不按账号分 —— 它只需要互不相同。断线的遥控器重连时带着手上
/// 这个数回来,服务端据此认得出它早就被顶替了(见 [`Control::claim`])。
pub type Generation = u64;

/// 账号上当前那一条遥控关系。
struct Grant {
    /// 遥控器的设备 id。
    controller: String,
    /// 被控端的设备 id。
    target: String,
    generation: Generation,
}

/// 每个账号至多一台遥控器(产品规则),所以是**一个槽位**而不是一张表。
#[derive(Default)]
pub struct Control {
    slots: HashMap<AccountId, Grant>,
    next_generation: Generation,
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
        let current = self.slots.get(&account);

        if let Some(generation) = resume {
            return match current {
                Some(grant)
                    if grant.generation == generation
                        && grant.controller
                            == controller
                        && grant.target == target =>
                {
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
        let revoked = current
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
/// 与 [`crate::signaling::route`] 同一个形状、同一条纪律:**同步**函数,
/// 发往别人的消息就地 `try_send`,发回自己的走返回值。握着名册的锁 await
/// 会把所有人的名册一起卡住。
pub fn route(
    roster: &Roster<Sink>,
    control: &mut Control,
    account: AccountId,
    from: &str,
    message: ClientSignal,
) -> Option<ServerSignal> {
    match message {
        ClientSignal::ClaimControl { target, resume } => {
            claim(
                roster, control, account, from, &target,
                resume,
            )
        }
        ClientSignal::ExitControlled => {
            let controller =
                control.release(account, from)?;
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
        ClientSignal::Command { to, cmd } => forward(
            roster,
            control,
            account,
            from,
            &to,
            ServerSignal::Command { cmd },
        ),
        // 上报的目标由槽位定,不由被控端指定:让它自己写目标的话,
        // 它能把自己的播放位置每秒推给任何一台设备。
        ClientSignal::State { state } => {
            let controller =
                control.controller_of(account, from)?;
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
mod tests {
    use contract::{
        ClientSignal, DeviceDto, RemoteCommand,
        RemotePlayState, RemoteStateDto, ServerSignal,
    };
    use similar_asserts::assert_eq;
    use tokio::sync::mpsc;

    use super::*;
    use crate::roster::Roster;
    use crate::signaling::{AccountId, Sink};

    const ALICE: AccountId = 1;
    const BOB: AccountId = 2;
    const CAPACITY: usize = 32;

    fn device(id: &str) -> DeviceDto {
        DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        }
    }

    fn report() -> RemoteStateDto {
        RemoteStateDto {
            track: None,
            position_ms: 1_000,
            state: RemotePlayState::Playing,
            queue: Vec::new(),
            queue_index: 0,
            volume: 1.0,
            sent_at: 42,
        }
    }

    /// 一台手机、一台 pc、外加一台备用手机,都在 Alice 名下。
    fn three_devices() -> (
        Roster<Sink>,
        mpsc::Receiver<ServerSignal>,
        mpsc::Receiver<ServerSignal>,
        mpsc::Receiver<ServerSignal>,
    ) {
        let (phone, rx_phone) = mpsc::channel(CAPACITY);
        let (pc, rx_pc) = mpsc::channel(CAPACITY);
        let (spare, rx_spare) = mpsc::channel(CAPACITY);
        let mut roster = Roster::default();
        roster.join(ALICE, device("phone"), phone);
        roster.join(ALICE, device("pc"), pc);
        roster.join(ALICE, device("spare"), spare);
        (roster, rx_phone, rx_pc, rx_spare)
    }

    /// 接管成功:遥控器拿到代次,被控端收到「你被谁遥控了」。
    ///
    /// 被控端非知道不可 —— 它要据此锁住本地播放动作并挂出横幅。
    #[test]
    fn claiming_grants_control_and_tells_the_target() {
        let (roster, mut rx_phone, mut rx_pc, _) =
            three_devices();
        let mut control = Control::default();

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );

        assert!(
            matches!(
                reply,
                Some(ServerSignal::ControlGranted { .. })
            ),
            "实得 {reply:?}"
        );
        assert_eq!(
            rx_pc.try_recv(),
            Ok(ServerSignal::ControlledBy {
                device: device("phone")
            })
        );
        assert!(
            rx_phone.try_recv().is_err(),
            "应答走返回值,不该再往自己的收件箱里塞一份"
        );
    }

    /// 第二台**主动**接管顶掉第一台,第一台收到撤权。
    #[test]
    fn a_second_claim_revokes_the_first_controller() {
        let (roster, mut rx_phone, _rx_pc, _rx_spare) =
            three_devices();
        let mut control = Control::default();
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        let _ = rx_phone.try_recv();

        route(
            &roster,
            &mut control,
            ALICE,
            "spare",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );

        assert_eq!(
            rx_phone.try_recv(),
            Ok(ServerSignal::ControlRevoked {
                by: "spare".to_owned()
            }),
            "被顶掉的那台必须知道自己失权了"
        );
        assert_eq!(
            control.controller_of(ALICE, "pc"),
            Some("spare")
        );
    }

    /// 同一台遥控器再接管一次,不该给自己发撤权。
    ///
    /// 发了的话遥控器会把自己降级回本机输出,而它其实还持着权。
    #[test]
    fn reclaiming_as_the_same_controller_revokes_nobody() {
        let (roster, mut rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();
        let claim = || ClientSignal::ClaimControl {
            target: "pc".to_owned(),
            resume: None,
        };
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            claim(),
        );
        let _ = rx_phone.try_recv();

        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            claim(),
        );

        assert!(
            rx_phone.try_recv().is_err(),
            "不该给自己发一条撤权"
        );
    }

    /// **断线的旧遥控器重连不夺回**(产品规则)。
    ///
    /// 重连时自动重发的那条带着它手上的旧代次;槽位已经换人,就只能得到
    /// 一条撤权。不区分的话,手机一恢复网络就会把接管者顶掉,而接管者那边
    /// 什么也没做过。
    #[test]
    fn a_stale_resume_does_not_steal_control_back() {
        let (roster, mut rx_phone, _rx_pc, mut rx_spare) =
            three_devices();
        let mut control = Control::default();
        let granted = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        let Some(ServerSignal::ControlGranted {
            generation,
        }) = granted
        else {
            panic!("实得 {granted:?}")
        };
        route(
            &roster,
            &mut control,
            ALICE,
            "spare",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        while rx_phone.try_recv().is_ok() {}
        while rx_spare.try_recv().is_ok() {}

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: Some(generation),
            },
        );

        assert_eq!(
            reply,
            Some(ServerSignal::ControlRevoked {
                by: "spare".to_owned()
            })
        );
        assert_eq!(
            control.controller_of(ALICE, "pc"),
            Some("spare"),
            "重连的那台把控制权抢回去了"
        );
        assert!(
            rx_spare.try_recv().is_err(),
            "接管者不该因为别人重连而收到任何东西"
        );
    }

    /// 代次还对得上的重连:续上,不换人也不换代次。
    #[test]
    fn a_matching_resume_keeps_the_same_generation() {
        let (roster, mut rx_phone, mut rx_pc, _) =
            three_devices();
        let mut control = Control::default();
        let granted = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        let Some(ServerSignal::ControlGranted {
            generation,
        }) = granted
        else {
            panic!("实得 {granted:?}")
        };
        while rx_phone.try_recv().is_ok() {}
        while rx_pc.try_recv().is_ok() {}

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: Some(generation),
            },
        );

        assert_eq!(
            reply,
            Some(ServerSignal::ControlGranted {
                generation
            })
        );
    }

    /// 被控端按了「退出被遥控」:槽位清空,遥控器收到撤权。
    #[test]
    fn the_target_exiting_revokes_its_controller() {
        let (roster, mut rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        while rx_phone.try_recv().is_ok() {}

        route(
            &roster,
            &mut control,
            ALICE,
            "pc",
            ClientSignal::ExitControlled,
        );

        assert_eq!(
            rx_phone.try_recv(),
            Ok(ServerSignal::ControlRevoked {
                by: "pc".to_owned()
            })
        );
        assert_eq!(
            control.controller_of(ALICE, "pc"),
            None
        );
    }

    /// **遥控器下线不解锁被控端**(产品规则:手机没电不能让 pc1 停)。
    ///
    /// 与被控端下线那条正好相反,最容易写反 —— 写反的症状是手机一锁屏
    /// pc1 就自己解锁了,而用户根本没碰过它。
    #[test]
    fn a_controller_going_offline_leaves_the_target_locked()
    {
        let (roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );

        let freed = control.release(ALICE, "phone");

        assert_eq!(freed, None, "遥控器下线不该清掉槽位");
        assert_eq!(
            control.controller_of(ALICE, "pc"),
            Some("phone"),
            "被控端不该因为遥控器下线而解锁"
        );
    }

    /// 被控端下线:槽位清掉,并把失权的遥控器交还给调用方去通知。
    #[test]
    fn the_target_going_offline_frees_the_slot() {
        let (roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );

        let freed = control.release(ALICE, "pc");

        assert_eq!(freed, Some("phone".to_owned()));
        assert_eq!(
            control.controller_of(ALICE, "pc"),
            None
        );
    }

    /// 命令只从持权的那台转过去。
    ///
    /// 不看这一条的话,同账号下任意一台设备都能让 pc1 切歌 ——
    /// 而 pc1 前面的人只会看到歌自己跳了。
    #[test]
    fn a_command_from_a_non_controller_is_refused() {
        let (roster, _rx_phone, mut rx_pc, _) =
            three_devices();
        let mut control = Control::default();

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "spare",
            ClientSignal::Command {
                to: "pc".to_owned(),
                cmd: RemoteCommand::Next,
            },
        );

        assert!(
            matches!(
                reply,
                Some(ServerSignal::Error { ref code, .. })
                    if code == "not_controller"
            ),
            "实得 {reply:?}"
        );
        assert!(
            rx_pc.try_recv().is_err(),
            "没持权的命令不该真的送过去"
        );
    }

    /// 持权时命令原样转给被控端。
    #[test]
    fn a_command_from_the_controller_reaches_the_target() {
        let (roster, _rx_phone, mut rx_pc, _) =
            three_devices();
        let mut control = Control::default();
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        while rx_pc.try_recv().is_ok() {}

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::Command {
                to: "pc".to_owned(),
                cmd: RemoteCommand::Seek { ms: 30_000 },
            },
        );

        assert_eq!(reply, None, "转发成功时不该有应答");
        assert_eq!(
            rx_pc.try_recv(),
            Ok(ServerSignal::Command {
                cmd: RemoteCommand::Seek { ms: 30_000 }
            })
        );
    }

    /// 上报只送给持权的遥控器,不广播。
    ///
    /// 广播的话,同账号的每台设备每秒都会收到一份别人的播放位置。
    #[test]
    fn a_report_goes_only_to_the_controller() {
        let (roster, mut rx_phone, _rx_pc, mut rx_spare) =
            three_devices();
        let mut control = Control::default();
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        while rx_phone.try_recv().is_ok() {}

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "pc",
            ClientSignal::State { state: report() },
        );

        assert_eq!(reply, None);
        assert_eq!(
            rx_phone.try_recv(),
            Ok(ServerSignal::State {
                from: "pc".to_owned(),
                state: report(),
            })
        );
        assert!(rx_spare.try_recv().is_err());
    }

    /// 没人持权时的上报就地丢掉,不回错误。
    ///
    /// 回错误的话,一台刚失权的被控端会每秒收到一条它做不了任何事的报错。
    #[test]
    fn a_report_without_a_controller_is_dropped() {
        let (roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "pc",
            ClientSignal::State { state: report() },
        );

        assert_eq!(reply, None);
    }

    /// 要快照同样要持权,转过去的是一条不带参数的请求。
    #[test]
    fn a_snapshot_request_needs_control() {
        let (roster, _rx_phone, mut rx_pc, _) =
            three_devices();
        let mut control = Control::default();

        let refused = route(
            &roster,
            &mut control,
            ALICE,
            "spare",
            ClientSignal::SnapshotRequest {
                to: "pc".to_owned(),
            },
        );
        assert!(
            matches!(
                refused,
                Some(ServerSignal::Error { ref code, .. })
                    if code == "not_controller"
            ),
            "实得 {refused:?}"
        );

        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        while rx_pc.try_recv().is_ok() {}
        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::SnapshotRequest {
                to: "pc".to_owned(),
            },
        );

        assert_eq!(
            rx_pc.try_recv(),
            Ok(ServerSignal::SnapshotRequest)
        );
    }

    /// 接管一台不在线的设备要回错误,与信令那侧同一个说法。
    #[test]
    fn claiming_an_offline_device_reports_an_error() {
        let (roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "不在线".to_owned(),
                resume: None,
            },
        );

        assert!(
            matches!(
                reply,
                Some(ServerSignal::Error { ref code, .. })
                    if code == "device_offline"
            ),
            "实得 {reply:?}"
        );
    }

    /// 接管自己没有意义:输出设备选本机走的是本地那条路,一条信令都不发。
    #[test]
    fn claiming_yourself_is_refused() {
        let (roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let mut control = Control::default();

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "phone".to_owned(),
                resume: None,
            },
        );

        assert!(
            matches!(
                reply,
                Some(ServerSignal::Error { ref code, .. })
                    if code == "cannot_control_self"
            ),
            "实得 {reply:?}"
        );
        assert_eq!(
            control.controller_of(ALICE, "phone"),
            None
        );
    }

    /// 跨账号够不着:别人账号下的设备一律当作不在线。
    #[test]
    fn claiming_across_accounts_is_refused() {
        let (mut roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let (sink, mut rx_bob) = mpsc::channel(CAPACITY);
        roster.join(BOB, device("bobpc"), sink);
        let mut control = Control::default();

        let reply = route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "bobpc".to_owned(),
                resume: None,
            },
        );

        assert!(
            matches!(
                reply,
                Some(ServerSignal::Error { ref code, .. })
                    if code == "device_offline"
            ),
            "实得 {reply:?}"
        );
        assert!(
            rx_bob.try_recv().is_err(),
            "不该把别人的设备锁进本账号的遥控里"
        );
    }

    /// 两个账号各有各的槽位,互不影响。
    #[test]
    fn each_account_has_its_own_slot() {
        let (mut roster, _rx_phone, _rx_pc, _) =
            three_devices();
        let (bob_phone, _rx1) = mpsc::channel(CAPACITY);
        let (bob_pc, _rx2) = mpsc::channel(CAPACITY);
        roster.join(BOB, device("phone"), bob_phone);
        roster.join(BOB, device("pc"), bob_pc);
        let mut control = Control::default();

        route(
            &roster,
            &mut control,
            ALICE,
            "phone",
            ClientSignal::ClaimControl {
                target: "pc".to_owned(),
                resume: None,
            },
        );
        control.release(BOB, "pc");

        assert_eq!(
            control.controller_of(ALICE, "pc"),
            Some("phone"),
            "别人账号上的退出把这一桶的槽位清掉了"
        );
    }
}
