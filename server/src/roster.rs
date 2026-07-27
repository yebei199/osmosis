//! 在线设备名册,以及信令的投递去向。
//!
//! 名册**就是**当前活跃连接的集合 —— 没有设备表、没有落盘、没有"曾经见过"这个状态
//! (`docs/adr/0009`)。因此这里只有一张内存里的表,进程重启即清空,这是对的。
//!
//! 单独成模块是为了让它离开 WebSocket 被测:连接的生命周期难以在单测里摆布,
//! 而"谁在线、消息该给谁"这两件事是纯逻辑,恰恰也是会出错的地方。

use std::collections::HashMap;

use contract::DeviceDto;

/// 一台设备的出口:往它的连接里塞消息用的发送端。
///
/// 泛型而非写死 `mpsc::Sender`:测试里塞一个记录用的假出口,就能验证
/// "只发给目标那一台"这类断言,不必真的建连接。
pub struct Roster<Sink> {
    /// 设备 id → (设备信息, 出口)。
    entries: HashMap<String, (DeviceDto, Sink)>,
}

impl<Sink> Default for Roster<Sink> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<Sink> Roster<Sink> {
    /// 设备上线。同 id 已在册时**替换**旧条目并返回它的出口。
    ///
    /// 返回旧出口而不是丢弃:调用方得关掉那条僵死的连接,否则它会一直占着资源,
    /// 而且下线时会把新连接从名册里带走。
    pub fn join(
        &mut self,
        device: DeviceDto,
        sink: Sink,
    ) -> Option<Sink> {
        self.entries
            .insert(device.id.clone(), (device, sink))
            .map(|(_, stale)| stale)
    }

    /// 设备下线。
    pub fn leave(&mut self, device_id: &str) {
        self.entries.remove(device_id);
    }

    /// 当前在线的全部设备。
    ///
    /// 按 id 排序:`HashMap` 的遍历顺序每次都不同,不排的话名册会无故重排,
    /// 客户端列表就会自己跳来跳去。
    pub fn devices(&self) -> Vec<DeviceDto> {
        let mut devices: Vec<DeviceDto> = self
            .entries
            .values()
            .map(|(device, _)| device.clone())
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices
    }

    /// 取某台设备的出口,不在线则 `None`。
    pub fn sink(&self, device_id: &str) -> Option<&Sink> {
        self.entries.get(device_id).map(|(_, sink)| sink)
    }

    /// 全部出口,用于广播名册变化。
    pub fn sinks(&self) -> impl Iterator<Item = &Sink> {
        self.entries.values().map(|(_, sink)| sink)
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

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

        assert!(
            roster.join(device("a"), "出口a").is_none()
        );

        assert_eq!(roster.devices(), vec![device("a")]);
    }

    /// 断开即消失。在线没有别的含义 —— 不存在"离线但记着"的状态。
    #[test]
    fn leaving_removes_device() {
        let mut roster = Roster::default();
        roster.join(device("a"), "出口a");

        roster.leave("a");

        assert!(roster.devices().is_empty());
        assert!(roster.sink("a").is_none());
    }

    /// 断线重连时旧连接可能还没被清理,同一个 id 不能在名册里出现两次。
    ///
    /// 出现两次的后果不是显示重复那么轻:第二条的出口是死的,
    /// 主控会挑到它、把 offer 发进一条没人读的连接,然后一直等应答。
    #[test]
    fn rejoin_replaces_stale_entry() {
        let mut roster = Roster::default();
        roster.join(device("a"), "旧出口");

        let stale = roster.join(device("a"), "新出口");

        assert_eq!(
            stale,
            Some("旧出口"),
            "应把旧出口交还给调用方去关掉"
        );
        assert_eq!(roster.devices().len(), 1);
        assert_eq!(roster.sink("a"), Some(&"新出口"));
    }

    /// 一台设备也没有时是空列表,不是错误。
    #[test]
    fn empty_roster_when_alone() {
        let roster: Roster<&str> = Roster::default();

        assert!(roster.devices().is_empty());
    }

    /// 信令只送给目标那一台。
    ///
    /// 广播出去的话,每台设备都会收到一份不是给自己的 offer,
    /// 于是每台都以为自己被邀请入会 —— 而这不会报任何错。
    #[test]
    fn signal_routes_only_to_target() {
        let mut roster = Roster::default();
        roster.join(device("a"), "出口a");
        roster.join(device("b"), "出口b");

        assert_eq!(roster.sink("b"), Some(&"出口b"));
        assert_ne!(roster.sink("b"), roster.sink("a"));
    }

    /// 目标不在线时明确地没有出口,由调用方回一条错误 —— 不能静默当作送到了。
    #[test]
    fn signal_to_unknown_device_reports_error() {
        let mut roster = Roster::default();
        roster.join(device("a"), "出口a");

        assert!(roster.sink("不存在").is_none());
    }
}
