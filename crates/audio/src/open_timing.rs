//! 开一条音频流的分段计时(#137 ⑥):DNS、TCP+TLS、首字节、攒够预读、解码器建好。
//!
//! 起播方差的主要来源是 CDN 开流(⑥ 前一半的测量),但一个总数看不出钱花在哪一段 ——
//! 是冷连接的握手，还是服务端出首字节慢，还是预读门槛太高。每开一条流记一行，和 `api:`
//! 那行同一种写法，日志能直接归并。
//!
//! DNS 与建连发生在 reqwest 的连接池里，看不见是替哪次请求建的。开流那段 future 套在
//! [`scope`] 里，连接器与解析器在同一个任务上跑，记进 task-local;复用了池里的连接就
//! 两样都不会被调到 —— 这正好就是「热连接」的判据。

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// 这次开流的连接是新建的还是池里复用的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    New,
    Reused,
}

/// 一次开流各段的耗时。每段是从上一段结束算起，不是从开流算起。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Laps {
    /// 主机名(不含路径与查询串:直链的查询串里有签名)。
    pub host: String,
    /// 域名解析。IP 直连、或者复用连接时没有。
    pub dns: Option<Duration>,
    /// TCP 建连 + TLS 握手(reqwest 不单独给出两者的分界)。复用连接时没有。
    pub connect: Option<Duration>,
    /// 连接好之后到响应头回来。
    pub head: Option<Duration>,
    /// 响应头回来之后到攒够预读门槛(或整首下完，若比门槛短)。
    pub prefetch: Option<Duration>,
    /// 攒够之后到解码器建好(已探测格式、解出第一包)。
    pub decode: Option<Duration>,
    /// 开流到解码器建好的总耗时。
    pub total: Option<Duration>,
}

impl Laps {
    /// 连接是新建的还是复用的。
    pub fn connection(&self) -> Connection {
        Connection::Reused
    }

    /// 日志里的那一行。
    pub fn line(&self) -> String {
        String::new()
    }
}

/// 开流这一段里记下的 DNS 与建连耗时。
#[derive(Debug, Default)]
pub(crate) struct Handshake {
    pub dns: Option<Duration>,
    pub connect: Option<Duration>,
}

tokio::task_local! {
    static CURRENT: Arc<Mutex<Handshake>>;
}

/// 在这次开流的计时范围里跑 `future`,交回它的结果与记下的握手耗时。
pub(crate) async fn scope<F: Future>(
    future: F,
) -> (F::Output, Handshake) {
    let cell = Arc::new(Mutex::new(Handshake::default()));
    let output = CURRENT.scope(cell.clone(), future).await;
    let handshake = std::mem::take(
        &mut *cell
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    (output, handshake)
}

/// 解析器量到一次 DNS。
pub(crate) fn note_dns(_took: Duration) {}

/// 连接器量到一次建连(含它内部的 DNS)。
pub(crate) fn note_connect(_took: Duration) {}
