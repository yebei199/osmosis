//! 播放器:一个常驻的输出设备,加上音量与位置的读写。

use std::io::{Read, Seek};

use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};

use crate::{AudioError, codec, spectrum};

/// 出声的那一头。持有音频设备,活多久声音就能放多久。
pub struct Player {
    /// 设备句柄。drop 掉声音就断了,所以必须留着。
    _device: MixerDeviceSink,
    player: rodio::Player,
    /// 可视化的频谱分析器,每次换源在 [`Self::play`] 里接上新支路。
    viz: spectrum::Analyzer,
}

impl Player {
    /// 打开默认音频设备。
    pub fn new() -> Result<Self, AudioError> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| {
                AudioError::Device(e.to_string())
            })?;
        let player =
            rodio::Player::connect_new(device.mixer());

        Ok(Self {
            _device: device,
            player,
            viz: spectrum::Analyzer::new(),
        })
    }

    /// 放一路音频,替换掉当前正在放的。
    ///
    /// 收任意 `rodio::Source` 而不只收解码器:听众放的是 [`ChannelSource`] ——
    /// 网络推来的 PCM,从没经过本机的解码器。
    ///
    /// 先清空:队列语义在这里是错的 —— 用户点第二首歌是"改放这首",
    /// 不是"放完上一首再放这首"。
    ///
    /// 这里也是可视化的**统一挖点**:任何要出声的源都从本方法进,分一支采样
    /// 给 [`spectrum::Analyzer`],单机、主控、听众的可视化因此天然一致,
    /// 频谱不进网络(见 `CONTEXT.md`「可视化」)。
    pub fn play<S>(&self, source: S)
    where
        S: rodio::Source + Send + 'static,
    {
        let channels = source.channels().get();
        let (tap, rx) =
            codec::Tee::new(source, spectrum::TAP_CAPACITY);
        self.viz.attach(rx, channels);
        self.player.clear();
        self.player.append(tap);
        self.player.play();
    }

    /// 可视化分析器的共享句柄,UI 侧每帧取频谱/波形用。
    pub fn visualizer(&self) -> spectrum::Analyzer {
        self.viz.clone()
    }

    /// 停止并清空队列。
    pub fn stop(&self) {
        self.player.clear();
    }

    /// 暂停。当前源留在原地,[`Self::resume`] 从暂停处接着放。
    pub fn pause(&self) {
        self.player.pause();
    }

    /// 从暂停处继续。
    ///
    /// 不叫 `play`:那个名字已经被"放一路新源"占了,两个语义挤一个名字,
    /// 调用错了编译器还拦不住。
    pub fn resume(&self) {
        self.player.play();
    }

    /// 当前源放空了没有。控制条靠它区分"暂停中"(false)与"放完了"(true),
    /// 自动续播靠它知道该切下一首了。
    pub fn empty(&self) -> bool {
        self.player.empty()
    }

    /// 已经放到第几秒。
    ///
    /// 这是唯一能从外面看出"真的在出声"的东西:rodio 的输出线程若挂了,
    /// 队列可能仍然非空、`empty()` 仍然为假,但这个位置**不再前进**。
    pub fn position(&self) -> core::time::Duration {
        self.player.get_pos()
    }

    /// 跳到某个时间点。**下场有两种,别只看返回值。**
    ///
    /// `Err`:当场就知道跳不动 —— 格式不支持、这条流只进不退、或者压根没有
    /// 可跳的东西(正在听同播)。这一侧是**确定的**:rodio 的 `TrackPosition`
    /// 只在 `Ok` 时挪位置,所以位置计数器纹丝不动,进度条不会显示一个声音
    /// 没去过的时刻。
    ///
    /// `Ok`:请求收下了。可能已经跳完,也可能解码线程还在取字节 —— 跳到还没
    /// 下到的位置要重开一个 range 请求,那要好几秒(见
    /// [`ChannelSource::try_seek`] 的裁决窗口)。后一种情况的结论要问
    /// [`ChannelSource::seek_state`],那也是界面显示「缓冲中」的来源。
    pub fn seek(
        &self,
        to: core::time::Duration,
    ) -> Result<(), AudioError> {
        self.player.try_seek(to).map_err(|err| {
            AudioError::Device(err.to_string())
        })
    }

    /// 当前音量,0.0 到 1.0。
    pub fn volume(&self) -> f32 {
        self.player.volume()
    }

    /// 调音量。超出范围的值先夹再设(见 [`clamped_volume`])。
    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(clamped_volume(volume));
    }
}

/// 把音量夹进 0.0..=1.0。
///
/// rodio 对越界值照单全收,而后果都不报错:负数是把波形反相 —— 单独听像是
/// "声音变空了",与别的声源混在一起会互相抵消;大于 1 是数字过载,削波失真。
/// 两种都难听,且都不会有任何一行日志说出原因。
///
/// NaN 当静音处理:它的比较全为假,不特判的话会原样传下去。
pub fn clamped_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        return 0.0;
    }

    volume.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests;
