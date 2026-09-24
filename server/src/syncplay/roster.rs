//! 在线设备名册,以及信令的投递去向。
//!
//! 名册**就是**当前活跃连接的集合 —— 没有设备表、没有落盘、没有"曾经见过"这个状态
//! (`docs/adr/0009`)。因此这里只有一张内存里的表,进程重启即清空,这是对的。
//!
//! 表按**账号**分桶:一台设备只看得见同账号的设备,信令也只在桶内转发。
//! 全局一张表的话,任何人都能看到所有人的设备名,还能去接管陌生人的设备 ——
//! 而那不会报任何错,对面只是莫名其妙地被遥控了。
//!
//! 单独成模块是为了让它离开 WebSocket 被测:连接的生命周期难以在单测里摆布,
//! 而"谁在线、消息该给谁"这两件事是纯逻辑,恰恰也是会出错的地方。

use std::collections::HashMap;

use contract::DeviceDto;

use crate::syncplay::signaling::AccountId;

/// 一条连接在名册里的代次。
///
/// 同一台设备重连时,新旧两条连接有一段重叠:旧连接的清理跑在它自己的任务里,
/// 完全可能**晚于**新连接入册。只按设备 id 删的话,那次清理会把刚上线的新连接
/// 从名册里带走 —— 设备自己以为在线,别人却怎么也找不到它,而这不报任何错。
/// 代次让 [`Roster::leave`] 认得出"我要删的那条早就被顶替了"。
pub type Generation = u64;

/// 名册里的一条。
struct Entry<Sink> {
    device: DeviceDto,
    sink: Sink,
    generation: Generation,
}

/// 一台设备的出口:往它的连接里塞消息用的发送端。
///
/// 泛型而非写死 `mpsc::Sender`:测试里塞一个记录用的假出口,就能验证
/// "只发给目标那一台"这类断言,不必真的建连接。
pub struct Roster<Sink> {
    /// 账号 id → (设备 id → 条目)。
    buckets:
        HashMap<AccountId, HashMap<String, Entry<Sink>>>,
    /// 下一条连接的代次。全局递增,不按设备分 —— 它只需要互不相同。
    next_generation: Generation,
}

impl<Sink> Default for Roster<Sink> {
    fn default() -> Self {
        Self {
            buckets: HashMap::new(),
            next_generation: 0,
        }
    }
}

impl<Sink> Roster<Sink> {
    /// 设备上线。同账号下同 id 已在册时**替换**旧条目并返回它的出口。
    ///
    /// 返回旧出口而不是丢弃:调用方得关掉那条僵死的连接,否则它会一直占着资源。
    /// 一并返回本条连接的代次 —— 出册时要带着它,见 [`Generation`]。
    pub fn join(
        &mut self,
        account: AccountId,
        device: DeviceDto,
        sink: Sink,
    ) -> (Generation, Option<Sink>) {
        let generation = self.next_generation;
        self.next_generation += 1;

        let stale = self
            .buckets
            .entry(account)
            .or_default()
            .insert(
                device.id.clone(),
                Entry {
                    device,
                    sink,
                    generation,
                },
            )
            .map(|entry| entry.sink);

        (generation, stale)
    }

    /// 设备下线。**只删代次相同的那一条**,已经被重连顶替掉的留着不动。
    ///
    /// 返回这次有没有真的删掉。调用方要据此决定要不要顺手清掉别处以它为
    /// 主语的状态(遥控的控制权槽位就是一处)—— 被顶替的那次清理什么都
    /// 不该动,否则一次重连会把刚接上的遥控关系带走。
    pub fn leave(
        &mut self,
        account: AccountId,
        device_id: &str,
        generation: Generation,
    ) -> bool {
        let Some(bucket) = self.buckets.get_mut(&account)
        else {
            return false;
        };

        if bucket.get(device_id).is_none_or(|entry| {
            entry.generation != generation
        }) {
            return false;
        }
        bucket.remove(device_id);

        // 桶空了就连桶一起删:账号数量没有上界,留着空桶等于一张只涨不落的表。
        if bucket.is_empty() {
            self.buckets.remove(&account);
        }
        true
    }

    /// 取某个账号名下某台设备,不在线则 `None`。
    ///
    /// 与 [`Self::sink`] 分开:要名字的地方(遥控的「正被 xx 遥控」横幅)
    /// 不该顺手拿到一个能往里发消息的出口。
    pub fn device(
        &self,
        account: AccountId,
        device_id: &str,
    ) -> Option<&DeviceDto> {
        self.buckets
            .get(&account)?
            .get(device_id)
            .map(|entry| &entry.device)
    }

    /// 某个账号当前在线的全部设备。
    ///
    /// 按 id 排序:`HashMap` 的遍历顺序每次都不同,不排的话名册会无故重排,
    /// 客户端列表就会自己跳来跳去。
    pub fn devices(
        &self,
        account: AccountId,
    ) -> Vec<DeviceDto> {
        let mut devices: Vec<DeviceDto> = self
            .entries(account)
            .map(|entry| entry.device.clone())
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices
    }

    /// 取某个账号名下某台设备的出口,不在线则 `None`。
    ///
    /// 跨账号一律取不到 —— "只能给自己的设备发信令"这条规则就落在这里。
    pub fn sink(
        &self,
        account: AccountId,
        device_id: &str,
    ) -> Option<&Sink> {
        self.buckets
            .get(&account)?
            .get(device_id)
            .map(|entry| &entry.sink)
    }

    /// 某个账号名下的全部出口,用于广播名册变化。
    pub fn sinks(
        &self,
        account: AccountId,
    ) -> impl Iterator<Item = &Sink> {
        self.entries(account).map(|entry| &entry.sink)
    }

    fn entries(
        &self,
        account: AccountId,
    ) -> impl Iterator<Item = &Entry<Sink>> {
        self.buckets
            .get(&account)
            .into_iter()
            .flat_map(HashMap::values)
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 两个账号,用来验分桶。
    const ALICE: AccountId = 1;
    const BOB: AccountId = 2;

    /// 出口用一个可辨认的标记代替真连接。
    fn device(id: &str) -> DeviceDto {
        DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        }
    }

    /// 连上即出现在名册里。
    #[test]
    fn joining_makes_device_visible() {
        let mut roster = Roster::default();

        let (_, stale) =
            roster.join(ALICE, device("a"), "出口a");

        assert!(stale.is_none());

        assert_eq!(
            roster.devices(ALICE),
            vec![device("a")]
        );
    }

    /// 断开即消失。在线没有别的含义 —— 不存在"离线但记着"的状态。
    #[test]
    fn leaving_removes_device() {
        let mut roster = Roster::default();
        let (generation, _) =
            roster.join(ALICE, device("a"), "出口a");

        roster.leave(ALICE, "a", generation);

        assert!(roster.devices(ALICE).is_empty());
        assert!(roster.sink(ALICE, "a").is_none());
    }

    /// 断线重连时旧连接可能还没被清理,同一个 id 不能在名册里出现两次。
    ///
    /// 出现两次的后果不是显示重复那么轻:第二条的出口是死的,
    /// 遥控器会挑到它、把命令发进一条没人读的连接,然后一直等上报。
    #[test]
    fn rejoin_replaces_stale_entry() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("a"), "旧出口");

        let (_, stale) =
            roster.join(ALICE, device("a"), "新出口");

        assert_eq!(
            stale,
            Some("旧出口"),
            "应把旧出口交还给调用方去关掉"
        );
        assert_eq!(roster.devices(ALICE).len(), 1);
        assert_eq!(
            roster.sink(ALICE, "a"),
            Some(&"新出口")
        );
    }

    /// 一台设备也没有时是空列表,不是错误。
    #[test]
    fn empty_roster_when_alone() {
        let roster: Roster<&str> = Roster::default();

        assert!(roster.devices(ALICE).is_empty());
    }

    /// 信令只送给目标那一台。
    ///
    /// 广播出去的话,每台设备都会收到一条不是给自己的命令,
    /// 于是每台都照着去放 —— 而这不会报任何错。
    #[test]
    fn signal_routes_only_to_target() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("a"), "出口a");
        roster.join(ALICE, device("b"), "出口b");

        assert_eq!(roster.sink(ALICE, "b"), Some(&"出口b"));
        assert_ne!(
            roster.sink(ALICE, "b"),
            roster.sink(ALICE, "a")
        );
    }

    /// 目标不在线时明确地没有出口,由调用方回一条错误 —— 不能静默当作送到了。
    #[test]
    fn signal_to_unknown_device_reports_error() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("a"), "出口a");

        assert!(roster.sink(ALICE, "不存在").is_none());
    }

    /// 名册只装得下自己账号的设备。
    ///
    /// 不分桶的话,设备名是谁都看得见的一行字,而"在线的那台"是谁都能挑的目标。
    #[test]
    fn each_account_sees_only_its_own_devices() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("a"), "出口a");
        roster.join(BOB, device("b"), "出口b");

        assert_eq!(
            roster.devices(ALICE),
            vec![device("a")]
        );
        assert_eq!(roster.devices(BOB), vec![device("b")]);
    }

    /// 跨账号取不到出口 —— 信令因此转不过去。
    #[test]
    fn signalling_across_accounts_finds_no_sink() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("a"), "出口a");
        roster.join(BOB, device("b"), "出口b");

        assert!(
            roster.sink(ALICE, "b").is_none(),
            "不该够得着别人账号下的设备"
        );
    }

    /// 两个账号各自用了同一个设备 id,互不覆盖。
    ///
    /// id 是设备自报的(主机名加进程号),两个人的机器重名一点都不稀奇。
    #[test]
    fn the_same_device_id_in_two_accounts_is_two_devices() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("笔记本"), "爱丽丝的");

        roster.join(BOB, device("笔记本"), "鲍勃的");

        assert_eq!(
            roster.sink(ALICE, "笔记本"),
            Some(&"爱丽丝的"),
            "另一个账号入册不该顶掉这一条"
        );
        assert_eq!(
            roster.sink(BOB, "笔记本"),
            Some(&"鲍勃的")
        );
    }

    /// 旧连接晚一步收工时,不许把顶替它的新连接从名册里带走。
    ///
    /// 这是重连最常见的时序:新连接已经入册,旧连接的清理才跑起来。
    /// 不看代次的话,设备自己以为在线,别人却怎么也找不到它。
    #[test]
    fn a_stale_leave_does_not_evict_the_new_connection() {
        let mut roster = Roster::default();
        let (old, _) =
            roster.join(ALICE, device("a"), "旧出口");
        roster.join(ALICE, device("a"), "新出口");

        roster.leave(ALICE, "a", old);

        assert_eq!(
            roster.sink(ALICE, "a"),
            Some(&"新出口"),
            "旧连接的清理把新连接删掉了"
        );
    }

    /// 广播只覆盖同账号的出口。
    #[test]
    fn broadcast_reaches_only_the_same_account() {
        let mut roster = Roster::default();
        roster.join(ALICE, device("a"), "出口a");
        roster.join(ALICE, device("a2"), "出口a2");
        roster.join(BOB, device("b"), "出口b");

        let mut reached: Vec<&&str> =
            roster.sinks(ALICE).collect();
        reached.sort_unstable();

        assert_eq!(reached, vec![&"出口a", &"出口a2"]);
    }
}
