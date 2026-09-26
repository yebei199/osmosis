//! 信令客户端:连上 axum 的 `/signal`,自报家门,收发名册、校时与组状态。
//!
//! 只管**转达**。什么时候发什么是 [`crate::Client`] 那边的事。

use contract::{ClientSignal, DeviceDto, ServerSignal};
use tokio::sync::mpsc;

use crate::SyncError;

/// 收件箱容量。信令稀疏,这个数只是给突发留的缓冲。
///
/// 不能是 0:`mpsc::channel(0)` 会 panic,而这个常量看着像个可以随手调小的数字。
/// 编译期钉住,连带把这条约束写进类型系统而不是留给一条运行时测试。
const INBOX_CAPACITY: usize = 64;
const _: () = assert!(INBOX_CAPACITY > 0);

/// 信令端点的路径。调用方只给主机地址,不必知道服务端把它挂在哪。
const ENDPOINT: &str = "/signal";

/// 服务端认不出 token 时的状态码。
const UNAUTHORIZED: u16 = 401;

/// 服务端判 token 无效时 `ErrorDto` 里的 code(`server/src/error.rs`)。
const TOKEN_REJECTED: &str = "unauthorized";

/// 建连额度用完了。
const TOO_MANY_REQUESTS: u16 = 429;

/// 连着这么久一帧都没收到,就判这条连接死了。
///
/// 服务端每 30 秒 Ping 一次、容忍两次不回(`server::signaling` 的 `Timing`),
/// 所以一条活着的连接最多静默 30 秒 —— 75 秒是它的两倍多,留足了抖动的余地。
/// Ping 帧照样走 [`Signalling::next`] 底下那个流,所以「没有信令」不算静默。
///
/// 非有不可:少了它,客户端这侧**没有任何活性判断**。移动网络上半开的 socket
/// 不会有 FIN,`ws_rx.next()` 于是永远等下去 —— 重连不触发,组状态也就一直停在
/// 断之前那一版(#102 F-005)。TCP 自己发现要十几分钟。
const IDLE_LIMIT: std::time::Duration =
    std::time::Duration::from_secs(75);

/// 一条连着信令服务器的连接。
pub struct Signalling {
    /// 服务端来信。
    inbox: mpsc::Receiver<ServerSignal>,
    /// 发往服务端。
    outbox: mpsc::Sender<ClientSignal>,
}

/// 只能发、不能收的那一半,可以随手 clone。
///
/// 收信要 `&mut self`(独占那个收件箱),发信只要 `&self` —— 两者绑在同一个
/// 结构上时,谁都不能一边等来信一边发信。
#[derive(Clone)]
pub struct SignalSender(mpsc::Sender<ClientSignal>);

impl SignalSender {
    /// 出声设备的执行事实(#142),服务端转给组里其他设备。
    pub async fn report_device(
        &self,
        report: contract::DeviceReportDto,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::Report { report }).await
    }

    /// 校时的一次往返：发出去，等 `TimePong`。
    pub async fn time_ping(
        &self,
        id: u64,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::TimePing { id }).await
    }

    async fn push(
        &self,
        message: ClientSignal,
    ) -> Result<(), SyncError> {
        self.0.send(message).await.map_err(|_| {
            SyncError::Signalling("连接已关闭".to_owned())
        })
    }
}

/// 握手失败的分类。两种状态码单独拎出来,因为它们各要一种不同的等法:
///
/// - 带 `unauthorized` code 的 **401** 是「这个 token 不作数了」,不是「网络不好」—— 拿同一个 token
///   重试只会再得到一个 401,要等的是下一个 token;
/// - **429** 是「额度用完了」,而服务端**算得出还欠多少**,客户端算不出。
///   按自己的节奏重连只会把闸撞得更死(#109 F-R3)。
fn classify(
    error: tokio_tungstenite::tungstenite::Error,
) -> SyncError {
    let tokio_tungstenite::tungstenite::Error::Http(
        response,
    ) = &error
    else {
        return SyncError::Signalling(error.to_string());
    };

    match response.status().as_u16() {
        UNAUTHORIZED
            if rejects_the_token(response.body()) =>
        {
            SyncError::Unauthorized
        }
        TOO_MANY_REQUESTS => SyncError::Throttled {
            retry_after: retry_after(response),
        },
        _ => SyncError::Signalling(error.to_string()),
    }
}

/// 这个 401 是不是我们的服务端在说「token 不作数」(#127)。
///
/// 与 HTTP 那侧同一个判据:按 `ErrorDto` 的 code,不按状态码。半路代理回的 401、
/// 读不到响应体的 401 都不算 —— 算了的话界面会删掉一份好好的会话,而那不可逆;
/// 不算的代价只是按退避再连几次。
fn rejects_the_token(body: &Option<Vec<u8>>) -> bool {
    body.as_deref()
        .and_then(|body| {
            serde_json::from_slice::<contract::ErrorDto>(
                body,
            )
            .ok()
        })
        .is_some_and(|error| error.code == TOKEN_REJECTED)
}

/// `Retry-After` 里那个秒数。读不出来就是 `None` —— 那时退避走自己那一套。
fn retry_after<T>(
    response: &tokio_tungstenite::tungstenite::http::Response<T>,
) -> Option<core::time::Duration> {
    response
        .headers()
        .get(tokio_tungstenite::tungstenite::http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(core::time::Duration::from_secs)
}

impl Signalling {
    /// 连上并自报家门。
    ///
    /// `base_url` 形如 `ws://127.0.0.1:3000` —— 端点路径由本函数补上,
    /// 调用方不必知道服务端把它挂在哪。
    ///
    /// `token` 走 `Authorization: Bearer`:**账号由服务端从它定**,设备只自报
    /// id 与名字。浏览器的 `WebSocket` 构造器设不了请求头,所以这条路径
    /// 目前只有原生端走得通(服务端那侧同样的说明见 `server::signaling`)。
    pub async fn connect(
        base_url: &str,
        device: DeviceDto,
        token: &str,
    ) -> Result<Self, SyncError> {
        Self::connect_with_idle(
            base_url, device, token, IDLE_LIMIT,
        )
        .await
    }

    /// 同上,但判死的时限由调用方给。
    ///
    /// 拆出来只为测试:75 秒的判断不能靠真的等 75 秒,而这条判断恰恰是
    /// 半开 socket 唯一的出口,不测就没人走过。
    pub async fn connect_with_idle(
        base_url: &str,
        device: DeviceDto,
        token: &str,
        idle_limit: std::time::Duration,
    ) -> Result<Self, SyncError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

        let mut request = format!("{base_url}{ENDPOINT}")
            .into_client_request()
            .map_err(|e| {
                SyncError::Signalling(e.to_string())
            })?;
        let credential = format!("Bearer {token}")
            .parse()
            .map_err(|_| {
                SyncError::Signalling(
                    "token 放不进请求头".to_owned(),
                )
            })?;
        request
            .headers_mut()
            .insert(AUTHORIZATION, credential);

        let (socket, _) =
            tokio_tungstenite::connect_async(request)
                .await
                .map_err(classify)?;
        let (mut ws_tx, mut ws_rx) = socket.split();

        // 自报家门必须在**任何**其他消息之前:服务端在收到 Hello 之前不入册,
        // 之后发的信令会被它当作还没握手的连接丢掉。
        // 自报家门顺带说自己讲哪一版协议。新服务端据此在**入册之前**决定
        // 收不收这条连接(`docs/adr/0031`);旧服务端不认识这个字段,
        // 会原样忽略它,那一侧由客户端自己判(见 `client::verify_handshake`)。
        let hello = ClientSignal::Hello {
            device,
            protocol_version: contract::PROTOCOL_VERSION,
        };
        ws_tx
            .send(Message::text(
                serde_json::to_string(&hello)
                    .unwrap_or_default(),
            ))
            .await
            .map_err(|e| {
                SyncError::Signalling(e.to_string())
            })?;

        let (inbox_tx, inbox) =
            mpsc::channel(INBOX_CAPACITY);
        let (outbox, mut outbox_rx) =
            mpsc::channel::<ClientSignal>(INBOX_CAPACITY);

        // 收发各跑一个任务。合在一起的话,一边在等来信时另一边就发不出去 ——
        // 被控端每秒一条的上报不能等一条来信才发得出去。
        tokio::spawn(async move {
            loop {
                // 超时 = 判死。跳出去就把收件箱的发送端丢掉,`next` 于是
                // 返回 `None`,编排循环按「连接断了」处理并重连 ——
                // 与真的收到 FIN 走的是同一条路。
                //
                // 收件箱那头没人了(`Client` 被丢掉、编排循环收工)也要走:不走的话读半边
                // 一直攥着 socket,要等下一条来信或空闲超时才发现,服务端在这段时间里
                // 一直以为这台设备还在线(#142)。
                let incoming = tokio::select! {
                    incoming = tokio::time::timeout(
                        idle_limit,
                        ws_rx.next(),
                    ) => incoming,
                    () = inbox_tx.closed() => break,
                };
                let Ok(incoming) = incoming else {
                    break;
                };
                let Some(Ok(message)) = incoming else {
                    break;
                };
                // Ping/Pong 与二进制帧到不了这里,但它们同样重置上面那个
                // 计时器 —— 静默指的是「一帧都没有」,不是「没有信令」。
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(parsed) = serde_json::from_str::<
                    ServerSignal,
                >(&text) else {
                    continue;
                };
                if inbox_tx.send(parsed).await.is_err() {
                    break;
                }
            }
        });

        tokio::spawn(async move {
            while let Some(message) = outbox_rx.recv().await
            {
                let text = serde_json::to_string(&message)
                    .unwrap_or_default();
                if ws_tx
                    .send(Message::text(text))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok(Self { inbox, outbox })
    }

    /// 等下一条服务端消息。连接断了返回 `None`。
    pub async fn next(&mut self) -> Option<ServerSignal> {
        self.inbox.recv().await
    }

    /// 拿一个能独立发信的把手。
    pub fn sender(&self) -> SignalSender {
        SignalSender(self.outbox.clone())
    }
}

#[cfg(test)]
mod tests {
    use tokio_tungstenite::tungstenite::http;

    use super::*;

    /// 429 认得出来,而且把服务端给的秒数带出去。
    ///
    /// 不认的话它会落进 `Signalling(String)`,退避走客户端自己那套 1、2、4 秒
    /// —— 而额度欠着几百秒,那就是拿额度去撞一堵还没开的门(#109 F-R3)。
    #[test]
    fn a_throttled_upgrade_carries_the_wait_it_was_given() {
        let response = http::Response::builder()
            .status(429)
            .header(http::header::RETRY_AFTER, "340")
            .body(None::<Vec<u8>>)
            .map(Box::new)
            .expect("造不出响应");

        let classified = classify(
            tokio_tungstenite::tungstenite::Error::Http(
                response,
            ),
        );

        assert!(
            matches!(
                classified,
                SyncError::Throttled {
                    retry_after: Some(wait)
                } if wait == core::time::Duration::from_secs(340)
            ),
            "该认成限流并带上 340 秒,实得 {classified}"
        );
    }

    /// 没有 `Retry-After` 也仍然是限流 —— 只是等多久由客户端自己定。
    #[test]
    fn a_throttled_upgrade_without_a_hint_is_still_throttled()
     {
        let response = http::Response::builder()
            .status(429)
            .body(None::<Vec<u8>>)
            .map(Box::new)
            .expect("造不出响应");

        let classified = classify(
            tokio_tungstenite::tungstenite::Error::Http(
                response,
            ),
        );

        assert!(matches!(
            classified,
            SyncError::Throttled { retry_after: None }
        ));
    }

    /// 别的状态码仍然是普通信令错误,退避照旧。
    #[test]
    fn other_statuses_stay_ordinary_failures() {
        let response = http::Response::builder()
            .status(502)
            .body(None::<Vec<u8>>)
            .map(Box::new)
            .expect("造不出响应");

        let classified = classify(
            tokio_tungstenite::tungstenite::Error::Http(
                response,
            ),
        );

        assert!(matches!(
            classified,
            SyncError::Signalling(_)
        ));
    }
}
