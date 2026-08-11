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

/// 界面明暗的三档选择。
///
/// 「跟随系统」的检测当前只有桌面端接线(docs/design.md「主题与色板」),
/// 拿不到系统偏好的端按深色处理 —— 深色是这个 app 一直以来的样子。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
}

/// 这台设备上的设置。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    /// 播放音量,0.0 到 1.0。
    pub volume: f32,
    /// 界面明暗的三档选择。
    ///
    /// 与音量同一条理由跟着设备走:同一个账号在办公室的亮屏和床上的暗屏,
    /// 想要的不是同一套。
    pub theme: ThemeMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
            theme: ThemeMode::Dark,
        }
    }
}

/// 落盘格式的原样镜像,只在解析时用。
///
/// 与 `Settings` 分开是为了迁移:琥珀年代存的是 `dark` 布尔,三值枚举
/// 落地后是 `theme` 字符串。两个字段都认,`theme` 优先。
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSettings {
    volume: Option<f32>,
    dark: Option<bool>,
    theme: Option<ThemeMode>,
}

/// 把文本解析成设置。**任何解析不了的东西都退回默认值**。
///
/// 文件是手可以改的,也可能写到一半断电。这里返回 `Result` 的话,
/// 每个调用点都要再决定一次"坏了怎么办",而答案永远是同一个。
pub fn parse(raw: &str) -> Settings {
    let raw: RawSettings =
        serde_json::from_str(raw).unwrap_or_default();

    Settings {
        // 文件里的数也要夹:手改成 5 存进去,rodio 照单全收(见 audio::clamped_volume)
        volume: audio_clamp(
            raw.volume.unwrap_or(DEFAULT_VOLUME),
        ),
        // 新字段优先;没有就翻译琥珀年代的 dark 布尔;都没有落默认(深色)。
        theme: raw.theme.unwrap_or_else(|| {
            match raw.dark {
                Some(false) => ThemeMode::Light,
                _ => ThemeMode::Dark,
            }
        }),
    }
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
        let settings = Settings {
            volume: 0.42,
            ..Settings::default()
        };

        assert_eq!(parse(&render(&settings)), settings);
    }

    /// 明暗的选择也跟着设备走,与音量同一条路。三档都存得住。
    #[test]
    fn the_theme_choice_survives_a_round_trip() {
        for theme in [
            ThemeMode::Dark,
            ThemeMode::Light,
            ThemeMode::System,
        ] {
            let settings = Settings {
                volume: 0.42,
                theme,
            };

            assert_eq!(
                parse(&render(&settings)),
                settings
            );
        }
    }

    /// **边界:琥珀年代的设置文件存的是 `dark` 布尔。**
    ///
    /// 三值枚举落地前,明暗是一位 bool。老文件不该退回默认
    /// (那会冲掉音量),bool 要翻译成对应的那一档。
    #[test]
    fn an_amber_era_dark_bool_becomes_the_matching_mode() {
        assert_eq!(
            parse(r#"{"volume":0.3,"dark":false}"#).theme,
            ThemeMode::Light,
            "存过浅色的该落到浅色档"
        );
        assert_eq!(
            parse(r#"{"volume":0.3,"dark":true}"#).theme,
            ThemeMode::Dark,
            "存过深色的该落到深色档"
        );
    }

    /// **边界:老的设置文件里没有这个字段。**
    ///
    /// 升级上来的用户手里那份 json 只有 volume。少一个字段不该让整份设置退回
    /// 默认(那会把他调好的音量一起冲掉),也不该把界面变成浅色 —— 深色是它
    /// 一直以来的样子。
    #[test]
    fn an_old_settings_file_without_a_theme_stays_dark() {
        // 升级前存下来的那份长这样:只有音量
        let parsed = parse(r#"{"volume":0.3}"#);

        assert_eq!(
            parsed.theme,
            ThemeMode::Dark,
            "没写过明暗就该是深色 —— 那是它一直以来的样子"
        );
        assert!(
            (parsed.volume - 0.3).abs() < f32::EPSILON,
            "少一个字段不该把已经存好的音量一起冲掉"
        );
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
