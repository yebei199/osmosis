//! 一次同播会话:把信令、名册与若干条 WebRTC 连接串起来。
//!
//! 星型拓扑(`docs/adr/0008`):本机若是主控,就与**每个**听众各建一条连接;
//! 若是听众,就只有与主控的那一条。角色不是配置出来的,是**行为**决定的 ——
//! 谁发起邀请谁就是主控。

use contract::DeviceDto;

/// 会话看得见的在线设备。
///
/// 名册由服务端主动推,这里只是把最新一份存下来给界面读。
#[derive(Debug, Default)]
pub struct Roster {
    /// 本机的设备 id,用来把自己从"可推送的设备"里剔掉。
    own_id: String,
    devices: Vec<DeviceDto>,
}

impl Roster {
    pub fn new(own_id: String) -> Self {
        Self {
            own_id,
            devices: Vec::new(),
        }
    }

    /// 收下服务端推来的一份名册。
    ///
    /// **整批替换**而非合并:服务端每次推的都是完整名册,合并会让下线的设备
    /// 永远留在列表里 —— 而它已经推不了流了。
    pub fn update(&mut self, devices: Vec<DeviceDto>) {
        self.devices = devices
            .into_iter()
            .filter(|device| device.id != self.own_id)
            .collect();
    }

    /// 可以推给谁 —— 即除自己之外的在线设备。
    pub fn others(&self) -> &[DeviceDto] {
        &self.devices
    }
}

/// 一台设备当前在会话里扮演什么。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Role {
    /// 没参与同播。
    #[default]
    Alone,
    /// 正在把声音推给这些设备。
    Host { listeners: Vec<String> },
    /// 正在听这台设备推来的声音。
    Listener { host: String },
}

/// 会话状态:名册 + 本机当前的角色。
///
/// 还不持有连接 —— 编排(谁邀请谁、断线时关掉哪些)尚未落地,
/// 提前放一个没人读的 `peers` 只会让人以为它已经在管了。
pub struct Session {
    roster: Roster,
    role: Role,
}

impl Session {
    pub fn new(own_id: String) -> Self {
        Self {
            roster: Roster::new(own_id),
            role: Role::default(),
        }
    }

    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    pub fn roster_mut(&mut self) -> &mut Roster {
        &mut self.roster
    }

    pub fn role(&self) -> &Role {
        &self.role
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn device(id: &str) -> DeviceDto {
        DeviceDto {
            id: id.to_owned(),
            name: format!("设备 {id}"),
        }
    }

    /// 名册更新反映到会话上。
    #[test]
    fn session_tracks_the_roster() {
        let mut roster = Roster::new("me".to_owned());

        roster.update(vec![device("me"), device("other")]);

        assert_eq!(roster.others(), [device("other")]);
    }

    /// 自己不在"可推送的设备"里。
    ///
    /// 推给自己没有意义,而界面若把它列出来,点下去会得到一条自己连自己的
    /// PeerConnection —— 它甚至能建成功,只是声音绕了一圈回到同一个扬声器。
    #[test]
    fn session_ignores_its_own_device() {
        let mut roster = Roster::new("me".to_owned());

        roster.update(vec![device("me")]);

        assert!(
            roster.others().is_empty(),
            "只有自己在线时没有可推送的设备"
        );
    }
}
