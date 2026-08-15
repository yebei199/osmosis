//! 一条通道喂出来的音频源:听众侧用它把收到的 PCM 交给 rodio。
//!
//! 与本 crate 其余部分相反 —— 那些是「拉」(rodio 向解码器要采样),
//! 这里是「推」(网络什么时候给,就什么时候有)。两者的落差正是本模块存在的理由。

use std::sync::mpsc;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::codec::{SYNC_CHANNELS, SYNC_SAMPLE_RATE};

#[cfg(test)]
mod fixtures;
mod retry;
mod seek_state;

pub use seek_state::SeekState;

use retry::apply_seek;

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

/// 一次跳转请求:跳到哪、跳完之后往**哪条新通道**送采样、裁决往哪儿回。
///
/// 带着一条新通道是这件事的另一半。缓冲里躺着最多 5 秒的旧采样,跳完还得先
/// 听完它们的话,声音会在跳过去之后又倒回来放一遍那 5 秒。换通道等于一次
/// 倒干净,而挨个丢要在两条线程之间约定"丢到哪一个为止"。
///
/// 裁决通道见 [`SEEK_VERDICT_WINDOW`]。
pub(crate) type SeekRequest = (
    Duration,
    mpsc::SyncSender<Sample>,
    mpsc::Sender<Result<(), SeekError>>,
);

/// 等解码线程的裁决最多等这么久。
///
/// **这是一段花在声卡回调里的时间**([`ChannelSource::try_seek`] 说明了为什么),
/// 所以它有界。取 10ms 的两头理由:
///
/// - 下界:快失败(`ForwardOnly` 这类)根本不读网络,微秒级返回,整条路只是
///   两次线程唤醒。10ms 是它的十倍余量。
/// - 上界:本仓库量到的一次 cpal 回调预算是 21ms(见 `CALLBACK_BUDGET`),
///   取它的一半,最坏也就赔上一块缓冲 —— 而那一刻本来就在跳转,声音本来就断。
///
/// 等得到裁决,`try_seek` 就如实返回,rodio 的 `TrackPosition` 只在 `Ok` 时挪
/// 位置,进度条因此不会显示一个声音没去过的时刻。等不到就乐观放行 —— 那时它
/// 真的在往那儿去,位置先跳过去反而是对的。
const SEEK_VERDICT_WINDOW: Duration =
    Duration::from_millis(10);

/// 落点跳不动时,往前挪这么久再试一次。
///
/// mp3 的一帧可以引用**前一帧**留在比特池(bit reservoir)里的字节,最多回溯
/// 511 字节 —— 约一两帧、几十毫秒。跳转正好落在这样一帧上,解码器当场报
/// `invalid main_data_begin`,而往前挪一点点就没事了。
///
/// 一秒是那几十毫秒的四十倍余量,而**只挪这一次**:一秒还不成就不是比特池
/// 的事了(格式不支持、这条流只进不退),再往前退一样失败,白花一次取字节。
const SEEK_RETRY_BACKOFF: Duration = Duration::from_secs(1);

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
///
/// 收 [`Source`] 而不只收迭代器:跳转要由**这条线程**执行 —— 能跳的解码器
/// 在通道的这一头,而 rodio 手里只有另一头那条通道(见 [`ChannelSource::try_seek`])。
pub fn buffered<S>(source: S) -> ChannelSource
where
    S: Source + Send + 'static,
{
    buffered_with(source, BUFFER_SAMPLES)
}

/// [`buffered`] 的可调版本。
///
/// 容量做成参数只为测试:按 [`BUFFER_SAMPLES`] 那 5 秒,一条测试要真等 5 秒,
/// 而它要验的恰恰是"存货撑住了多久"这件与时长成正比的事。
pub fn buffered_with<S>(
    mut source: S,
    capacity: usize,
) -> ChannelSource
where
    S: Source + Send + 'static,
{
    let (mut tx, rx) = mpsc::sync_channel(capacity);
    let (seek, requests) = mpsc::channel::<SeekRequest>();
    let state = SeekState::default();
    let reported = state.clone();

    std::thread::spawn(move || {
        loop {
            // 跳转比手上这个采样急:抢在取下一个之前看一眼有没有人在等
            if let Ok(request) = requests.try_recv() {
                tx = apply_seek(
                    &mut source,
                    request,
                    &reported,
                );
                continue;
            }

            let Some(sample) = source.next() else {
                return;
            };
            // 缓冲满了就在这儿等 —— 背压落在这条线程上,不落在声卡回调上。
            if tx.send(sample).is_ok() {
                continue;
            }

            // 送不进去有两种可能:接收端换了一条通道(正在跳转),或者它走了
            // (换歌、停止)。阻塞收一次正好把两者分开 —— 前者的请求马上就到,
            // 后者连发送端都一起没了,`recv` 立刻报错,线程就此收工不漏。
            let Ok(request) = requests.recv() else {
                return;
            };
            tx =
                apply_seek(&mut source, request, &reported);
        }
    });

    ChannelSource {
        samples: rx,
        silence: 0,
        capacity,
        seek,
        state,
    }
}

/// 把一条采样通道当作音频源。
pub struct ChannelSource {
    samples: mpsc::Receiver<Sample>,
    /// 还欠多少个静音采样。
    silence: usize,
    /// 换通道时新通道开多大。与原来那条一样,不然跳一次缓冲就缩水一次。
    capacity: usize,
    /// 把跳转请求送去解码线程。听众侧没有解码线程,这条通道生下来就是断的。
    seek: mpsc::Sender<SeekRequest>,
    state: SeekState,
}

impl ChannelSource {
    /// 直接从一条通道建。听众侧用这条路:PCM 是网络推来的,没有解码器可跳。
    pub fn new(samples: mpsc::Receiver<Sample>) -> Self {
        // 接收端当场丢掉,于是 `try_seek` 里那次 send 必然失败 ——
        // 「没有可跳的东西」因此是如实报错,不是靠一个额外的标志位记着。
        let (seek, _) = mpsc::channel();
        Self {
            samples,
            silence: 0,
            capacity: 0,
            seek,
            state: SeekState::default(),
        }
    }

    /// 跳转进行到哪一步了。界面每秒问一次,据此显示「缓冲中」与失败原因。
    pub fn seek_state(&self) -> SeekState {
        self.state.clone()
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

    /// 把跳转转交给通道另一头的解码器。
    ///
    /// 真正的读字节在解码线程上发生,而 rodio 是在**声卡回调**里调这个方法的
    /// —— 在这儿一直等就是让整个混音器停摆,而躲开回调正是 `buffered` 这一层
    /// 存在的全部理由。所以这里等**有界的**一小会儿(见 [`SEEK_VERDICT_WINDOW`]):
    ///
    /// - 裁决及时到:如实返回。跳不动的失败恰恰都是快失败,于是它们全都落在
    ///   这一侧 —— rodio 的 `TrackPosition` 只在 `Ok` 时挪位置,返回 `Err`
    ///   进度条就纹丝不动,屏幕上的时刻与声音始终对得上。
    /// - 等不到:说明它真在取字节。乐观返回 `Ok`,位置先跳过去是**对的**,
    ///   结论随后由 [`ChannelSource::seek_state`] 补上,界面每秒问一次。
    ///
    /// 换一条新通道而不是清空旧的:旧通道里躺着最多 5 秒的采样,而"丢到哪个
    /// 为止"需要两条线程约定一个界碑。换通道让那个界碑变成通道本身。
    fn try_seek(
        &mut self,
        pos: Duration,
    ) -> Result<(), SeekError> {
        let (tx, rx) = mpsc::sync_channel(self.capacity);
        let (verdict, answer) = mpsc::channel();
        // 送不进去 = 那一头没有解码线程(听同播),或者它已经收工了。
        // 这是唯一如实的答复:假装跳了的话进度条会跳走而声音留在原地。
        if self.seek.send((pos, tx, verdict)).is_err() {
            return Err(SeekError::NotSupported {
                underlying_source: "ChannelSource(没有可跳的解码器)",
            });
        }

        self.state.begin();
        // 先换通道再等裁决:解码线程多半正卡在旧通道的 `send` 上,把旧接收端
        // 丢掉才是叫醒它的那一下,不然这一等必然走到超时。
        self.samples = rx;
        // 欠着的静音跟着旧通道一起作废,不然跳完还要先补完它
        self.silence = 0;

        answer
            .recv_timeout(SEEK_VERDICT_WINDOW)
            .unwrap_or(Ok(()))
    }
}

#[cfg(test)]
mod tests;
