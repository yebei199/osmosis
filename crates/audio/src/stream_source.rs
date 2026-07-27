//! 一条通道喂出来的音频源:听众侧用它把收到的 PCM 交给 rodio。
//!
//! 与本 crate 其余部分相反 —— 那些是「拉」(rodio 向解码器要采样),
//! 这里是「推」(网络什么时候给,就什么时候有)。两者的落差正是本模块存在的理由。

use std::sync::mpsc;
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::codec::{SYNC_CHANNELS, SYNC_SAMPLE_RATE};

/// 通道空时,一次返回多少个静音采样再重新查看。
///
/// 太小则空转频繁,太大则真的结束后要多等一会儿。一帧的量是个自然的刻度。
const SILENCE_BURST: usize = 64;

/// 把一条采样通道当作音频源。
pub struct ChannelSource {
    samples: mpsc::Receiver<Sample>,
    /// 还欠多少个静音采样。
    silence: usize,
}

impl ChannelSource {
    pub fn new(samples: mpsc::Receiver<Sample>) -> Self {
        Self {
            samples,
            silence: 0,
        }
    }
}

impl Iterator for ChannelSource {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        // 欠着的静音先补完,再去查通道。这一步让"空了"这件事有个固定的代价,
        // 而不是每个采样都去问一次通道。
        if self.silence > 0 {
            self.silence -= 1;
            return Some(0.0);
        }

        match self.samples.try_recv() {
            Ok(sample) => Some(sample),
            // 暂时没数据:发送端还在,只是网络抖了一下。给静音,**绝不返回 None** ——
            // 那对 rodio 就是"放完了",它会丢掉这个源,之后再来的包全打水漂。
            Err(mpsc::TryRecvError::Empty) => {
                self.silence = SILENCE_BURST - 1;
                Some(0.0)
            }
            // 发送端没了,这才是真的结束。
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
}

impl Source for ChannelSource {
    /// `None` = 参数随时可变。这里其实恒定,但 rodio 用它做分段判断,
    /// 给 `None` 让它每次都重新问,免得缓存住一个过期的段长。
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(SYNC_CHANNELS)
            .expect("声道数是编译期常量,非零")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(SYNC_SAMPLE_RATE)
            .expect("采样率是编译期常量,非零")
    }

    /// 直播没有总时长。
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 通道里的采样原样播出。
    #[test]
    fn channel_source_yields_what_was_sent() {
        let (tx, rx) = mpsc::channel();
        for sample in [0.1, -0.2, 0.3] {
            tx.send(sample).expect("发不进通道");
        }
        drop(tx);

        let played: Vec<Sample> =
            ChannelSource::new(rx).collect();

        assert_eq!(played, vec![0.1, -0.2, 0.3]);
    }

    /// **通道暂时空时给静音,不能结束。**
    ///
    /// 返回 `None` 对 rodio 就是"这首放完了",它会把源丢掉 ——
    /// 于是一次几十毫秒的网络抖动会让听众**永久**没声,而连接一切正常。
    #[test]
    fn channel_source_fills_underrun_with_silence() {
        let (tx, rx) = mpsc::channel();
        tx.send(0.5).expect("发不进通道");
        // 发送端还活着,只是暂时没有新数据 —— 正是抖动时的样子。

        let mut source = ChannelSource::new(rx);

        assert_eq!(source.next(), Some(0.5));
        assert_eq!(
            source.next(),
            Some(0.0),
            "欠数据时该给静音而不是结束"
        );
        assert_eq!(source.next(), Some(0.0));
    }

    /// 发送端关闭才是真的结束。
    #[test]
    fn channel_source_ends_when_channel_closes() {
        let (tx, rx) = mpsc::channel::<Sample>();
        drop(tx);

        let mut source = ChannelSource::new(rx);

        // 关闭时若还欠着静音,先把它补完;补完必须结束。
        let played: Vec<Sample> = source
            .by_ref()
            .take(SILENCE_BURST * 2)
            .collect();
        assert!(
            played.len() < SILENCE_BURST * 2,
            "发送端关闭后不该无限产出静音"
        );
        assert_eq!(source.next(), None);
    }
}
