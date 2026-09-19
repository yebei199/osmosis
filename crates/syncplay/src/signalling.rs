//! 信令客户端:连上 axum 的 `/signal`,自报家门,收发端到端信令。
//!
//! 只管**转达**。谁该给谁发 offer 是 [`crate::Peer`] 那边的事,这里不认识 WebRTC。

use contract::{
    ClientSignal, DeviceDto, RemoteCommand, RemoteStateDto,
    ServerSignal,
};
use tokio::sync::mpsc;

use crate::{Envelope, SyncError};

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
/// 结构上时,谁都不能一边等来信一边发信。而 ICE 候选恰恰是在等对端应答的
/// **同时**源源不断产生的:每条连接都要有一个能独立发信的把手。
#[derive(Clone)]
pub struct SignalSender(mpsc::Sender<ClientSignal>);

impl SignalSender {
    /// 把一条端到端信令发给某台设备。
    pub async fn send(
        &self,
        to: &str,
        envelope: &Envelope,
    ) -> Result<(), SyncError> {
        self.0
            .send(ClientSignal::Signal {
                to: to.to_owned(),
                payload: envelope.encode(),
            })
            .await
            .map_err(|_| {
                SyncError::Signalling(
                    "连接已关闭".to_owned(),
                )
            })
    }

    // ── 遥控器模式(`docs/adr/0030`)。都只是把一条消息塞进同一个出口。 ──

    /// 接管 `target`。`resume` 是重连时手上那个代次,主动接管时是 `None`。
    pub async fn claim(
        &self,
        target: &str,
        resume: Option<u64>,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::ClaimControl {
            target: target.to_owned(),
            resume,
        })
        .await
    }

    /// 被控端退出被遥控。
    pub async fn exit_controlled(
        &self,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::ExitControlled).await
    }

    /// 把一条命令发给被控端。
    pub async fn command(
        &self,
        to: &str,
        cmd: RemoteCommand,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::Command {
            to: to.to_owned(),
            cmd,
        })
        .await
    }

    /// 把本机状态上报出去。目标由服务端从控制权槽位查。
    pub async fn report(
        &self,
        state: RemoteStateDto,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::State { state }).await
    }

    /// 向被控端要一次完整状态。
    pub async fn snapshot(
        &self,
        to: &str,
    ) -> Result<(), SyncError> {
        self.push(ClientSignal::SnapshotRequest {
            to: to.to_owned(),
        })
        .await
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

/// 握手失败的分类。401 单独拎出来:它是"这个 token 不作数了",
/// 而不是"网络不好" —— 拿同一个 token 重试只会再得到一个 401。
fn classify(
    error: tokio_tungstenite::tungstenite::Error,
) -> SyncError {
    if let tokio_tungstenite::tungstenite::Error::Http(
        response,
    ) = &error
        && response.status().as_u16() == UNAUTHORIZED
    {
        return SyncError::Unauthorized;
    }
    SyncError::Signalling(error.to_string())
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
        let hello = ClientSignal::Hello { device };
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
        // 而 ICE 候选恰恰是在等对端应答的同时源源不断产生的。
        tokio::spawn(async move {
            while let Some(Ok(message)) = ws_rx.next().await
            {
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

    /// 把一条端到端信令发给某台设备。
    pub async fn send(
        &self,
        to: &str,
        envelope: &Envelope,
    ) -> Result<(), SyncError> {
        self.sender().send(to, envelope).await
    }
}
