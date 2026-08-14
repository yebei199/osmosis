//! 音频链路可能的失败方式,以及把整条 source 链摊平成一句话。

use rodio::decoder::DecoderError;

/// 播放链路可能的失败方式。
#[derive(Debug)]
pub enum AudioError {
    /// 打不开音频设备 —— 没声卡,或者被独占了。
    Device(String),
    /// 拉不动这条流:地址不对、连不上、或者服务端拒绝。
    Stream(String),
    /// 拿到了字节,但它不是能放的音频。
    ///
    /// 最常见的真实成因不是"格式冷门",而是直链过期后上游返回了一个 HTML 错误页。
    Decode(String),
}

impl core::fmt::Display for AudioError {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            Self::Device(message) => {
                write!(f, "音频设备错误: {message}")
            }
            Self::Stream(message) => {
                write!(f, "音频流错误: {message}")
            }
            Self::Decode(message) => {
                write!(f, "音频解码错误: {message}")
            }
        }
    }
}

impl core::error::Error for AudioError {}

impl From<DecoderError> for AudioError {
    fn from(error: DecoderError) -> Self {
        Self::Decode(error.to_string())
    }
}

/// 把一条错误链摊成一句话。
///
/// rodio 的 `SeekError::SymphoniaDecoder(_)` 自己那句 Display 是笼统的
/// 「Symphonia decoder returned an error」,真正说明问题的三选一
/// (要精确跳转但没有时基 / 流不可回跳 / 解复用器自己失败)藏在 `source()` 链上。
/// 只 `to_string()` 顶层的话,日志和界面都在说一句等于没说的话 —— 那次
/// 「跳不了」的真实原因是我读 rodio 源码倒推出来的,不是日志告诉我的。
pub fn full_cause(
    error: &dyn core::error::Error,
) -> String {
    let mut text = error.to_string();
    let mut source = error.source();

    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }

    text
}

#[cfg(test)]
mod tests;
