//! 把一个 URL 变成能出声的东西:取流、解码、包进带缓冲的 Source。

use std::io::{Read, Seek};
use std::sync::Arc;

use stream_download::storage::temp::TempStorageProvider;
use stream_download::{Settings, StreamDownload};

use crate::runtime::runtime;
use crate::{AudioError, range_stream};

mod tuning;

pub use tuning::{PREFETCH_BYTES, StreamHealth, Tuning};

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
pub async fn load_with(
    url: &str,
    tuning: Tuning,
) -> Result<(Loaded, StreamHealth), AudioError> {
    let (loaded, health, _) =
        load_timed(url, tuning).await?;
    Ok((loaded, health))
}

/// [`load_with`],同时交回这次开流的分段计时(见 [`crate::open_timing`]),并记一行 `stream:`。
pub async fn load_timed(
    url: &str,
    tuning: Tuning,
) -> Result<
    (Loaded, StreamHealth, crate::open_timing::Laps),
    AudioError,
> {
    use std::sync::OnceLock;
    use std::sync::atomic::{
        AtomicU64, AtomicUsize, Ordering,
    };
    use std::time::Instant;

    let url = url.to_owned();

    runtime()
        .spawn(async move {
            let started = Instant::now();
            let parsed: reqwest::Url = url.parse().map_err(|e| {
                AudioError::Stream(format!("{e}: {url}"))
            })?;
            let host = parsed.host_str().unwrap_or("?").to_owned();
            // 预读攒够的那一刻。回调在下载任务上跑，只记第一次。
            let prefetched = Arc::new(OnceLock::<Instant>::new());
            let prefetch_mark = prefetched.clone();

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
                    let target = match state.phase {
                        stream_download::StreamPhase::Prefetching {
                            target,
                            ..
                        } => Some(target),
                        _ => None,
                    };
                    if prefetch_reached(target, state.current_position) {
                        prefetch_mark.get_or_init(Instant::now);
                    }
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

            // 建连在这一段里发生(若不复用池里的连接);返回时响应头已经到了。
            let (stream, handshake) =
                crate::open_timing::scope(StreamDownload::new::<
                    range_stream::RangeStream,
                >(
                    parsed,
                    TempStorageProvider::default(),
                    settings,
                ))
                .await;
            let stream = stream.map_err(|e| {
                AudioError::Stream(e.to_string())
            })?;
            let headers_at = Instant::now();

            // 长度得在流被搬进解码任务之前问 —— 之后它就归解码器了。
            // 拿不到(上游没给 Content-Length)时这一首只能往前跳,见 [`decode`]。
            let byte_len = stream.content_length();

            // 解码要阻塞读若干秒(等够探测格式的字节),不能占着 async 线程。
            let decoder = tokio::task::spawn_blocking(
                move || decode(stream, byte_len),
            )
            .await
            .map_err(|e| AudioError::Stream(e.to_string()))??;
            let decoded_at = Instant::now();

            let laps = laps(
                host,
                &handshake,
                [started, headers_at, decoded_at],
                prefetched.get().copied(),
            );
            log::info!("{}", laps.line());
            Ok((decoder, health, laps))
        })
        .await
        .map_err(|e| AudioError::Stream(e.to_string()))?
}

/// 把开流时打下的几个时刻折成各段耗时。三个时刻依次是开流、响应头到、解码器建好。
fn laps(
    host: String,
    handshake: &crate::open_timing::Handshake,
    [started, headers_at, decoded_at]: [std::time::Instant;
        3],
    prefetched: Option<std::time::Instant>,
) -> crate::open_timing::Laps {
    // 建连那段含它内部的 DNS;首字节那段是扣掉建连之后的请求往返
    let dns = handshake.dns;
    let connect = handshake.connect;
    let head = (headers_at - started)
        .saturating_sub(connect.unwrap_or_default());
    // 解码器要等预读攒够才建得好;万一记下的时刻落在外面，夹回这两个时刻之间
    let prefetched = prefetched
        .map(|at| at.clamp(headers_at, decoded_at));
    crate::open_timing::Laps {
        host,
        dns,
        connect: connect.map(|c| {
            c.saturating_sub(dns.unwrap_or_default())
        }),
        head: Some(head),
        prefetch: prefetched.map(|at| at - headers_at),
        decode: Some(
            decoded_at - prefetched.unwrap_or(headers_at),
        ),
        total: Some(decoded_at - started),
    }
}

/// 解码一个音频源。失败时不 panic —— 直链过期是常态,不是程序错误。
///
/// **`byte_len` 决定这条流能不能往回跳。** rodio 的默认设置是
/// `is_seekable: false` / `byte_len: None`,于是 symphonia 把流当作只进不退,
/// 任何回跳都返回 `SeekErrorKind::ForwardOnly` —— 症状是进度条往前拖得动、
/// 往回拖报「这首跳不了」,而两次拖的是同一首歌。它同时是 MP3/Vorbis
/// **算总时长**的前提(这两种格式的头里没有时长)。
///
/// 拿不到长度时维持只进不退,如实报错。探长度要 seek 到流尾,而那对一条
/// 边下边播的流意味着**先把整首下完**,正是 [`load`] 存在的理由的反面。
pub fn decode<R: Source>(
    source: R,
    byte_len: Option<u64>,
) -> Result<rodio::Decoder<R>, AudioError> {
    let builder = rodio::decoder::DecoderBuilder::new()
        .with_data(source);
    // with_byte_len 顺带把 is_seekable 置真 —— 两者分开设是没有意义的,
    // rodio 的文档也这么说
    let builder = match byte_len {
        Some(len) => builder.with_byte_len(len),
        None => builder,
    };

    Ok(builder.build()?)
}

/// 这一次进度回调是不是已经攒够了预读。`target` 是预读阶段的门槛，别的阶段为 `None`。
///
/// 到达门槛就在那一次预读回调里记，不等之后第一次别的阶段的回调：stream-download 在预读阶段
/// 不放读的一方走，放行发生在预读之后的第一次写入或下载完成里，而那一次的回调排在放行**之后**。
/// 小文件几百微秒就解完码，常常抢在它前面，量出来的预读就丢了(#137 ⑥)。到达门槛的那次预读回调
/// 与放行在同一个下载任务里先后执行，一定在放行之前。短于门槛的文件没有这一次，只能等下载完的回调。
fn prefetch_reached(
    target: Option<u64>,
    position: u64,
) -> bool {
    target.is_none_or(|target| position >= target)
}

#[cfg(test)]
mod tests;
