//! 音频播放能力层:把一条直链变成声音。
//!
//! 与 `api`、`render3d` 平行 —— `app-core` 不认识本 crate,由 `ui` 注入。
//! 各端音频后端的差异(linux 走 alsa、android 走 AAudio、web 将来走 WebAudio)
//! 到此为止,不向上传播。
//!
//! **边下边播,不整曲下载。** [`load`] 给出的流句柄实现 `Read + Seek`,
//! rodio 读多少就下多少。这不只是为了首播延迟:同播的主控必须边解码边推给听众,
//! 等整首下完再开始推是不能接受的(见 `docs/adr/0008`)。
//!
//! 解码与出声刻意分开:出声需要真实声卡,断言不了;而解码才是真会出故障的地方 ——
//! 直链过期时上游返回的是一个 HTML 页面,不是音频。

pub mod codec;
mod range_stream;
pub mod spectrum;
mod stream_source;

pub use stream_source::{
    BUFFER_SAMPLES, ChannelSource, buffered, buffered_with,
};

use std::io::{Read, Seek};
use std::sync::{Arc, OnceLock};

use rodio::decoder::DecoderError;
use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};
use stream_download::storage::temp::TempStorageProvider;
use stream_download::{Settings, StreamDownload};
use tokio::runtime::Runtime;

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

/// 音频源:一个能读、能跳的字节流。
///
/// 生产环境喂的是 [`load`] 内部开出的流句柄,测试喂的是 `Cursor<Vec<u8>>` ——
/// 两者走**同一条**解码路径,所以测试证明的东西对真实播放也成立。
pub trait Source:
    Read + Seek + Send + Sync + 'static
{
}

impl<T: Read + Seek + Send + Sync + 'static> Source for T {}

/// 一条已经可以直接送进 [`Player`] 的流式音频。
pub type Loaded =
    rodio::Decoder<StreamDownload<TempStorageProvider>>;

/// 后台多线程 tokio runtime,专门跑下载。
///
/// 与 `api` 里那个同构、同理由(`docs/adr/0002`),但**必须是另一个** ——
/// 两个 crate 谁也不依赖谁。
///
/// 多线程是硬要求,不是性能选择:[`load`] 里解码器要**阻塞读**这条流,
/// 而喂它的下载任务跑在同一个 runtime 上。单线程 runtime 里两者互等,
/// 症状是整个调用永久挂起、没有任何报错。
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Runtime::new()
            .expect("failed to start tokio runtime")
    })
}

/// 流的三个旋钮:攒多少才起播、多久没数据算一次失联、失联几次就放弃。
///
/// 做成参数而不是常量,是为了让测试能把它们调到毫秒级 —— 生产值意味着一条
/// 测试要枯等十几秒,那种测试不会有人跑。
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub prefetch_bytes: u64,
    pub retry_timeout: core::time::Duration,
    pub give_up_after: usize,
}

impl Tuning {
    /// 生产值。取值理由见 [`PREFETCH_BYTES`] 与 `docs/adr/0013`。
    pub const PRODUCTION: Self = Self {
        prefetch_bytes: PREFETCH_BYTES,
        // 库默认值。调低会在正常网络下误触发重连,库文档明说了。
        retry_timeout: core::time::Duration::from_secs(5),
        // 两次 ≈ 10 秒。缓冲里有 5 秒存货,所以用户约听到 5 秒沉默。
        give_up_after: 2,
    };
}

/// 一条流有没有已经放弃。
///
/// 断流与放完了在下游长得一模一样 —— 都是采样不再来、源结束。这个句柄是**唯一**
/// 能把两者分开的东西:它由放弃那一刻的同一段代码置位,不是事后猜的
/// (见 `docs/adr/0013`)。
#[derive(Clone, Debug, Default)]
pub struct StreamHealth(
    std::sync::Arc<std::sync::atomic::AtomicBool>,
);

impl StreamHealth {
    /// 这条流是不是已经放弃了。源结束时问它:为真是断流,为假是放完了。
    pub fn gave_up(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// 起播前先攒够多少字节。
///
/// 它保的只有**开头那几秒**:那时 [`buffered`] 的解码缓冲还是空的,来不及垫。
/// 曲中掉速归缓冲管,不归这里。
///
/// **这个数同时是切歌的等待时间。** 它曾被提到 4MB(约合无损半分钟),结果是
/// 点下一首之后歌名、封面立刻换了,声音却要好几秒才跟上 —— 界面那半在
/// `play_current` 一进来就推,而声音要等这一段下完。两倍于库默认值够垫开头,
/// 再往上加就是拿切歌的手感换一份缓冲已经提供的保险。
const PREFETCH_BYTES: u64 = 512 << 10;

/// 把一条直链变成可播放的流式音频:开流 + 解码,全在后台 runtime 上完成。
///
/// 开流与解码不拆成两个公开函数,是因为它们**必须在同一个 runtime 上**跑。
/// 拆开的话调用方很容易在 Slint 的 UI 线程上解码 —— 那里没有 tokio 反应堆,
/// 下载推不动,解码器就一直等,界面停在「加载中」再也不动。
///
/// 落盘到临时文件而不是常驻内存:seek 回已下过的位置(拖进度条、解码器回读
/// 帧头)不必重新请求,而一首无损动辄几十兆,内存里堆着毫无必要。
pub async fn load(
    url: &str,
) -> Result<(Loaded, StreamHealth), AudioError> {
    load_with(url, Tuning::PRODUCTION).await
}

/// [`load`] 的可调版本,给测试用。
///
/// 交回的 [`StreamHealth`] 是这条流的死亡证明:放弃时由 `on_reconnect` 里那段
/// 代码置位。没有它的话,下游只知道"源结束了",分不出是放完还是断了。
async fn load_with(
    url: &str,
    tuning: Tuning,
) -> Result<(Loaded, StreamHealth), AudioError> {
    use std::sync::atomic::{
        AtomicU64, AtomicUsize, Ordering,
    };

    let url = url.to_owned();

    runtime()
        .spawn(async move {
            let parsed = url.parse().map_err(|e| {
                AudioError::Stream(format!("{e}: {url}"))
            })?;

            let health = StreamHealth::default();
            let flag = health.0.clone();
            // 连续失联的次数。**来了数据就清零** —— 不清的话,一首歌里两次相隔
            // 几分钟、各自都缓过来了的短抖动会被算成一次断流,把歌掐掉。
            let misses = Arc::new(AtomicUsize::new(0));
            let recovered = misses.clone();
            // 最近一次收到数据的位置,只为出事时的那行日志。
            let reached = Arc::new(AtomicU64::new(0));
            let advanced = reached.clone();
            let give_up_after = tuning.give_up_after;

            let settings = Settings::default()
                .prefetch_bytes(tuning.prefetch_bytes)
                .retry_timeout(tuning.retry_timeout)
                .on_progress(move |_, state, _| {
                    recovered.store(0, Ordering::Relaxed);
                    advanced.store(
                        state.current_chunk.end,
                        Ordering::Relaxed,
                    );
                })
                // 参数类型得写全:闭包里调 `header` 要求这时就知道流的具体类型,
                // 而它本来要等下面那行 `new::<RangeStream>` 才定下来。
                .on_reconnect(move |stream: &range_stream::RangeStream, token| {
                    let missed = misses
                        .fetch_add(1, Ordering::Relaxed)
                        + 1;
                    // `Accept-Ranges` 记进日志:它缺席过(真机日志里连续四次),
                    // 而那正是 `range_stream` 存在的理由。留着这一行是为了下次
                    // 还能一眼看出流经过了什么 —— 比如中间有没有代理。
                    log::warn!(
                        "音频流失联第 {missed} 次,已到 {} 字节,Accept-Ranges: {:?}",
                        reached.load(Ordering::Relaxed),
                        stream.header("Accept-Ranges"),
                    );
                    if missed >= give_up_after {
                        flag.store(true, Ordering::Relaxed);
                        // 取消让下载任务收尾并置为失败,此后所有 read 立刻报错 ——
                        // 不取消的话它会永远重连下去,读的那一头永远挂着。
                        token.cancel();
                    }
                });

            let stream = StreamDownload::new::<
                range_stream::RangeStream,
            >(
                parsed,
                TempStorageProvider::default(),
                settings,
            )
            .await
            .map_err(|e| {
                AudioError::Stream(e.to_string())
            })?;

            // 解码要阻塞读若干秒(等够探测格式的字节),不能占着 async 线程。
            let decoder = tokio::task::spawn_blocking(
                move || decode(stream),
            )
            .await
            .map_err(|e| AudioError::Stream(e.to_string()))??;

            Ok((decoder, health))
        })
        .await
        .map_err(|e| AudioError::Stream(e.to_string()))?
}

/// 解码一个音频源。失败时不 panic —— 直链过期是常态,不是程序错误。
pub fn decode<R: Source>(
    source: R,
) -> Result<rodio::Decoder<R>, AudioError> {
    Ok(rodio::Decoder::new(source)?)
}

/// 出声的那一头。持有音频设备,活多久声音就能放多久。
pub struct Player {
    /// 设备句柄。drop 掉声音就断了,所以必须留着。
    ///
    /// 它内含一个 `cpal::Stream`,而那东西 **不是 `Send`** —— 整个 `Player`
    /// 因此过不了线程边界,想在别的线程上碰播放器只能走 [`Seeker`]。
    _device: MixerDeviceSink,
    /// `Arc` 而不是裸值:[`Seeker`] 要把它带去别的线程(见那里的说明)。
    player: Arc<rodio::Player>,
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
        let player = Arc::new(rodio::Player::connect_new(
            device.mixer(),
        ));

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

    /// 一个能带去别的线程的跳转句柄。
    ///
    /// 跳转**必然会阻塞**(见 [`Seeker::seek`]),所以它不能长在 `Player` 上:
    /// `Player` 因设备句柄而不是 `Send`,方法一旦挂在它身上,调用方就只能在
    /// 持有它的那个线程 —— 也就是界面线程 —— 上等。
    pub fn seeker(&self) -> Seeker {
        Seeker(self.player.clone())
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

/// 跳转句柄:只带得动"跳到第几秒"这一件事,但**跨得过线程边界**。
///
/// 只装 rodio 的播放器而不装整个 [`Player`],是因为后者还捏着声卡句柄
/// (`cpal::Stream`,不是 `Send`)。分出这一个的理由见 [`Self::seek`]。
#[derive(Clone)]
pub struct Seeker(Arc<rodio::Player>);

impl Seeker {
    /// 跳到某个时间点。**这是一次阻塞调用,不要在界面线程上叫它。**
    ///
    /// 阻塞有两段,叠在一起:rodio 把跳转排给音频线程后,自己在一个 channel 上
    /// 等回执;而音频线程那边真去读字节 —— 底下那条流是 `Read + Seek`,落盘到
    /// 临时文件正是为了这一下能落在已经下过的字节上(见 [`load`]),可跳到
    /// **还没下到**的位置时,`StreamDownload` 会重开一个 range 请求并阻塞到
    /// 数据来为止。一首无损往后拖两分钟,这一等是好几秒。
    ///
    /// 放在界面线程上等的后果不是"卡一下":那期间一帧都画不出来,于是连
    /// "缓冲中"这三个字都送不到屏幕上,看起来就是整个应用死了。
    ///
    /// 有些格式跳不了(rodio 的解码器各自表态),那时返回错误而不是装作跳了。
    pub fn seek(
        &self,
        to: core::time::Duration,
    ) -> Result<(), AudioError> {
        self.0.try_seek(to).map_err(|err| {
            AudioError::Device(err.to_string())
        })
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
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use rodio::Source as _;
    use similar_asserts::assert_eq;

    use super::*;

    /// [`Seeker`] 必须过得了线程边界 —— 整个跳转设计就压在这一条上。
    ///
    /// 断言写成编译期的:哪天有人往 `Seeker` 里塞了不是 `Send` 的东西
    /// (设备句柄就是一个),这里当场编译不过,而不是等到界面那侧发现
    /// 又只能在界面线程上等,而那时症状是"拖进度条整个应用卡死"。
    #[test]
    fn a_seeker_crosses_thread_boundaries() {
        const fn require_send<T: Send>() {}
        require_send::<Seeker>();
    }

    /// 合成一段单声道 44.1kHz 的 WAV,`samples` 个采样点。
    ///
    /// 手搓而不是引 hound:44 字节的头就能让测试独立于任何编码库,
    /// 也让"截断"这种边界能被精确构造。
    fn wav(samples: u32) -> Vec<u8> {
        const SAMPLE_RATE: u32 = 44_100;
        const CHANNELS: u16 = 1;
        const BITS: u16 = 16;

        let data_len = samples * u32::from(BITS / 8);
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(
            &(36 + data_len).to_le_bytes(),
        );
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&CHANNELS.to_le_bytes());
        out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        let byte_rate = SAMPLE_RATE
            * u32::from(CHANNELS)
            * u32::from(BITS / 8);
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(
            &(CHANNELS * BITS / 8).to_le_bytes(),
        );
        out.extend_from_slice(&BITS.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..samples {
            // 一段随便什么波形,只要不是恒零,免得被解码器当成空块跳过。
            let sample = (i % 1000) as i16;
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    /// 一次声卡回调的预算。cpal 在 48kHz 立体声上常见的一块缓冲是 1024 帧,
    /// 约 21ms —— 回调拖过这个数还没填完,设备就欠载。
    const CALLBACK_BUDGET: Duration =
        Duration::from_millis(21);

    /// 假流停摆多久。取得远大于 [`CALLBACK_BUDGET`],
    /// 好让判据不受调度抖动左右。
    const STALL: Duration = Duration::from_millis(300);

    /// 卡点的字节位置。必须大于探测格式时的预读量,否则开头那几次读就把卡点
    /// 吞了,"卡点之前"这个对照组根本不存在。
    const STALL_AFTER: u64 = 400_000;

    /// 取到多少个采样才算真的跨过了卡点。
    ///
    /// 不能按 `STALL_AFTER ÷ 2 字节` 直接算:解码器是按 32KB 成块读的,
    /// 刚好够喂到卡点的那次读**发生在卡点之前**,停摆要到再下一次读才触发。
    /// 多留两块的余量,这条测试才不会因为块大小变了就假绿。
    const PAST_STALL: usize = 300_000;

    /// 对照组取多少个采样:约合 10 万字节,远在卡点之内。
    const BEFORE_STALL: usize = 50_000;

    /// 一块回调要多少帧。cpal 在 48kHz 上的常见块大小,正是
    /// [`CALLBACK_BUDGET`] 那 21ms 的由来。
    const CALLBACK_FRAMES: usize = 1024;

    /// 跨过卡点要取多少块回调。
    ///
    /// 输入是 44.1kHz 单声道,回调那头是 48kHz 立体声,一个输入采样约合
    /// 2.18 个输出采样;[`PAST_STALL`] 个输入采样即约 65 万个输出采样,
    /// 除以每块的 1024×2。取整后再多留几块。
    const BLOCKS_PAST_STALL: usize = 330;

    /// 一条读到 [`STALL_AFTER`] 之后就停摆的字节流:模拟 CDN 掉速或连接卡死。
    ///
    /// 卡点之前照常给字节,之后每次 `read` 先睡一觉再给 —— 这正是
    /// `StreamDownload` 读到没下完的位置时的行为(它是**阻塞**的)。
    ///
    /// 只让 `read` 停摆,`seek` 照常:解码器探测格式时会来回跳,
    /// 那几下不该被算进"网络卡住"里。
    struct StallingSource {
        inner: Cursor<Vec<u8>>,
        /// 卡点的字节位置。按流传而不是取全局常量:两组测试要的卡点深浅不同 ——
        /// 一组要它深到能留出对照区,一组要它浅到实时播放一两秒就能撞上。
        stall_after: u64,
        /// 只停摆**一次**。一次抖动就足以证明因果,每次读都睡只是让测试变慢。
        stalled: bool,
    }

    impl Read for StallingSource {
        fn read(
            &mut self,
            buf: &mut [u8],
        ) -> std::io::Result<usize> {
            if !self.stalled
                && self.inner.position() >= self.stall_after
            {
                self.stalled = true;
                std::thread::sleep(STALL);
            }
            self.inner.read(buf)
        }
    }

    impl Seek for StallingSource {
        fn seek(
            &mut self,
            pos: std::io::SeekFrom,
        ) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// 取 `count` 个采样,返回**单次**取样最久的那一次花了多长。
    ///
    /// 看单次而不是总时长:声卡回调是一次一次被叫醒的,拖垮它的是某一次
    /// 交不出数据,平均值反而会把这件事摊平到看不见。
    fn longest_pull(count: usize) -> Duration {
        // 10 秒的 WAV,足够让卡点落在中间而不是开头。
        let source = StallingSource {
            inner: Cursor::new(wav(441_000)),
            stall_after: STALL_AFTER,
            stalled: false,
        };
        let mut decoder =
            decode(source).expect("合法 WAV 应能解码");

        let mut worst = Duration::ZERO;
        for _ in 0..count {
            let started = std::time::Instant::now();
            if decoder.next().is_none() {
                break;
            }
            worst = worst.max(started.elapsed());
        }
        worst
    }

    /// 音量夹在 0..=1。
    ///
    /// rodio 对越界值照单全收,而后果都不报错:负数是把波形反相,单独听像是
    /// "声音变空了",与别的声源混在一起会互相抵消;大于 1 是数字过载削波。
    /// 两种都难听,且都不会有任何一行日志说出原因。
    #[test]
    fn volume_is_clamped_to_a_sane_range() {
        assert!(
            (clamped_volume(0.5) - 0.5).abs()
                < f32::EPSILON,
            "范围内的值不该被动"
        );
        assert!(
            (clamped_volume(1.7) - 1.0).abs()
                < f32::EPSILON,
            "过载要收到 1.0"
        );
        assert!(
            (clamped_volume(-0.3) - 0.0).abs()
                < f32::EPSILON,
            "负数要收到 0.0,不能留着反相"
        );
        // NaN 的比较全为假,不特判就会原样传给 rodio
        assert!(
            (clamped_volume(f32::NAN) - 0.0).abs()
                < f32::EPSILON,
            "NaN 当静音"
        );
    }

    /// **字节流一卡,取样就跟着卡。**
    ///
    /// 生产环境里调 `next()` 的是 cpal 的声卡回调(rodio-0.22 `stream.rs:527`
    /// 那句 `samples.next()`),所以这里量到的阻塞时长,就是回调交不出数据的
    /// 时长。这条钉死的是欠载的**成因**,不是欠载本身 —— 后者要真声卡。
    #[test]
    fn a_stalled_byte_stream_blocks_the_sample_pull() {
        let worst = longest_pull(PAST_STALL);

        assert!(
            worst >= STALL,
            "跨过卡点的那次取样只花了 {worst:?},没被流阻塞住"
        );
    }

    /// 阻塞多久算出事,得有个数,免得日后靠感觉争论。
    ///
    /// 与上一条量的是同一件事,判据不同:上一条问"是不是被阻塞了",
    /// 这一条问"阻塞得够不够让声卡欠载"。前者是机制,后者是后果。
    #[test]
    fn the_stall_outlasts_one_callback_budget() {
        let worst = longest_pull(PAST_STALL);

        assert!(
            worst > CALLBACK_BUDGET,
            "最久的一次取样 {worst:?} 没超过回调预算 {CALLBACK_BUDGET:?},欠载的推论不成立"
        );
    }

    /// 对照组:卡点之前的取样必须很快回来。
    ///
    /// 没有这条,上面两条也可能只是"这条流从头到尾都慢"造成的,
    /// 证不出"卡在特定位置"与"取样阻塞"之间的因果。
    #[test]
    fn samples_before_the_stall_point_come_back_promptly() {
        let worst = longest_pull(BEFORE_STALL);

        assert!(
            worst < CALLBACK_BUDGET,
            "卡点之前就已经有一次取样花了 {worst:?},这条流本身就慢,对照组不成立"
        );
    }

    /// 按生产的组装方式把一路源接到 Mixer 上,交回声卡回调那一端。
    ///
    /// 与 [`emit`](../../ui/src/music.rs) + [`Player::play`] 同形:
    /// normalize → Tee → `rodio::Player` → `Mixer`。唯一缺的是 OS 设备,
    /// 而它的职责恰好由调用方扮演 —— 按块来取,取不到就欠载。
    ///
    /// `Player` 必须一起交回去:drop 掉它会把播放停掉(rodio `player.rs:345`),
    /// 留在函数里的话回调那头立刻就只剩静音。
    fn callback_end<S>(
        source: S,
    ) -> (rodio::Player, rodio::mixer::MixerSource)
    where
        S: rodio::Source + Send + 'static,
    {
        let (mixer, out) = rodio::mixer::mixer(
            rodio::ChannelCount::new(codec::SYNC_CHANNELS)
                .expect("声道数是编译期常量,非零"),
            rodio::SampleRate::new(codec::SYNC_SAMPLE_RATE)
                .expect("采样率是编译期常量,非零"),
        );
        let player = rodio::Player::connect_new(&mixer);

        let (tee, _branch) = codec::Tee::new(
            codec::normalize(source),
            codec::BRANCH_CAPACITY,
        );
        player.append(tee);
        player.play();

        (player, out)
    }

    /// 扮演 cpal 的回调:一块一块地取,返回**单块**取得最久的那一次。
    ///
    /// `stalling` 决定喂的是会停摆的流还是老实流 —— 两条测试的差别只有这一个,
    /// 别的都得一模一样,否则对照组证不了东西。
    fn longest_callback_block(stalling: bool) -> Duration {
        let bytes = wav(441_000);
        let source = StallingSource {
            inner: Cursor::new(bytes),
            stall_after: STALL_AFTER,
            // 老实流:一上来就当作"已经停摆过了",于是永不睡。
            stalled: !stalling,
        };
        let decoder =
            decode(source).expect("合法 WAV 应能解码");
        let (_player, mut out) = callback_end(decoder);

        let mut worst = Duration::ZERO;
        for _ in 0..BLOCKS_PAST_STALL {
            let started = std::time::Instant::now();
            // Mixer 没源可放时给静音而不是结束,跟真设备一样,所以不必判 None。
            for _ in 0..CALLBACK_FRAMES
                * codec::SYNC_CHANNELS as usize
            {
                out.next();
            }
            worst = worst.max(started.elapsed());
        }
        worst
    }

    /// 一次驱动的结果:最慢的一块花了多久,以及这一路取到的非静音采样数。
    ///
    /// 两个数缺一不可。只看时长的话,一个**什么都不放**的实现也能满分 ——
    /// 静音取得飞快。非静音计数是防止这条测试变成空断言的那一半。
    struct Driven {
        worst: Duration,
        audible: usize,
    }

    /// 扮演 cpal 的回调,并且**按实时节奏**取:每块之间补足到 21ms 再取下一块。
    ///
    /// [`longest_callback_block`] 那种不停地取,消费端等于无限快,任何缓冲都
    /// 来不及填 —— 拿它测缓冲只会得到"缓冲没用"的假结论。真声卡是按节奏来的,
    /// 缓冲能起作用正是因为这个节奏留出了填的时间。
    fn drive_paced<S>(source: S, blocks: usize) -> Driven
    where
        S: rodio::Source + Send + 'static,
    {
        let (_player, mut out) = callback_end(source);

        let mut worst = Duration::ZERO;
        let mut audible = 0;
        for _ in 0..blocks {
            let started = std::time::Instant::now();
            for _ in 0..CALLBACK_FRAMES
                * codec::SYNC_CHANNELS as usize
            {
                if out.next().is_some_and(|s| s != 0.0) {
                    audible += 1;
                }
            }
            let spent = started.elapsed();
            worst = worst.max(spent);
            // 补足这一块的实时时长。真设备就是这样:填完就等下一次被叫醒。
            if let Some(rest) =
                CALLBACK_BUDGET.checked_sub(spent)
            {
                std::thread::sleep(rest);
            }
        }

        Driven { worst, audible }
    }

    /// 按实时节奏播时的卡点。比 [`STALL_AFTER`] 浅得多:44.1kHz 单声道下
    /// 15 万字节约合第 1.7 秒,**不带缓冲**那一路也能在两秒内老老实实播到。
    ///
    /// 深浅在这里是判据的一部分:卡点若深到实时播不到,不带缓冲的那一路会
    /// 因为"根本没撞上"而通过,测试就成了空的(第一版正是这么假绿的)。
    const PACED_STALL_AFTER: u64 = 150_000;

    /// 按实时节奏跑多少块。100 块约合 2.1 秒,足够越过
    /// [`PACED_STALL_AFTER`],又不至于让测试变成秒级的枯等。
    const PACED_BLOCKS: usize = 100;

    /// 一条会在 [`PACED_STALL_AFTER`] 处停摆一次的解码器。
    fn stalling_decoder() -> rodio::Decoder<StallingSource>
    {
        decode(StallingSource {
            inner: Cursor::new(wav(441_000)),
            stall_after: PACED_STALL_AFTER,
            stalled: false,
        })
        .expect("合法 WAV 应能解码")
    }

    /// **阻塞会一路传到声卡回调,中间没有任何一层挡得住。**
    ///
    /// 补的是上面三条的空档:它们只到解码器为止,而生产里解码器与设备之间还
    /// 隔着 `Player`、队列、`Mixer`。这三层里但凡有一层自带缓冲,阻塞就传不
    /// 过去,"网络卡一下就欠载"的推论也就不成立 —— 那是从源码读出来的结论,
    /// 得有东西钉住它。
    #[test]
    fn a_stalled_stream_starves_the_simulated_callback() {
        let worst = longest_callback_block(true);

        assert!(
            worst > CALLBACK_BUDGET,
            "最久的一块只花了 {worst:?},没超过回调预算 {CALLBACK_BUDGET:?} —— 阻塞没传到回调这头"
        );
    }

    /// 对照组:流不卡时,每一块都在预算内取完。
    ///
    /// 排除"这条链路本身就跟不上实时"这个替代解释 —— 尤其是 normalize 那步的
    /// 44.1k→48k 重采样,它是链上唯一有点算力开销的环节。
    #[test]
    fn an_unstalled_stream_keeps_the_simulated_callback_on_time()
     {
        let worst = longest_callback_block(false);

        assert!(
            worst < CALLBACK_BUDGET,
            "流没卡,却有一块花了 {worst:?}(预算 {CALLBACK_BUDGET:?}) —— 慢的是链路本身,不是网络"
        );
    }

    /// **解码搬出回调之后,流卡住不再拖慢回调。**
    ///
    /// 这是把解码挪到自己线程上的验收判据,两个断言各挡一半:时长挡"回调被
    /// 拖住",非静音计数挡"什么都不放也算过" —— 静音取得飞快,只看时长的话
    /// 一个把声音全丢掉的实现能拿满分。
    #[test]
    fn buffering_carries_the_callback_through_a_stall() {
        let driven = drive_paced(
            buffered(codec::normalize(stalling_decoder())),
            PACED_BLOCKS,
        );

        assert!(
            driven.worst < CALLBACK_BUDGET,
            "有一块花了 {:?},超过回调预算 {CALLBACK_BUDGET:?} —— 停摆仍然穿到了回调这头",
            driven.worst
        );
        assert!(
            driven.audible
                > PACED_BLOCKS
                    * CALLBACK_FRAMES
                    * codec::SYNC_CHANNELS as usize
                    / 2,
            "只取到 {} 个非静音采样,缓冲扛住了卡顿却没把声音送出来",
            driven.audible
        );
    }

    /// **缓冲不能把"放完了"吃掉。**
    ///
    /// 自动续播靠 `Player::empty()` 知道该切下一首,而它为真的前提是源返回
    /// `None`。缓冲线程跑完要丢掉发送端,通道排空后 [`ChannelSource`] 才会结束 ——
    /// 漏了这一步,每首歌放完都会停在原地,再也不切下一首。
    ///
    /// 真正的判据是**这个测试能跑完**:结束不了的话它会挂死,而不是断言失败。
    #[test]
    fn a_buffered_song_still_ends() {
        // 100ms 的音,归一到 48kHz 立体声约合 9600 个采样。
        let decoder = decode(Cursor::new(wav(4_410)))
            .expect("合法 WAV 应能解码");

        let played =
            buffered(codec::normalize(decoder)).count();

        assert!(
            played >= 9_600,
            "只放出 {played} 个采样,短了一截 —— 缓冲把尾巴吃掉了"
        );
    }

    /// 测试用的旋钮:毫秒级的失联判定,以及小到几十 KB 的起播门槛。
    ///
    /// 生产值意味着一条测试要枯等十几秒 —— 那种测试没人会跑,也就等于没有。
    const FAST: Tuning = Tuning {
        prefetch_bytes: 16 * 1024,
        retry_timeout: Duration::from_millis(200),
        give_up_after: 2,
    };

    /// 起一个 HTTP 服务:先老实给 `prefix` 个字节,之后**装死** ——
    /// 连接不关,也不再给任何数据。
    ///
    /// 这才是"没网"真实的样子:wifi 连着、路由器亮着,但出口是个黑洞。拔网线
    /// 是另一回事,那会立刻 ECONNRESET,走的是别的错误路径。
    ///
    /// 裸 `TcpListener` 而不是 axum:要的行为恰恰是"不把响应收尾",
    /// 任何框架都会替我们收尾,反而做不出这个场景。
    ///
    /// **重连拿到的是空响应**:第二个连接起只给响应头。让它重发一遍数据的话,
    /// `on_progress` 会把失联计数清零,于是永远走不到放弃那一步。
    fn stalling_server(
        body: Vec<u8>,
        prefix: usize,
    ) -> String {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("绑不上本地端口");
        let addr =
            listener.local_addr().expect("取不到本地地址");
        let served = Arc::new(AtomicUsize::new(0));

        // 线程与进程同寿:测试进程退出即回收,不值得为它造一套关停。
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let body = body.clone();
                let served = served.clone();

                std::thread::spawn(move || {
                    // 请求头读到空行为止。内容不看 —— 对任何请求都给同一个回答。
                    let peek = stream
                        .try_clone()
                        .expect("连接复制不了");
                    let mut reader = BufReader::new(peek);
                    let mut line = String::new();
                    while reader
                        .read_line(&mut line)
                        .is_ok_and(|n| n > 2)
                    {
                        line.clear();
                    }

                    let head = format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Length: {}\r\n\
                         Content-Type: audio/wav\r\n\
                         Accept-Ranges: bytes\r\n\r\n",
                        body.len()
                    );
                    let _ =
                        stream.write_all(head.as_bytes());

                    let first = served
                        .fetch_add(1, Ordering::Relaxed)
                        == 0;
                    if first {
                        let _ = stream.write_all(
                            &body[..prefix.min(body.len())],
                        );
                    }
                    let _ = stream.flush();

                    if !first || prefix < body.len() {
                        // 装死。连接留着,字节不再来。
                        std::thread::park();
                    }
                });
            }
        });

        format!("http://{addr}/song.wav")
    }

    /// 一个**不声明 `Accept-Ranges`**、给几十 KB 就装死的服务,并把收到的每条
    /// 请求头原样记下来。
    ///
    /// 这是网易云 CDN 在真机日志里的样子(`Accept-Ranges: None`,约 62KB 后断供)。
    /// 记请求是本 fixture 存在的理由:要断言的正是**重连那一条请求长什么样**。
    fn range_watching_server(
        body: Vec<u8>,
        prefix: usize,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("绑不上本地端口");
        let addr =
            listener.local_addr().expect("取不到本地地址");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();

        std::thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let body = body.clone();
                let recorder = recorder.clone();
                let serve_body = first;
                first = false;

                std::thread::spawn(move || {
                    let peek = stream
                        .try_clone()
                        .expect("连接复制不了");
                    let mut reader = BufReader::new(peek);
                    let mut request = String::new();
                    let mut line = String::new();
                    while reader
                        .read_line(&mut line)
                        .is_ok_and(|n| n > 2)
                    {
                        request.push_str(&line);
                        line.clear();
                    }
                    recorder
                        .lock()
                        .expect("记录锁不该中毒")
                        .push(request);

                    // **不给 Accept-Ranges** —— 真实 CDN 就是这样,而
                    // stream-download 看不见它就不敢用 range。
                    let head = format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Length: {}\r\n\
                         Content-Type: audio/wav\r\n\r\n",
                        body.len()
                    );
                    let _ =
                        stream.write_all(head.as_bytes());
                    if serve_body {
                        let _ = stream.write_all(
                            &body[..prefix.min(body.len())],
                        );
                    }
                    let _ = stream.flush();
                    std::thread::park();
                });
            }
        });

        (format!("http://{addr}/song.wav"), seen)
    }

    /// **重连必须只要还缺的那一段,哪怕服务端没声明支持 range。**
    ///
    /// 不带 Range 的重连拿回来的是**整首歌的开头**,而那些字节会被写在当前写
    /// 位置上 —— 于是歌放到一半又从头来一遍,一段接一段。这不是推演:真机日志里
    /// 连续四次 `Accept-Ranges: None`,位置停在 63223 / 63219 / 63214 / 126429,
    /// 几乎是同一个数和它的两倍,正是"每次重连都重新给开头那 62KB"。
    #[test]
    fn a_reconnect_asks_only_for_the_bytes_it_still_needs()
    {
        let (url, requests) =
            range_watching_server(wav(200_000), 32 * 1024);

        let (decoder, _health) = runtime()
            .block_on(load_with(&url, FAST))
            .expect("头 32KB 是完整的 WAV,起播该成功");
        // 取到停摆之后:逼它撞上超时并重连。
        let _ = decoder.take(200_000).count();

        let seen = requests
            .lock()
            .expect("记录锁不该中毒")
            .clone();
        assert!(
            seen.len() >= 2,
            "服务端装死了,该发生过重连,实际只收到 {} 条请求",
            seen.len()
        );
        assert!(
            seen[1].contains("Range: bytes="),
            "重连没带 Range,拿回来的会是整首歌的开头:\n{}",
            seen[1]
        );
    }

    /// **服务端装死时,流必须放弃,而且要留下放弃的证据。**
    ///
    /// 不放弃的话下游会永远挂着 —— 那正是改这一版之前的样子:界面停在正在播放,
    /// 声音没有,`empty()` 仍为假,谁也不知道出了什么事(见 `docs/adr/0013`)。
    #[test]
    fn a_silent_server_makes_the_stream_give_up() {
        // 起播门槛 16KB,先给 32KB 再装死:load 能成,读到 32KB 之后才断。
        let url = stalling_server(wav(200_000), 32 * 1024);

        let started = std::time::Instant::now();
        let (decoder, health) = runtime()
            .block_on(load_with(&url, FAST))
            .expect(
                "头几十 KB 是完整的 WAV,起播这一步该成功",
            );
        // 取到源结束为止。放弃机制不成立的话,这一行永远回不来。
        let _ = decoder.count();
        let waited = started.elapsed();

        assert!(
            health.gave_up(),
            "源结束了却没留下放弃的证据,下游会把断流当成放完了"
        );
        assert!(
            waited < Duration::from_secs(3),
            "等了 {waited:?} 才放弃,远超两次 200ms 失联该有的时间"
        );
    }

    /// **断流不是立刻没声,缓冲会把它藏一会儿。**
    ///
    /// 这条钉的是那句"用户先听到约 5 秒沉默才看到横幅"里的前半段:流已经放弃了,
    /// 而存货还在放,用户此刻毫无察觉。判据是**放弃之后仍有多少块出了声** ——
    /// 缓冲要是没起作用,那个数会是零,断流当场就是死寂。
    ///
    /// 测试里缓冲调到 1 秒、失联调到 200ms;生产是 5 秒与 5 秒,同一个形状放大。
    #[test]
    fn the_buffer_keeps_playing_after_the_stream_is_gone() {
        // 约 2 秒的音频后装死。放弃只要 0.4 秒,所以断流发生时存货还厚着。
        let url = stalling_server(wav(200_000), 176 * 1024);
        let (decoder, health) = runtime()
            .block_on(load_with(&url, FAST))
            .expect("头一段是完整的 WAV,起播该成功");

        // 1 秒的缓冲:比放弃所需的 0.4 秒厚,不然"藏住了"这件事无从观察。
        let mut source = buffered_with(
            codec::normalize(decoder),
            codec::SYNC_SAMPLE_RATE as usize
                * codec::SYNC_CHANNELS as usize,
        );

        let mut audible_after_give_up = 0;
        let mut gave_up = false;
        let mut blocks = 0;
        'playing: loop {
            let mut audible = false;
            for _ in 0..CALLBACK_FRAMES
                * codec::SYNC_CHANNELS as usize
            {
                match source.next() {
                    Some(sample) => {
                        audible |= sample != 0.0;
                    }
                    // 发送端走了且通道排空 —— 这一路真的完了。
                    None => break 'playing,
                }
            }

            blocks += 1;
            gave_up |= health.gave_up();
            if gave_up && audible {
                audible_after_give_up += 1;
            }

            assert!(
                blocks < 400,
                "源迟迟不结束,缓冲那条线程没收工"
            );
            // 按实时节奏取,否则消费端无限快,缓冲永远是空的。
            std::thread::sleep(CALLBACK_BUDGET);
        }

        assert!(gave_up, "服务端装死了,这条流该放弃");
        assert!(
            audible_after_give_up >= 15,
            "放弃之后只出了 {audible_after_give_up} 块声音(约 {}ms),\
             缓冲没把断流藏住,用户当场就听到死寂",
            audible_after_give_up * 21
        );
    }

    /// 对照组:整段都送到的流,结束时**不能**留下放弃的证据。
    ///
    /// 没有这条,上一条也可能是"这个句柄永远为真"造成的 —— 那样自动续播会
    /// 再也不切歌,而两条测试都还是绿的。
    #[test]
    fn a_complete_stream_never_reports_a_give_up() {
        let body = wav(200_000);
        let complete = body.len();
        let url = stalling_server(body, complete);

        let (decoder, health) = runtime()
            .block_on(load_with(&url, FAST))
            .expect("完整的 WAV 该能起播");
        let played = decoder.count();

        assert!(played > 0, "一个采样都没放出来");
        assert!(
            !health.gave_up(),
            "整段都送到了,却报告成断流"
        );
    }

    /// 内存里的 `Cursor` 与真实流句柄走同一条解码路径,
    /// 因此这里读对了采样率和声道数,真实播放的格式协商也就是对的。
    #[test]
    fn decodes_wav_from_seekable_source() {
        let decoder = decode(Cursor::new(wav(4_410)))
            .expect("合法 WAV 应能解码");

        assert_eq!(decoder.sample_rate().get(), 44_100);
        assert_eq!(decoder.channels().get(), 1);
    }

    /// 直链过期时上游返回的是一个 HTML 错误页。必须报解码错误,
    /// 让上层能提示「重新获取播放地址」,而不是 panic 掉整个 UI 线程。
    #[test]
    fn rejects_non_audio_source() {
        let html =
            b"<html><body>403 Forbidden</body></html>";

        // 用 matches! 而非 expect_err:rodio 的 Decoder 没有 Debug,
        // expect_err 要求 Ok 侧可 Debug,编不过。
        assert!(matches!(
            decode(Cursor::new(html.to_vec())),
            Err(AudioError::Decode(_))
        ));
    }

    /// 零字节边界:服务端返回了 200 但正文是空的。
    #[test]
    fn rejects_empty_source() {
        assert!(matches!(
            decode(Cursor::new(Vec::new())),
            Err(AudioError::Decode(_))
        ));
    }

    /// 流式独有的故障:头是完整的、数据只下来一半。
    ///
    /// 关键不是"能否解码"(头齐全就能),而是**取样必须终止**。
    /// 断流时若解码器一直等下去,UI 会停在「播放中」再也不动 ——
    /// 那是最难排查的一类症状,所以在这里钉死。
    #[test]
    fn truncated_source_terminates_instead_of_hanging() {
        let full = wav(44_100);
        let truncated = full[..1_000].to_vec();

        let decoder = decode(Cursor::new(truncated))
            .expect("头完整时应能建出解码器");

        // 头里声称有 44100 个采样点,实到不足 500 个。
        // 上界取声称值的两倍:真挂住的话这里会先耗尽而不是永远转下去。
        let produced = decoder.take(88_200).count();

        assert!(
            produced < 44_100,
            "截断的流不该产出完整时长的采样: 实得 {produced}"
        );
    }
}
