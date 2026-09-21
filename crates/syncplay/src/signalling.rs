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

/// 连着这么久一帧都没收到,就判这条连接死了。
///
/// 服务端每 30 秒 Ping 一次、容忍两次不回(`server::signaling` 的 `Timing`),
/// 所以一条活着的连接最多静默 30 秒 —— 75 秒是它的两倍多,留足了抖动的余地。
/// Ping 帧照样走 [`Signalling::next`] 底下那个流,所以「没有信令」不算静默。
///
/// 非有不可:少了它,客户端这侧**没有任何活性判断**。移动网络上半开的 socket
/// 不会有 FIN,`ws_rx.next()` 于是永远等下去 —— 重连不触发,重连时那条
/// resume claim 的自愈也就走不到,遥控器永久停在一个早就没了的会话上
/// (#102 F-005)。TCP 自己发现要十几分钟。
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

/// 这条命令发出去会是多少字节 —— **发之前**就能问。
///
/// 量的是真正上线的那一份:`ClientSignal::Command` 连着外层的 `type` 与 `to`
/// 一起序列化,与 [`Signalling::connect`] 里出栈那一行用的是同一个编码器。
/// 只量 `cmd` 自己会漏掉外层那几十个字节,而判定就卡在边界上时,漏掉多少都算错。
///
/// 为什么非要在发之前问:超限的消息不是「被丢掉」,是让服务端读循环跳出、
/// **整条连接断掉**(见 `contract::MAX_SIGNAL_BYTES`)。发出去再看结果,
/// 代价是遥控器连自己的控制权都一起丢了。
///
/// 序列化不出来时返回 `usize::MAX`:那一条本来也发不出去,当成"超限"拒掉,
/// 比当成"没问题"放行安全。
pub fn command_wire_len(
    to: &str,
    cmd: &RemoteCommand,
) -> usize {
    serde_json::to_string(&ClientSignal::Command {
        to: to.to_owned(),
        cmd: cmd.clone(),
    })
    .map_or(usize::MAX, |text| text.len())
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
        // 而 ICE 候选恰恰是在等对端应答的同时源源不断产生的。
        tokio::spawn(async move {
            loop {
                // 超时 = 判死。跳出去就把收件箱的发送端丢掉,`next` 于是
                // 返回 `None`,编排循环按「连接断了」处理并重连 ——
                // 与真的收到 FIN 走的是同一条路。
                let Ok(incoming) = tokio::time::timeout(
                    idle_limit,
                    ws_rx.next(),
                )
                .await
                else {
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
                // 帧长要说出来。服务端给每条消息设了上限
                // (`server::syncplay::signaling` 的 `MAX_MESSAGE_BYTES`),
                // 而信令里唯一**大小随用户数据增长**的是带整批曲目的
                // `RemoteCommand::Play` —— 超限时它连解析都到不了,于是
                // 那一跳的日志是空的,症状与「压根没发」一模一样(#108)。
                if let ClientSignal::Command {
                    cmd, ..
                } = &message
                {
                    log::info!(
                        "遥控命令出栈: {} {} 字节",
                        cmd.summary(),
                        text.len()
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 量的是**连外层一起**的那一份,因为服务端数的就是那一份。
    ///
    /// 只量 `cmd` 自己会漏掉 `{"type":"command","to":"…"}` 那几十个字节 ——
    /// 而判定正卡在边界上时,漏掉多少都算错。
    #[test]
    fn the_measured_length_includes_the_envelope() {
        let cmd = RemoteCommand::Next;
        let bare = serde_json::to_string(&cmd)
            .expect("命令该序列化得出来")
            .len();

        let wire = command_wire_len("pc1", &cmd);

        assert!(
            wire > bare,
            "外层的 type 与 to 也要算进去:裸 {bare} 字节,上线 {wire} 字节"
        );
    }

    /// **没有一条命令随用户的数据增长了**(#109 AC-2)。
    ///
    /// 这里原本有两条相反的测试,它们钉的是当时的事实:`a_longer_batch_measures_larger`
    /// 断言两百首比一首大两个数量级,`a_thousand_track_batch_exceeds_the_signal_limit`
    /// 断言一千首越得过 64 KiB —— 那正是 #108 在真机上撞到的那条命令
    /// (977 首实测 224194 字节)。两条的前提都是「`Play` 拖着整批曲目」,
    /// 而本轮把曲目挪去了 HTTP(`docs/adr/0031`),前提没了,断言跟着翻过来。
    ///
    /// 上限那道自检**没有跟着删**:`Play` 现在是定长的,但 `command_wire_len`
    /// 仍是发之前唯一一道闸,而超限的后果仍然是整条连接断掉。哪天有人往
    /// 某条命令里塞回一个长字段,拦住它的还是这一道。
    #[test]
    fn no_command_grows_with_user_data() {
        let commands = [
            RemoteCommand::Play {
                queue_id: 7,
                revision: 3,
                entry_id: 12,
                operation_id:
                    "8f1c2e0a-0000-4000-8000-000000000000"
                        .to_owned(),
            },
            RemoteCommand::Pause,
            RemoteCommand::Resume,
            RemoteCommand::Next,
            RemoteCommand::Prev,
            RemoteCommand::Seek { ms: 42_000 },
            RemoteCommand::Volume { level: 0.35 },
        ];

        for cmd in commands {
            let wire = command_wire_len("pc1", &cmd);
            assert!(
                wire < 512,
                "{} 上线 {wire} 字节 —— 命令该是定长的",
                cmd.summary()
            );
        }
    }
}
