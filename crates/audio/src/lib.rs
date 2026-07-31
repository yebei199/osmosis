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
pub mod spectrum;
mod stream_source;

pub use stream_source::ChannelSource;

use std::io::{Read, Seek};
use std::sync::OnceLock;

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

/// 起播前先攒够多少字节。
///
/// rodio 是在 cpal 的音频回调里直接向解码器要采样的,而本 crate 交给它的流
/// **读到没下完的位置就阻塞**。两者叠起来:网络一旦跟不上实时码率,阻塞就发生在
/// 声卡回调里,设备欠载,听感是"卡在原地反复放同一小段",而 `Player::empty()`
/// 仍为假、`position()` 冻住 —— 上层看不出任何异常,也就永远不会自己恢复。
///
/// 默认的 256KB 对无损只有两秒左右,正是"开头卡住"的量级。4MB 约合无损半分钟,
/// 代价只是起播多等一会儿(下载器一直在后台跑,不是等满才出声)。
///
/// ponytail: 这只把起播那一段垫厚,治不了曲中掉速 —— 真要治得把解码挪出音频
/// 回调(解码线程 + 有界通道 + `ChannelSource`),等量到确实是曲中欠载再做。
const PREFETCH_BYTES: u64 = 4 << 20;

/// 把一条直链变成可播放的流式音频:开流 + 解码,全在后台 runtime 上完成。
///
/// 开流与解码不拆成两个公开函数,是因为它们**必须在同一个 runtime 上**跑。
/// 拆开的话调用方很容易在 Slint 的 UI 线程上解码 —— 那里没有 tokio 反应堆,
/// 下载推不动,解码器就一直等,界面停在「加载中」再也不动。
///
/// 落盘到临时文件而不是常驻内存:seek 回已下过的位置(拖进度条、解码器回读
/// 帧头)不必重新请求,而一首无损动辄几十兆,内存里堆着毫无必要。
pub async fn load(url: &str) -> Result<Loaded, AudioError> {
    let url = url.to_owned();

    runtime()
        .spawn(async move {
            let parsed = url.parse().map_err(|e| {
                AudioError::Stream(format!("{e}: {url}"))
            })?;
            let stream = StreamDownload::new_http(
                parsed,
                TempStorageProvider::default(),
                Settings::default()
                    .prefetch_bytes(PREFETCH_BYTES),
            )
            .await
            .map_err(|e| {
                AudioError::Stream(e.to_string())
            })?;

            // 解码要阻塞读若干秒(等够探测格式的字节),不能占着 async 线程。
            tokio::task::spawn_blocking(move || {
                decode(stream)
            })
            .await
            .map_err(|e| {
                AudioError::Stream(e.to_string())
            })?
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
    _device: MixerDeviceSink,
    player: rodio::Player,
    /// 可视化的频谱分析器,每次换源在 [`Self::play`] 里接上新支路。
    viz: spectrum::Analyzer,
}

impl Player {
    /// 打开默认音频设备。
    pub fn new() -> Result<Self, AudioError> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| AudioError::Device(e.to_string()))?;
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
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rodio::Source as _;
    use similar_asserts::assert_eq;

    use super::*;

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
