//! 播放器:一个常驻的输出设备,加上音量与位置的读写。
//!
//! 出声的是自己开的声卡流(`crate::output`),每块报出呈现时刻;放的每一路都包一层
//! [`SyncSource`]:位置、跳转、暂停归它,rodio 的 `Player` 只管排队与音量,且始终是
//! 「放着」的(#137 ⑤)。

use std::sync::Arc;
use std::time::Duration;

use crate::sync::{
    Feed, SourceFeed, SyncShared, SyncSource, Target,
};
use crate::{AudioError, output, pcm, spectrum};

/// 等同步源给跳转裁决最多等多久。
///
/// 同步源在下一次被声卡拉的时候执行跳转,一块缓冲十几到二十毫秒;底下的
/// `ChannelSource::try_seek` 自己还要等解码线程最多 10ms。100ms 是两者之和的几倍。
/// 等不到就乐观地当成跳了,结论照旧由 `SeekState` 补上。
/// 暂停久了输出流是关着的(#138),这一回还要先重开流,多半等不到,走的就是乐观这条。
const SEEK_VERDICT: Duration = Duration::from_millis(100);

/// 出声的那一头。持有音频设备,活多久声音就能放多久。
pub struct Player {
    /// 声卡流。drop 掉声音就断了,所以必须留着。暂停久了它自己关流,要出声时靠
    /// [`output::Output::poke`] 叫它马上重开(#138)。
    output: output::Output,
    player: rodio::Player,
    /// 可视化的频谱分析器,每次换源在 [`Self::play`] 里接上新支路。
    viz: spectrum::Analyzer,
    /// 当前这一路的同步状态:时间线、位置、暂停。换源不换它。
    shared: Arc<SyncShared>,
}

impl Player {
    /// 打开默认音频设备。
    pub fn new() -> Result<Self, AudioError> {
        let shared = fresh_shared();
        let output = output::open(shared.clone())?;
        let player =
            rodio::Player::connect_new(&output.mixer);
        player.play();

        Ok(Self {
            output,
            player,
            viz: spectrum::Analyzer::new(),
            shared,
        })
    }

    /// 放一路音频,替换掉当前正在放的。
    ///
    /// 收任意 `rodio::Source` 而不只收解码器:本机放的是 [`crate::buffered`]
    /// 交出的 [`crate::ChannelSource`],解码在另一条线程上。
    ///
    /// 先清空:队列语义在这里是错的 —— 用户点第二首歌是"改放这首",
    /// 不是"放完上一首再放这首"。
    ///
    /// 这里也是可视化的**统一挖点**:任何要出声的源都从本方法进,分一支采样
    /// 给 [`spectrum::Analyzer`],任何来源的可视化因此天然一致,
    /// 频谱不进网络(见 `CONTEXT.md`「可视化」)。
    pub fn play<S>(&self, source: S)
    where
        S: rodio::Source + Send + 'static,
    {
        self.play_feed(SourceFeed(source));
    }

    /// 同 [`Self::play`],但底下是能说出「欠载」的媒体(见 [`Feed`]):
    /// 本机流式播放用 `ChannelSource` 走这里,同步播放时欠载那段不算媒体时间。
    pub fn play_feed<F: Feed>(&self, feed: F) {
        let source =
            SyncSource::new(feed, self.shared.clone());
        self.shared.resume();
        self.attach(source);
        self.output.poke();
    }

    /// 把一路同步源接到播放器上:分一支给可视化,替换掉正在放的。
    fn attach<F: Feed>(&self, source: SyncSource<F>) {
        let channels =
            rodio::Source::channels(&source).get();
        let (tap, rx) =
            pcm::Tee::new(source, spectrum::TAP_CAPACITY);
        self.viz.attach(rx, channels);
        self.player.clear();
        self.player.append(tap);
        self.player.play();
    }

    /// 同 [`Self::play`],但从 `at` 开始;`playing` 为假就停在那里等人按播放。
    ///
    /// 迁移用(#137 ③):目标要从源停下的那一毫秒接着放。先放再跳的话,
    /// 跳转生效之前那几毫秒会从 0:00 响出来 —— 同一首歌的开头,正是「全程不出现
    /// 另一段音频」要排除的那种。所以源是在**暂停着**的时候交进去、跳好,
    /// 再按要求放起来。
    ///
    /// 跳转的下场与 [`Self::seek`] 一样有两种:当场失败原样返回 `Err`(此时
    /// 播放器停在暂停上,不会从 0:00 响起来);乐观的 `Ok` 由调用方照常从
    /// `SeekState` 取结论。
    pub fn play_from<F: Feed>(
        &self,
        feed: F,
        at: core::time::Duration,
        playing: bool,
    ) -> Result<(), AudioError> {
        let source = start_from(
            SyncSource::new(feed, self.shared.clone()),
            &self.shared,
            at,
            playing,
        )?;
        self.attach(source);
        self.output.poke();
        Ok(())
    }

    /// 让当前这一路跟一条时间线(同步播放),或者回到顺序播放([`Target::Free`])。
    pub fn follow(&self, target: Target) {
        self.shared.set_target(target);
        self.output.poke();
    }

    /// 最近一块第一帧的呈现时刻(本机单调时钟纳秒)与那一刻的媒体位置(见 `SyncShared::pairing`)。
    pub fn pairing(
        &self,
    ) -> Option<(i64, core::time::Duration)> {
        self.shared.pairing().map(|(present, media)| {
            (
                present,
                core::time::Duration::from_nanos(
                    media.max(0) as u64,
                ),
            )
        })
    }

    /// 同步源此刻的状况:误差、跳了几次、在不在出声、欠载几次。
    pub fn sync_report(&self) -> crate::sync::Report {
        self.shared.report()
    }

    /// 播放器此刻是不是按着暂停。清空之后也算暂停(rodio 的约定)。
    ///
    /// 播放逻辑问「在不在放」要问这里,不问界面上那个图标 —— 界面是投影,
    /// 播放逻辑回读它就又有了第二份真相(#137 ③)。
    pub fn is_paused(&self) -> bool {
        self.shared.is_paused()
    }

    /// 可视化分析器的共享句柄,UI 侧每帧取频谱/波形用。
    pub fn visualizer(&self) -> spectrum::Analyzer {
        self.viz.clone()
    }

    /// 停止并清空队列。
    pub fn stop(&self) {
        self.player.clear();
        self.shared.pause();
    }

    /// 暂停。当前源留在原地,[`Self::resume`] 从暂停处接着放。
    pub fn pause(&self) {
        self.shared.pause();
    }

    /// 从暂停处继续。
    ///
    /// 不叫 `play`:那个名字已经被"放一路新源"占了,两个语义挤一个名字,
    /// 调用错了编译器还拦不住。
    pub fn resume(&self) {
        self.shared.resume();
        self.player.play();
        self.output.poke();
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
        self.shared.position()
    }

    /// 跳到某个时间点。**下场有两种,别只看返回值。**
    ///
    /// `Err`:当场就知道跳不动 —— 格式不支持、这条流只进不退、或者压根没有
    /// 可跳的东西(解码线程已经收工)。这一侧是**确定的**:rodio 的 `TrackPosition`
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
        let verdict = self.shared.request_seek(to);
        self.output.poke();
        match verdict.recv_timeout(SEEK_VERDICT) {
            Ok(verdict) => verdict.map_err(|err| {
                AudioError::Device(err.to_string())
            }),
            Err(_) => Ok(()),
        }
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

/// 刚开的播放器那一份同步状态：手上还没有源，算暂停(同 [`Player::is_paused`] 的约定)。
/// 不这样的话，应用启动后一首没放，输出流就一直开着送静音，暂停满时限关流那条永远轮不到
/// (#138)。放起来的每条路([`Player::play_feed`]、[`Player::play_from`])都自己定暂停与否。
fn fresh_shared() -> Arc<SyncShared> {
    let shared = SyncShared::new();
    shared.pause();
    shared
}

/// [`Player::play_from`] 的正身,拆出来是为了能不开声卡地测。
///
/// 跳转在同步源交给 rodio **之前**就做完:同步源此刻还在手上,直接调它,不经声卡线程。
/// 所以交出去之后吐的第一帧就是 `at` 那一帧;不放的话按下暂停,一帧都不出声。
fn start_from<F: Feed>(
    mut source: SyncSource<F>,
    shared: &SyncShared,
    at: core::time::Duration,
    playing: bool,
) -> Result<SyncSource<F>, AudioError> {
    rodio::Source::try_seek(&mut source, at).map_err(
        |err| AudioError::Device(err.to_string()),
    )?;
    if playing {
        shared.resume();
    } else {
        shared.pause();
    }
    Ok(source)
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
