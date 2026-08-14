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

            // 长度得在流被搬进解码任务之前问 —— 之后它就归解码器了。
            // 拿不到(上游没给 Content-Length)时这一首只能往前跳,见 [`decode`]。
            let byte_len = stream.content_length();

            // 解码要阻塞读若干秒(等够探测格式的字节),不能占着 async 线程。
            let decoder = tokio::task::spawn_blocking(
                move || decode(stream, byte_len),
            )
            .await
            .map_err(|e| AudioError::Stream(e.to_string()))??;

            Ok((decoder, health))
        })
        .await
        .map_err(|e| AudioError::Stream(e.to_string()))?
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

#[cfg(test)]
mod tests;
