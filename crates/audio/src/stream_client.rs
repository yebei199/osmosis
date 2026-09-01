//! 给 stream-download 的 HTTP 客户端:reqwest 0.12 + rustls-tls。
//!
//! stream-download 自带的 `reqwest` 特性拉的是 reqwest 0.13,它的 rustls 路线
//! 强制 aws-lc-rs + rustls-platform-verifier —— 后者在安卓上要 JNI 初始化胶水,
//! 不接就是每次握手都失败。这里换成与 crates/api 同一条栈(reqwest 0.12,
//! ring + 内置 webpki 根),安卓零胶水,依赖树里也少一个重复版本的 reqwest。
//!
//! 三个新类型都是薄委托,存在的唯一理由是孤儿规则:trait(stream-download 的)
//! 和类型(reqwest 的)都是外来的,必须有一个本地壳。逻辑对照上游自己的
//! `http/reqwest_client.rs`,行为一致。

use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use bytes::Bytes;
use futures_util::Stream;
use reqwest::header::{self, HeaderMap};
use stream_download::http::{
    Client, ClientResponse, RANGE_HEADER_KEY,
    ResponseHeaders, format_range_header_bytes,
};
use stream_download::source::DecodeError;

/// 播放流共用的 reqwest 客户端。
///
/// 不设整体超时:reqwest 的 timeout 罩住**整个响应体**,而一首歌的边下边读
/// 本来就要跨几分钟。失联由 loader 的 retry_timeout + on_reconnect 计数兜底。
static CLIENT: LazyLock<reqwest::Client> =
    LazyLock::new(reqwest::Client::new);

/// [`Client`] 的本地实现。
#[derive(Clone)]
pub struct StreamClient(reqwest::Client);

/// [`ClientResponse`] 的本地实现。
pub struct StreamResponse(reqwest::Response);

/// [`ResponseHeaders`] 的本地实现。
pub struct StreamHeaders(HeaderMap);

/// 服务端回了非 2xx。带上响应体,`decode_error` 时一并给出 —— 排查
/// "为什么这首放不了"时,body 里的那句错误比状态码有用得多。
#[derive(Debug)]
pub struct FetchError {
    source: reqwest::Error,
    response: reqwest::Response,
}

impl fmt::Display for FetchError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "Failed to fetch: {}", self.source)
    }
}

impl Error for FetchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

impl DecodeError for FetchError {
    async fn decode_error(self) -> String {
        match self.response.text().await {
            Ok(text) => format!("{}: {text}", self.source),
            Err(e) => {
                format!(
                    "{}. Error decoding response: {e}",
                    self.source
                )
            }
        }
    }
}

fn header_str<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Option<&'a str> {
    headers.get(name).and_then(|val| {
        val.to_str()
            .inspect_err(|e| {
                log::warn!(
                    "响应头 {name} 不是合法字符串: {e}"
                );
            })
            .ok()
    })
}

impl ResponseHeaders for StreamHeaders {
    fn header(&self, name: &str) -> Option<&str> {
        header_str(&self.0, name)
    }
}

impl ClientResponse for StreamResponse {
    type ResponseError = FetchError;
    type StreamError = reqwest::Error;
    type Headers = StreamHeaders;

    fn content_length(&self) -> Option<u64> {
        header_str(
            self.0.headers(),
            header::CONTENT_LENGTH.as_str(),
        )
        .and_then(|len| u64::from_str(len).ok())
    }

    fn content_type(&self) -> Option<&str> {
        header_str(
            self.0.headers(),
            header::CONTENT_TYPE.as_str(),
        )
    }

    fn headers(&self) -> Self::Headers {
        StreamHeaders(self.0.headers().clone())
    }

    fn into_result(
        self,
    ) -> Result<Self, Self::ResponseError> {
        match self.0.error_for_status_ref() {
            Ok(_) => Ok(self),
            Err(source) => Err(FetchError {
                source,
                response: self.0,
            }),
        }
    }

    fn stream(
        self,
    ) -> Box<
        dyn Stream<Item = Result<Bytes, Self::StreamError>>
            + Unpin
            + Send
            + Sync,
    > {
        Box::new(self.0.bytes_stream())
    }
}

impl Client for StreamClient {
    type Url = reqwest::Url;
    type Response = StreamResponse;
    type Error = reqwest::Error;
    type Headers = StreamHeaders;

    fn create() -> Self {
        Self(CLIENT.clone())
    }

    async fn get(
        &self,
        url: &Self::Url,
    ) -> Result<Self::Response, Self::Error> {
        self.0
            .get(url.clone())
            .send()
            .await
            .map(StreamResponse)
    }

    async fn get_range(
        &self,
        url: &Self::Url,
        start: u64,
        end: Option<u64>,
    ) -> Result<Self::Response, Self::Error> {
        self.0
            .get(url.clone())
            .header(
                RANGE_HEADER_KEY,
                format_range_header_bytes(start, end),
            )
            .send()
            .await
            .map(StreamResponse)
    }
}
