//! 信令客户端:连上 axum 的 `/signal`,自报家门,收发端到端信令。
//!
//! 只管**转达**。谁该给谁发 offer 是 [`crate::Peer`] 那边的事,这里不认识 WebRTC。

use contract::{ClientSignal, DeviceDto, ServerSignal};
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
}

impl Signalling {
    /// 连上并自报家门。
    ///
    /// `base_url` 形如 `ws://127.0.0.1:3000` —— 端点路径由本函数补上,
    /// 调用方不必知道服务端把它挂在哪。
    pub async fn connect(
        base_url: &str,
        device: DeviceDto,
    ) -> Result<Self, SyncError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (socket, _) = tokio_tungstenite::connect_async(
            format!("{base_url}{ENDPOINT}"),
        )
        .await
        .map_err(|e| {
            SyncError::Signalling(e.to_string())
        })?;
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
