//! 同账号的在线名册:界面上输出设备那一列的来源。

use contract::DeviceDto;

/// 会话看得见的在线设备。
///
/// 名册由服务端主动推,这里只是把最新一份存下来给界面读。
#[derive(Debug, Default)]
pub struct Roster {
    /// 本机的设备 id,用来把自己从可选的输出设备里剔掉。
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
    /// 永远留在列表里 —— 而它已经遥控不了了。
    pub fn update(&mut self, devices: Vec<DeviceDto>) {
        self.devices = devices
            .into_iter()
            .filter(|device| device.id != self.own_id)
            .collect();
    }

    /// 除自己之外的在线设备。
    pub fn others(&self) -> &[DeviceDto] {
        &self.devices
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

    /// 服务端推来的名册原样反映到会话上。
    #[test]
    fn roster_tracks_the_devices_that_are_online() {
        let mut roster = Roster::new("me".to_owned());

        roster.update(vec![device("me"), device("other")]);

        assert_eq!(roster.others(), [device("other")]);
    }

    /// 自己不在可选的输出设备里。
    ///
    /// 「本机」那颗已经常驻在最前面,名册里再列一次自己,同一台设备就被画了两遍;
    /// 点它还会去遥控自己,服务端回 `cannot_control_self`。
    #[test]
    fn roster_ignores_its_own_device() {
        let mut roster = Roster::new("me".to_owned());

        roster.update(vec![device("me")]);

        assert!(
            roster.others().is_empty(),
            "只有自己在线时没有别的设备可选"
        );
    }
}
