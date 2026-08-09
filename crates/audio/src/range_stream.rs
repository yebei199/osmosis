//! 一条**永远用 range 续传**的 HTTP 流。
//!
//! `stream-download` 的重连分两条路(`http/mod.rs:343`):服务端声明了
//! `Accept-Ranges` 就从断点续,没声明就**从第 0 字节重新 GET 整首歌**。后一条
//! 是有害的 —— 重拉回来的字节会接着写在当前写位置上,于是歌的开头被写到了中段,
//! 播到那里就又听见一遍开头,一段接一段。
//!
//! 真机日志里连续四次 `Accept-Ranges: None`,位置停在 63223 / 63219 / 63214 /
//! 126429 —— 几乎是同一个数和它的两倍,正是"每次重连都重新给开头那 62KB"。
//!
//! 而那个头缺席未必代表服务端不认 range:中间隔着代理时,它可能根本没被转发。
//! 实测同一个 CDN 直连时回 `Accept-Ranges: bytes`,Range 请求也老老实实回 206。
//!
//! 所以这里把选择去掉:**重连一律带 Range**。真不支持的话服务端会回整个 200,
//! 那与不带 Range 的结果一样坏,不会更坏;而支持的话就修好了一整类损坏。

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use stream_download::http::HttpStream;
use stream_download::http::reqwest::Client;
use stream_download::source::SourceStream;

/// 包一层 [`HttpStream`],只改重连那一步。
pub struct RangeStream(HttpStream<Client>);

impl RangeStream {
    /// 转发响应头的读取。`load` 用它记日志(尤其是 `Accept-Ranges` 到底有没有)。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.0.header(name)
    }
}

impl Stream for RangeStream {
    type Item = <HttpStream<Client> as Stream>::Item;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0).poll_next(cx)
    }
}

impl SourceStream for RangeStream {
    type Params =
        <HttpStream<Client> as SourceStream>::Params;
    type StreamCreationError = <HttpStream<Client> as SourceStream>::StreamCreationError;

    async fn create(
        params: Self::Params,
    ) -> Result<Self, Self::StreamCreationError> {
        HttpStream::create(params).await.map(Self)
    }

    fn content_length(&self) -> Option<u64> {
        self.0.content_length()
    }

    async fn seek_range(
        &mut self,
        start: u64,
        end: Option<u64>,
    ) -> io::Result<()> {
        self.0.seek_range(start, end).await
    }

    /// **本模块存在的唯一理由。**
    ///
    /// 不问 `Accept-Ranges`,直接从当前位置续。`seek_range` 自己在头缺席时会
    /// 打一句 warn 然后照发不误(`http/mod.rs:310`)—— 那正是这里要的行为。
    async fn reconnect(
        &mut self,
        current_position: u64,
    ) -> io::Result<()> {
        self.0.seek_range(current_position, None).await
    }

    fn supports_seek(&self) -> bool {
        self.0.supports_seek()
    }
}
