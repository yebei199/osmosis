//! 取流的可调参数,以及一次取流的健康状况。

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
    pub(crate) std::sync::Arc<std::sync::atomic::AtomicBool>,
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
pub const PREFETCH_BYTES: u64 = 512 << 10;
