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

/// 缓冲多少个采样。48kHz 立体声下约 5 秒。
///
/// 够盖住常见的网络抖动,而 48 万个 `f32` 不到 2MB —— 比起一首无损几十兆,
/// 这点内存不值得省。
pub const BUFFER_SAMPLES: usize =
    SYNC_SAMPLE_RATE as usize * SYNC_CHANNELS as usize * 5;

/// 把解码挪到自己的线程上,声卡回调那头只管从内存里取。
///
/// **这是「卡住时反复放同一小段」的正解。** rodio 是在 cpal 的回调里直接向
/// 解码器要采样的,而流式解码器读到没下完的位置会阻塞 —— 于是一次网络抖动
/// 就卡在回调里,设备欠载。中间垫一层线程 + 有界通道之后,抖动落在解码线程
/// 上(它等就是了),回调那头照常有存货;真等空了拿到的是静音,声音有个缺口
/// 但节奏没断,数据回来接着放。
///
/// **必须在 [`crate::codec::normalize`] 之后调用**:[`ChannelSource`] 对外
/// 声称的是 48kHz 立体声,喂进来的采样格式不对,放出来就是变调变速的。
pub fn buffered<S>(source: S) -> ChannelSource
where
    S: Iterator<Item = Sample> + Send + 'static,
{
    buffered_with(source, BUFFER_SAMPLES)
}

/// [`buffered`] 的可调版本。
///
/// 容量做成参数只为测试:按 [`BUFFER_SAMPLES`] 那 5 秒,一条测试要真等 5 秒,
/// 而它要验的恰恰是"存货撑住了多久"这件与时长成正比的事。
pub fn buffered_with<S>(
    source: S,
    capacity: usize,
) -> ChannelSource
where
    S: Iterator<Item = Sample> + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(capacity);

    std::thread::spawn(move || {
        for sample in source {
            // 缓冲满了就在这儿等 —— 背压落在这条线程上,不落在声卡回调上。
            // 送不进去说明接收端已经走了(换歌、停止),就此收工,别让线程漏着。
            if tx.send(sample).is_err() {
                return;
            }
        }
    });

    ChannelSource::new(rx)
}

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
