//! 两条泵:主控把 PCM 送上轨,听众把轨上的包变回 PCM。
//!
//! 它们是同播里唯一持续运转的东西 —— 别处都是一次性的握手。
//!
//! 两条都在**独立线程/任务**里跑,不占用 UI 线程:主控那条要做 Opus 编码
//! (每 20ms 一帧),听众那条要阻塞读 RTP。任何一条挂在 UI 线程上,界面就会跟着卡。

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use audio::codec::{Decoder, Encoder, FRAME_DURATION};
use bytes::Bytes;
use rodio::Sample;
use webrtc::media::Sample as MediaSample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_remote::TrackRemote;

/// 主控侧:把 tee 出来的 PCM 编码成 Opus 帧,写进这条轨。
///
/// `samples` 来自 [`audio::codec::Tee`],它在支路满时**丢采样**而不是阻塞,
/// 所以这条泵慢下来只会让听众少听几帧,不会拖累本机播放。
///
/// 一直跑到通道关闭(本机停止播放)或写轨失败(连接没了)为止。
pub fn spawn_host(
    samples: mpsc::Receiver<Sample>,
    track: Arc<TrackLocalStaticSample>,
) {
    // 阻塞读通道 + CPU 密集的编码,用真线程而非 async 任务:
    // 放进 tokio 的 async 上下文里会占住一整个 worker 不放。
    std::thread::spawn(move || {
        let Ok(mut encoder) = Encoder::new() else {
            return;
        };
        // 一次取一帧的量再编:逐采样调用编码器等于每次都走一遍攒帧逻辑。
        let mut batch = Vec::with_capacity(1024);

        while let Ok(sample) = samples.recv() {
            batch.push(sample);
            if batch.len() < 1024 {
                continue;
            }
            let Ok(frames) = encoder.push(&batch) else {
                return;
            };
            batch.clear();

            for frame in frames {
                let sample = MediaSample {
                    data: Bytes::from(frame),
                    duration: FRAME_DURATION,
                    ..Default::default()
                };
                // write_sample 是 async 的,而这里是普通线程 —— 用一个
                // 当场建起来的单线程 runtime 阻塞等它,别把 async 传染上去。
                if futures_executor::block_on(
                    track.write_sample(&sample),
                )
                .is_err()
                {
                    return;
                }
            }
        }
    });
}

/// 听众侧:从轨上读 RTP,解码成 PCM 送进通道。
///
/// 通道的另一头是 [`audio::ChannelSource`],它在暂时没数据时给静音而非结束,
/// 所以这条泵偶尔跟不上不会让播放停掉。
pub fn spawn_listener(
    track: Arc<TrackRemote>,
    samples: mpsc::SyncSender<Sample>,
) {
    tokio::spawn(async move {
        let Ok(mut decoder) = Decoder::new() else {
            return;
        };

        loop {
            // read_rtp 在连接断开时返回 Err,这就是听众侧感知
            //「主控走了」的地方 —— 会话随之结束(`docs/adr/0008`)。
            let Ok((packet, _)) = track.read_rtp().await
            else {
                return;
            };
            // Opus 的 RTP 载荷就是一整帧,不需要拆包。
            let Ok(pcm) = decoder.decode(&packet.payload)
            else {
                // 单帧坏掉不该终止整条流:丢包在真实网络上是常态。
                continue;
            };
            for sample in pcm {
                // 满了就丢:听众的扬声器读不动时,堆积只会让延迟越拖越长。
                if samples.try_send(sample).is_err()
                    && samples.try_send(sample).is_err()
                {
                    // 两次都失败说明接收端没了(播放停了),收工。
                    return;
                }
            }
        }
    });
}

/// 听众侧通道的容量,以采样计。
///
/// 约 200ms 的量:小于它则轻微抖动就断音,大于它则延迟肉眼可见地增加。
pub const LISTENER_BUFFER: usize = 48_000 * 2 / 5;

/// 主控每隔多久检查一次是否还该继续推。
pub const HOST_TICK: Duration = FRAME_DURATION;
