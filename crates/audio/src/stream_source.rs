//! 一条通道喂出来的音频源:听众侧用它把收到的 PCM 交给 rodio。
//!
//! 与本 crate 其余部分相反 —— 那些是「拉」(rodio 向解码器要采样),
//! 这里是「推」(网络什么时候给,就什么时候有)。两者的落差正是本模块存在的理由。

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::source::SeekError;
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

/// 一次跳转请求:跳到哪、跳完之后往**哪条新通道**送采样、裁决往哪儿回。
///
/// 带着一条新通道是这件事的另一半。缓冲里躺着最多 5 秒的旧采样,跳完还得先
/// 听完它们的话,声音会在跳过去之后又倒回来放一遍那 5 秒。换通道等于一次
/// 倒干净,而挨个丢要在两条线程之间约定"丢到哪一个为止"。
///
/// 裁决通道见 [`SEEK_VERDICT_WINDOW`]。
type SeekRequest = (
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

/// 跳转走到哪一步了。
///
/// 跳转是**异步**的:请求送出去就返回,真正的读字节发生在解码线程上,可能
/// 要好几秒(跳到还没下到的位置要重开一个 range 请求)。所以"成没成"不能
/// 由 `try_seek` 的返回值回答 —— 那时还没有答案。界面改为定期问这里。
#[derive(Clone, Default)]
pub struct SeekState(Arc<Mutex<Phase>>);

/// [`SeekState`] 的三种样子。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// 没有跳转在路上。
    #[default]
    Idle,
    /// 请求已经送出,解码线程还在取字节。
    Seeking,
    /// 上一次跳转失败了,附带原因。取走即清 —— 一句提示说一次就够。
    Failed(String),
}

impl SeekState {
    /// 还在取字节吗。界面据此显示「缓冲中」。
    fn begin(&self) {
        self.set(Phase::Seeking);
    }

    fn finish(&self) {
        self.set(Phase::Idle);
    }

    fn fail(&self, why: String) {
        self.set(Phase::Failed(why));
    }

    fn set(&self, phase: Phase) {
        // 锁只护一个枚举,中毒了也没有半截状态可言,取回去接着用
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) =
            phase;
    }

    /// 还在等字节吗。界面据此显示「缓冲中」。
    pub fn is_seeking(&self) -> bool {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
            == Phase::Seeking
    }

    /// 取走上一次失败的原因,取走即清。
    ///
    /// 清掉是必须的:界面每秒问一次,不清的话那句"这首跳不了"会一直重贴,
    /// 把后面真正该说的话盖住。
    pub fn take_failure(&self) -> Option<String> {
        let mut phase = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Phase::Failed(why) = &*phase else {
            return None;
        };
        let why = why.clone();
        *phase = Phase::Idle;
        Some(why)
    }
}

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

/// 执行一次跳转,交回此后该用的通道。
///
/// 失败不结束这条线程:跳不了的格式照样能从当前位置接着放,
/// 把歌掐掉是比"跳不动"更糟的答复。
///
/// 结论走**两条路中的一条,不是两条**:裁决送得进去说明调用方还在等,它自己
/// 会报;送不进去说明它已经超时走了,那才留在 [`SeekState`] 上等界面来取。
/// 两条都走的话,同一次失败会被说两遍 —— 一遍来自拖动那一下,一遍来自一秒后
/// 的轮询。
fn apply_seek<S: Source>(
    source: &mut S,
    (to, fresh, verdict): SeekRequest,
    state: &SeekState,
) -> mpsc::SyncSender<Sample> {
    let outcome = seek_with_retry(source, to);

    if let Err(err) = &outcome {
        // 摊开整条因果链:最外面那句往往是「解码器报错了」,等于没说
        log::warn!(
            "跳转到 {to:?} 失败: {}",
            crate::full_cause(err)
        );
    }

    let why = outcome
        .as_ref()
        .err()
        .map(|err| crate::full_cause(err));

    // 先落状态,再把裁决交出去。反过来的话,调用方一拿到答复就会去问
    // `is_seeking`,而那时这条线程还没走到下面 —— 谁先跑到纯看调度,
    // 开发机上从没输过,两核的 CI runner 上输了。
    state.finish();

    // send 失败 = 接收端已经丢了 = 调用方超时走了,它那句失败没人听见,
    // 于是留在状态上等界面来取。送进去了就不再说第二遍(见上面的注释)。
    //
    // 这中间有一小段状态是 Idle 而非 Failed。界面一秒问一次,撞进这几微秒
    // 只是晚一秒看到那句话,而失败本身留在状态上,不会丢。
    if verdict.send(outcome).is_err()
        && let Some(why) = why
    {
        state.fail(why);
    }

    fresh
}

/// 跳到 `to`;落点跳不动就往前挪 [`SEEK_RETRY_BACKOFF`] 再试一次。
///
/// 重试成功后**向前解码丢弃**到 `to`,位置因此停在调用方要的那一刻 ——
/// rodio 的 `TrackPosition` 拿到 `Ok` 就把位置设成目标值,落点若真在目标
/// 之前,进度条会一直偏着直到下次跳转,歌词跟着一起偏。丢掉的这一段顺带
/// 把比特池填满,正是回退想要的东西。
fn seek_with_retry<S: Source>(
    source: &mut S,
    to: Duration,
) -> Result<(), SeekError> {
    let stuck = match source.try_seek(to) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };

    let earlier = to.saturating_sub(SEEK_RETRY_BACKOFF);
    // 已经在开头了,退不出一个新的落点来
    if earlier == to {
        return Err(stuck);
    }

    log::warn!(
        "跳转到 {to:?} 落点解不开({}),回退到 {earlier:?} 重试",
        crate::full_cause(&stuck)
    );
    source.try_seek(earlier)?;
    discard(source, to - earlier);
    Ok(())
}

/// 向前解码并丢弃 `span` 这么长的采样。
///
/// 采样率与声道数取本模块的常量而不去问 `source`:[`buffered`] 的前提就是
/// 它跑在 [`crate::codec::normalize`] 之后,那一层的全部职责就是把这两样
/// 拉成 48kHz 立体声。
fn discard<S: Source>(source: &mut S, span: Duration) {
    let per_second = f64::from(SYNC_SAMPLE_RATE)
        * f64::from(SYNC_CHANNELS);
    let count = (span.as_secs_f64() * per_second) as u64;
    for _ in 0..count {
        if source.next().is_none() {
            return;
        }
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
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 一个位置读得出来的假源:每个采样的值就是"已经跳到了第几秒"。
    ///
    /// 这样"跳成没成"不必去问被测代码自己 —— 直接从放出来的采样里读,
    /// 而那正是真实链路上用户听到的东西。
    struct Marker {
        at: Sample,
        /// 跳不跳得动。要演两种源:能跳的解码器,和跳不了的东西。
        seekable: bool,
        /// 跳一次要多久。真实链路上跳到还没下到的位置要重开一个 range 请求,
        /// 而"缓冲中"这个状态只在那段时间里存在 —— 不让假源慢下来,
        /// 就没有那一段可看。
        delay: Duration,
    }

    impl Iterator for Marker {
        type Item = Sample;

        fn next(&mut self) -> Option<Sample> {
            Some(self.at)
        }
    }

    impl Source for Marker {
        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> ChannelCount {
            ChannelCount::new(SYNC_CHANNELS).expect("非零")
        }

        fn sample_rate(&self) -> SampleRate {
            SampleRate::new(SYNC_SAMPLE_RATE).expect("非零")
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }

        fn try_seek(
            &mut self,
            pos: Duration,
        ) -> Result<(), SeekError> {
            std::thread::sleep(self.delay);
            if !self.seekable {
                return Err(SeekError::NotSupported {
                    underlying_source: "Marker",
                });
            }
            self.at = pos.as_secs_f32();
            Ok(())
        }
    }

    fn marker(at: Sample, seekable: bool) -> Marker {
        Marker {
            at,
            seekable,
            delay: Duration::ZERO,
        }
    }

    /// 等解码线程给出跳转的结论。取走即清,所以只能问到一次。
    fn wait_for_failure(
        state: &SeekState,
    ) -> Option<String> {
        let deadline =
            std::time::Instant::now() + SEEK_DEADLINE;
        while std::time::Instant::now() < deadline {
            if let Some(why) = state.take_failure() {
                return Some(why);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// 跳转后多久必须听到新位置的声音。
    ///
    /// 给得远大于一次线程唤醒,好让判据不受调度抖动左右;真的坏了也不会
    /// 让这条测试挂在那里。
    const SEEK_DEADLINE: Duration = Duration::from_secs(2);

    /// 一直取采样,直到读到 `want` 或者超时,返回取了多少个。
    ///
    /// 轮询而不是睡一觉:解码线程什么时候跟上是调度说了算,
    /// 固定的睡眠要么白等,要么在忙机器上假红。
    fn pull_until(
        source: &mut ChannelSource,
        want: Sample,
    ) -> Option<usize> {
        let deadline =
            std::time::Instant::now() + SEEK_DEADLINE;
        let mut pulled = 0;
        while std::time::Instant::now() < deadline {
            pulled += 1;
            if source.next() == Some(want) {
                return Some(pulled);
            }
        }
        None
    }

    /// 跳转要穿过通道抵达另一头的解码器。
    ///
    /// 这是整条链上唯一认识"第几秒"的地方 —— rodio 手里只有一条采样通道,
    /// 通道不认识时间。
    #[test]
    fn a_seek_reaches_the_source_behind_the_channel() {
        let mut source =
            buffered_with(marker(0.0, true), 8);

        source
            .try_seek(Duration::from_secs(3))
            .expect("能跳的源该收下这次跳转");

        assert!(
            pull_until(&mut source, 3.0).is_some(),
            "跳完该听到第 3 秒的采样"
        );
    }

    /// 缓冲里躺着的旧采样必须丢掉。
    ///
    /// 不丢的话,跳过去之后要先把那最多 5 秒的旧声音放完 ——
    /// 听起来是"拖了进度条,声音过几秒才跟上",而进度条早就跳走了。
    #[test]
    fn samples_queued_before_a_seek_are_thrown_away() {
        /// 攒得足够多,好让"丢了"与"挨个嚼完"两种结果分得开。
        const CAPACITY: usize = 1000;
        /// 跳转之前那些采样的值。刻意不取 0.0 —— 那是欠数据时补的静音,
        /// 两者同值的话,"嚼旧货"与"等新货"在断言里长得一模一样。
        const STALE: Sample = 0.5;
        /// 跳到第几秒。[`Marker`] 跳完之后放的采样就是这个数。
        const TARGET: u64 = 7;

        let mut source =
            buffered_with(marker(STALE, true), CAPACITY);
        // 先让解码线程把缓冲灌满旧采样,不然没有对照
        std::thread::sleep(Duration::from_millis(50));

        source
            .try_seek(Duration::from_secs(TARGET))
            .expect("能跳的源该收下这次跳转");

        let deadline =
            std::time::Instant::now() + SEEK_DEADLINE;
        let mut stale = 0;
        let mut arrived = false;
        while std::time::Instant::now() < deadline {
            let Some(sample) = source.next() else { break };
            if sample == TARGET as Sample {
                arrived = true;
                break;
            }
            if sample == STALE {
                stale += 1;
            }
        }

        assert!(arrived, "跳完该听到第 {TARGET} 秒的采样");
        assert_eq!(
            stale, 0,
            "跳转之前攒下的采样一个都不该再放出来"
        );
    }

    /// **跳不动是当场就知道的事,当场说。**
    ///
    /// `ForwardOnly` 之类的失败不读网络,微秒级就返回。裁决窗口内等得到它,
    /// 于是 `try_seek` 如实返回 `Err` —— 这一条是「进度条说谎」的解药:
    /// rodio 的 `TrackPosition` 只在 `Ok` 时才把位置挪过去,返回 `Err`
    /// 它就根本不动,界面上的数字与声音因此始终对得上。
    #[test]
    fn a_fast_failure_comes_back_as_an_error() {
        let mut source =
            buffered_with(marker(0.25, false), 8);

        let err = source
            .try_seek(Duration::from_secs(3))
            .expect_err("跳不动该在裁决窗口内如实报回来");

        assert!(
            matches!(err, SeekError::NotSupported { .. }),
            "该原样转出解码器那句话,实际 {err}"
        );
        assert!(
            !source.seek_state().is_seeking(),
            "裁决已经交给调用方了,不该再挂着「在跳」"
        );
    }

    /// **真在取字节的那种慢,不能把声卡回调拖下水。**
    ///
    /// 裁决等不到就乐观放行:那时位置确实会先跳过去,但那是**对的** ——
    /// 它真的在往那儿去。结论随后由 [`SeekState`] 补上。窗口必须有界,
    /// `try_seek` 是在声卡回调里被调的。
    #[test]
    fn a_slow_seek_is_let_through_without_blocking() {
        /// 取字节要多久。必须远大于 [`SEEK_VERDICT_WINDOW`],
        /// 不然量不出"没等满"这件事。
        const FETCH: Duration = Duration::from_millis(400);

        let mut source = buffered_with(
            Marker {
                at: 0.0,
                seekable: true,
                delay: FETCH,
            },
            8,
        );

        let started = std::time::Instant::now();
        source
            .try_seek(Duration::from_secs(3))
            .expect("等不到裁决就该乐观放行");
        let waited = started.elapsed();

        assert!(
            waited < FETCH,
            "这一等花在声卡回调里,不能等满整个取字节的时间(实际 {waited:?})"
        );
        assert!(
            source.seek_state().is_seeking(),
            "放行之后仍在跳,结论得留给 SeekState"
        );
        assert!(
            pull_until(&mut source, 3.0).is_some(),
            "字节取回来之后该听到新位置"
        );
    }

    /// 调用方没等到的那条裁决,要留在 [`SeekState`] 上。
    ///
    /// 慢 + 失败是最难说清的一种:`try_seek` 那时已经乐观返回了,
    /// 没有这条路的话,失败就彻底沉默 —— 界面会一直挂着「缓冲中」。
    #[test]
    fn a_failure_the_caller_missed_is_left_on_the_state() {
        let mut source = buffered_with(
            Marker {
                at: 0.25,
                seekable: false,
                delay: Duration::from_millis(400),
            },
            8,
        );
        let state = source.seek_state();

        source
            .try_seek(Duration::from_secs(3))
            .expect("裁决还没出来,这一下该先放行");

        let why = wait_for_failure(&state)
            .expect("没人接住的失败该留在状态上");
        assert!(
            why.contains("not supported"),
            "该说清是跳不了,实际 {why}"
        );
    }

    /// 跳不动的源不能把歌掐掉。
    ///
    /// 为一次跳不动就结束这一首,比跳不动本身更糟。
    #[test]
    fn a_source_that_cannot_seek_says_so_and_keeps_playing()
    {
        /// 一个不会与静音(0.0)混淆的采样值,好认出源还在往下放。
        const STILL_PLAYING: Sample = 0.25;

        let mut source =
            buffered_with(marker(STILL_PLAYING, false), 8);

        // 跳不动这件事本身由上面那条测,这里只管它有没有把歌掐掉
        let _ = source.try_seek(Duration::from_secs(3));

        assert!(
            pull_until(&mut source, STILL_PLAYING)
                .is_some(),
            "跳不动不该把这一首掐掉"
        );
    }

    /// 一秒有多少个交错采样。位置用**采样序号**存而不是拿 `Duration` 累加:
    /// 浮点走上 96000 步之后就对不上整秒了,而断言要的正是整秒。
    const SAMPLES_PER_SECOND: u64 =
        SYNC_SAMPLE_RATE as u64 * SYNC_CHANNELS as u64;

    /// 一条**会走**的假带子:每取一个采样,位置就前进一个采样的时长。
    ///
    /// [`Marker`] 跳完之后永远吐同一个数,而"向前解码丢弃到目标点"这件事
    /// 只有在位置会走的源上才看得见 —— 丢掉的那些采样,正是让位置从落点
    /// 走到目标点的东西。放出来的采样值就是"现在是第几秒"。
    ///
    /// `stuck_after` 演的是 mp3 的比特池:落在那一刻的那一帧要用前一帧留下的
    /// 字节,解码器当场报错;往前挪一点再跳就没事了。
    struct Tape {
        /// 当前位置,交错采样序号。
        at: u64,
        /// 跳到**不早于**这个时刻就失败。`None` = 哪儿都跳得动。
        stuck_after: Option<Duration>,
        /// `try_seek` 被调了几次。用来证明"只重试一次"。
        attempts: Arc<Mutex<usize>>,
    }

    impl Iterator for Tape {
        type Item = Sample;

        fn next(&mut self) -> Option<Sample> {
            let now =
                self.at as f64 / SAMPLES_PER_SECOND as f64;
            self.at += 1;
            Some(now as Sample)
        }
    }

    impl Source for Tape {
        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> ChannelCount {
            ChannelCount::new(SYNC_CHANNELS).expect("非零")
        }

        fn sample_rate(&self) -> SampleRate {
            SampleRate::new(SYNC_SAMPLE_RATE).expect("非零")
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }

        fn try_seek(
            &mut self,
            pos: Duration,
        ) -> Result<(), SeekError> {
            *self
                .attempts
                .lock()
                .unwrap_or_else(|e| e.into_inner()) += 1;
            if self
                .stuck_after
                .is_some_and(|edge| pos >= edge)
            {
                return Err(SeekError::NotSupported {
                    underlying_source: "Tape(比特池接不上)",
                });
            }
            self.at = (pos.as_secs_f64()
                * SAMPLES_PER_SECOND as f64)
                .round() as u64;
            Ok(())
        }
    }

    /// 建一条带子,并把它的尝试计数器一并交出来。
    fn tape(
        stuck_after: Option<Duration>,
    ) -> (Tape, Arc<Mutex<usize>>) {
        let attempts = Arc::new(Mutex::new(0));
        (
            Tape {
                at: 0,
                stuck_after,
                attempts: Arc::clone(&attempts),
            },
            attempts,
        )
    }

    /// 取到第一个不是静音的采样 —— 也就是跳完之后真正交出来的第一份声音。
    ///
    /// 静音是通道暂时空时垫的,不代表位置。带子上第 0 个采样恰好也是 0.0,
    /// 但那一个永远落在丢弃的那一段里,到不了这儿。
    fn first_real_sample(
        source: &mut ChannelSource,
    ) -> Option<Sample> {
        let deadline =
            std::time::Instant::now() + SEEK_DEADLINE;
        while std::time::Instant::now() < deadline {
            let sample = source.next()?;
            if sample != 0.0 {
                return Some(sample);
            }
        }
        None
    }

    /// 数一下 `try_seek` 被调了几次。
    fn attempt_count(counter: &Arc<Mutex<usize>>) -> usize {
        *counter.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **落点跳不动就往前挪一秒再试一次。**
    ///
    /// mp3 的比特池接不上时,落在那一帧的解码必然失败,而往前一两帧就没事 ——
    /// 一秒是它的四十倍余量。不重试的话用户看到的是"这首歌回不去",
    /// 而它其实只差几十毫秒。
    #[test]
    fn a_stuck_seek_is_retried_one_second_earlier() {
        /// 用户拖到的时刻。
        const TARGET: Duration = Duration::from_secs(30);
        /// 卡住的边界:目标那一刻跳不动,往前一秒(第 29 秒)跳得动。
        const STUCK_AFTER: Duration =
            Duration::from_millis(29_500);

        let (tape, attempts) = tape(Some(STUCK_AFTER));
        let mut source = buffered_with(tape, 8);

        source
            .try_seek(TARGET)
            .expect("回退重试之后该跳成");

        assert!(
            pull_until(&mut source, 30.0).is_some(),
            "重试之后该听到第 30 秒的采样"
        );
        assert_eq!(
            attempt_count(&attempts),
            2,
            "该是原地一次、回退一秒一次"
        );
    }

    /// **重试成功之后,位置要落在用户要的那一刻,不是往前挪的那一刻。**
    ///
    /// rodio 的 `TrackPosition` 拿到 `Ok` 就把位置设成**目标值**。落点若真在
    /// 目标前一秒,进度条会一直偏一秒直到下次跳转,歌词跟着一起偏。
    /// 所以回退之后要向前解码丢弃到目标点 —— 那 1 秒也正好把比特池填满。
    #[test]
    fn the_retry_discards_forward_to_the_requested_position()
     {
        const TARGET: Duration = Duration::from_secs(30);
        const STUCK_AFTER: Duration =
            Duration::from_millis(29_500);

        let (tape, _) = tape(Some(STUCK_AFTER));
        let mut source = buffered_with(tape, 8);

        source
            .try_seek(TARGET)
            .expect("回退重试之后该跳成");

        let first = first_real_sample(&mut source)
            .expect("跳完该有声音");
        assert_eq!(
            first, 30.0,
            "落点在第 29 秒,但交出去的第一个采样必须已经走到第 30 秒"
        );
    }

    /// **只重试一次,失败就如实说。**
    ///
    /// 回退一秒还跳不动,就不是比特池的事了(格式不支持、这条流只进不退),
    /// 再往前退也一样失败,白白多花一次取字节。
    #[test]
    fn a_seek_that_fails_twice_is_reported_and_not_retried_again()
     {
        // 哪儿都跳不动 —— 回退一秒改变不了任何事
        let (tape, attempts) = tape(Some(Duration::ZERO));
        let mut source = buffered_with(tape, 8);

        let err = source
            .try_seek(Duration::from_secs(30))
            .expect_err("两次都跳不动就该如实报回来");

        assert!(
            matches!(err, SeekError::NotSupported { .. }),
            "该原样转出解码器那句话,实际 {err}"
        );
        assert_eq!(
            attempt_count(&attempts),
            2,
            "回退一次就够,不该没完没了地往前退"
        );
    }

    /// **边界:目标不足一秒时,回退落在 0,不下溢。**
    ///
    /// `Duration` 的减法会 panic,而歌的开头恰恰是最常被拖到的地方之一。
    #[test]
    fn a_retry_near_the_start_clamps_to_zero() {
        /// 拖到开头附近。回退一秒会越过 0。
        const TARGET: Duration = Duration::from_millis(500);
        const STUCK_AFTER: Duration =
            Duration::from_millis(400);

        let (tape, attempts) = tape(Some(STUCK_AFTER));
        let mut source = buffered_with(tape, 8);

        source
            .try_seek(TARGET)
            .expect("回退到 0 之后该跳成");

        let first = first_real_sample(&mut source)
            .expect("跳完该有声音");
        assert_eq!(
            first, 0.5,
            "落点被夹到 0,再向前丢弃到第 0.5 秒"
        );
        assert_eq!(attempt_count(&attempts), 2);
    }

    /// 听同播时没有解码器可跳,`try_seek` 必须报错而不是假装跳了。
    ///
    /// 假装跳了的话进度条会跳走,而声音还在原地 —— 两个说法对不上。
    #[test]
    fn a_listener_channel_cannot_be_seeked() {
        let (_tx, rx) = mpsc::channel::<Sample>();
        let mut source = ChannelSource::new(rx);

        let err = source
            .try_seek(Duration::from_secs(3))
            .expect_err("没有解码器就跳不了");

        assert!(
            matches!(err, SeekError::NotSupported { .. }),
            "该如实说不支持,实际 {err}"
        );
    }

    /// 跳转期间对外是"正在跳",跳完回到"没事"。界面靠它显示「缓冲中」。
    #[test]
    fn the_seek_state_reports_while_it_is_fetching() {
        /// 假源取字节要多久。够长到断言跑得完,够短到测试不难受。
        const FETCH: Duration = Duration::from_millis(200);

        let mut source = buffered_with(
            Marker {
                at: 0.0,
                seekable: true,
                delay: FETCH,
            },
            8,
        );
        let state = source.seek_state();
        assert!(!state.is_seeking(), "还没跳,不该说在跳");

        source
            .try_seek(Duration::from_secs(3))
            .expect("能跳的源该收下这次跳转");

        assert!(
            state.is_seeking(),
            "请求送出去之后、字节到位之前,对外就该是「在跳」"
        );
        assert!(pull_until(&mut source, 3.0).is_some());
        assert!(
            !state.is_seeking(),
            "跳完了就不该再说在跳"
        );
    }

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
