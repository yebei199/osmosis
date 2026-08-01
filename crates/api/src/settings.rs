//! 本地设置:这台设备上的偏好,与账号无关。
//!
//! 音量是第一个 —— 笔记本外放与一副耳机不该共用一个数值,所以它跟着设备走,
//! 不跟着账号同步。
//!
//! 与会话文件同一个目录、同一条规矩:**存不下不能拖垮启动**。设置读不出来
//! 只是回到默认值,那是可用的;为此把应用拦在门外不是。
//!
//! 住在 `api` 里而不是 `app-core`,是因为「文件放哪、wasm 上换成什么」这套
//! 分叉只在这个 crate 里有一份(见 `platform` 模块)。再造一份的代价比
//! 归属稍微不正更大。

use serde::{Deserialize, Serialize};

/// 音量的默认值:满。
///
/// 不取一个"温和"的中间值:第一次打开就比系统里别的应用小声,读起来像坏了。
const DEFAULT_VOLUME: f32 = 1.0;

/// 这台设备上的设置。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct Settings {
    /// 播放音量,0.0 到 1.0。
    pub volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
        }
    }
}

/// 把文本解析成设置。**任何解析不了的东西都退回默认值**。
///
/// 文件是手可以改的,也可能写到一半断电。这里返回 `Result` 的话,
/// 每个调用点都要再决定一次"坏了怎么办",而答案永远是同一个。
pub fn parse(raw: &str) -> Settings {
    let mut parsed: Settings =
        serde_json::from_str(raw).unwrap_or_default();

    // 文件里的数也要夹:手改成 5 存进去,rodio 照单全收(见 audio::clamped_volume)
    parsed.volume = audio_clamp(parsed.volume);
    parsed
}

/// 渲染成要落盘的文本。
pub fn render(settings: &Settings) -> String {
    serde_json::to_string(settings)
        .unwrap_or_else(|_| String::new())
}

/// 夹音量。
///
/// 与 `audio::clamped_volume` 同一条规矩,但这个 crate 不依赖 `audio`
/// (客户端的网络层不该为了一个 clamp 拖进音频栈),故各写一份;
/// 两处都有测试钉着同样的边界。
fn audio_clamp(volume: f32) -> f32 {
    if volume.is_nan() {
        return DEFAULT_VOLUME;
    }

    volume.clamp(0.0, 1.0)
}

/// 读这台设备的设置。读不到、读坏了都给默认值。
pub fn load() -> Settings {
    crate::platform::load_settings()
        .as_deref()
        .map_or_else(Settings::default, parse)
}

/// 存这台设备的设置。失败只记一笔日志。
pub fn save(settings: &Settings) {
    crate::platform::save_settings(&render(settings));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 存进去再读出来是同一份设置。
    #[test]
    fn volume_survives_a_round_trip() {
        let settings = Settings { volume: 0.42 };

        assert_eq!(parse(&render(&settings)), settings);
    }

    /// 没有设置文件时给默认值,不是错误 —— 第一次启动走的就是这条路。
    #[test]
    fn a_missing_settings_file_reads_the_default() {
        assert_eq!(parse(""), Settings::default());
    }

    /// 文件坏了(手改花了、写一半断电)也给默认值。
    ///
    /// 设置存不下不该拖垮启动 —— 与会话文件同一条规矩。
    #[test]
    fn a_corrupt_settings_file_falls_back_to_the_default() {
        for broken in [
            "{not json",
            "null",
            "[]",
            "{\"volume\": \"大声点\"}",
        ] {
            assert_eq!(
                parse(broken),
                Settings::default(),
                "{broken:?} 该退回默认值"
            );
        }
    }

    /// 文件里的音量也要夹:手改成 5 存进去,rodio 照单全收。
    #[test]
    fn a_hand_edited_volume_is_clamped() {
        assert!(
            (parse("{\"volume\": 5}").volume - 1.0).abs()
                < f32::EPSILON
        );
        assert!(
            (parse("{\"volume\": -2}").volume).abs()
                < f32::EPSILON
        );
    }
}
