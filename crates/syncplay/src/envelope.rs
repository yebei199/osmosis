//! 信令载荷的内部结构。
//!
//! 服务端把载荷当成一段不透明文本转发(`docs/adr/0008`),所以这里的编码
//! 只是两端客户端之间的约定 —— 改它不必动服务端一行,也不必升 `PROTOCOL_VERSION`。

use serde::{Deserialize, Serialize};

use crate::SyncError;

/// 一条端到端的信令。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Envelope {
    /// 主控发起:这是我的 SDP,你听不听?
    Offer { sdp: String },
    /// 听众应答。
    Answer { sdp: String },
    /// 一个 ICE 候选地址。
    ///
    /// 独立于 SDP 单发(trickle ICE):等候选收集完再发 offer 的话,
    /// 每次建连都要先干等几百毫秒到几秒,而那段时间里界面上什么都没有。
    Candidate { candidate: String },
}

impl Envelope {
    /// 编码成能塞进 `ClientSignal::Signal` 的那段文本。
    ///
    /// 序列化不会失败(全是 String 字段),但真失败了也不能 panic ——
    /// 一条发不出去的信令只该让这次建连失败,不该带走整个进程。
    pub fn encode(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| String::new())
    }

    /// 从收到的载荷里解出来。
    pub fn decode(
        payload: &str,
    ) -> Result<Self, SyncError> {
        serde_json::from_str(payload)
            .map_err(|e| SyncError::Envelope(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 编码再解码要拿回一模一样的东西。
    ///
    /// SDP 里全是 CRLF 和冒号,任何一步顺手做了转义/规范化,
    /// 都会让对端的 `set_remote_description` 以一个含糊的解析错误告败。
    #[test]
    fn envelope_survives_a_round_trip() {
        let original = Envelope::Offer {
            sdp: "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n"
                .to_owned(),
        };

        let decoded = Envelope::decode(&original.encode())
            .expect("自己编的自己该认得");

        assert_eq!(decoded, original);
    }

    /// 读不懂的载荷报错,不 panic —— 对端版本不一致时这条路径是可达的。
    #[test]
    fn unreadable_payload_is_an_error() {
        assert!(matches!(
            Envelope::decode("这不是 JSON"),
            Err(SyncError::Envelope(_))
        ));
    }
}
