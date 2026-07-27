//! 与另一台设备之间的一条 WebRTC 连接。
//!
//! 星型拓扑里主控与**每个**听众各有一条,所以这里描述的是一对一那条,
//! 不是整个会话(`docs/adr/0008`)。

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use webrtc::api::APIBuilder;
use webrtc::api::media_engine::{
    MIME_TYPE_OPUS, MediaEngine,
};
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::{Envelope, SyncError};

/// 待发 ICE 候选的缓冲容量。
///
/// 一次建连产出的候选是个位数(本机网卡数量级),64 是为突发留的余量。
const CANDIDATE_CAPACITY: usize = 64;

/// 这一端在会话里的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    /// 声音的来源,发 offer。
    Host,
    /// 声音的去处,回 answer。
    Listener,
}

/// 一条 WebRTC 连接,以及它要往外发的信令。
///
/// 可以随手 clone:每份都指向同一条连接。ICE 候选的中继要独立于调用方跑,
/// 而候选**只能被取走一次** —— 收件端因此关在 `Mutex` 里,谁取到算谁的。
#[derive(Clone)]
pub struct Peer {
    connection: Arc<RTCPeerConnection>,
    /// 本端产生的、需要经信令转给对端的东西(目前只有 ICE 候选)。
    outgoing: Arc<Mutex<mpsc::Receiver<Envelope>>>,
    /// 主控推流用的音频轨。听众侧为 `None`。
    track: Option<Arc<TrackLocalStaticSample>>,
}

impl Peer {
    /// 建一条连接。
    ///
    /// 主控会同时建好一条 Opus 音频轨并加进去 —— 轨必须在 offer **之前**存在,
    /// 否则协商出来的 SDP 里没有媒体行,之后再加就得重新协商一轮。
    pub async fn new(
        role: PeerRole,
    ) -> Result<Self, SyncError> {
        let api = build_api()?;
        let connection = Arc::new(
            api.new_peer_connection(configuration())
                .await
                .map_err(|e| {
                    SyncError::Peer(e.to_string())
                })?,
        );

        // 轨要在 offer 之前加进去,否则协商出的 SDP 里没有媒体行。
        let track = match role {
            PeerRole::Host => {
                let track = audio_track();
                connection
                    .add_track(track.clone())
                    .await
                    .map_err(|e| {
                    SyncError::Peer(e.to_string())
                })?;
                Some(track)
            }
            // 听众只收不发。加一条空轨会让它也出现在 SDP 里,
            // 对端于是以为可以往回推 —— 而这个方向上没人在听。
            PeerRole::Listener => None,
        };

        let (candidates, outgoing) =
            mpsc::channel(CANDIDATE_CAPACITY);
        connection.on_ice_candidate(Box::new(
            move |candidate| {
                let candidates = candidates.clone();
                Box::pin(async move {
                    // `None` 是"候选收集完了"的信号,没有东西要发。
                    let Some(candidate) = candidate else {
                        return;
                    };
                    let Ok(init) = candidate.to_json()
                    else {
                        return;
                    };
                    let Ok(encoded) =
                        serde_json::to_string(&init)
                    else {
                        return;
                    };
                    let _ = candidates
                        .send(Envelope::Candidate {
                            candidate: encoded,
                        })
                        .await;
                })
            },
        ));

        Ok(Self {
            connection,
            outgoing: Arc::new(Mutex::new(outgoing)),
            track,
        })
    }

    /// 主控:生成 offer 并设为本地描述。
    pub async fn create_offer(
        &self,
    ) -> Result<Envelope, SyncError> {
        let offer = self
            .connection
            .create_offer(None)
            .await
            .map_err(|e| SyncError::Peer(e.to_string()))?;
        // 先设本地描述再返回:ICE 候选的收集由它启动,
        // 漏了这一步候选永远不来,而 offer 看起来完全正常。
        self.connection
            .set_local_description(offer.clone())
            .await
            .map_err(|e| SyncError::Peer(e.to_string()))?;

        Ok(Envelope::Offer { sdp: offer.sdp })
    }

    /// 收下对端的一条信令。返回需要回给对端的东西(没有则 `None`)。
    ///
    /// 收 offer 会顺带生成 answer —— 两件事之间不能插入别的状态变更,
    /// 拆成两个方法就给了调用方插进去的机会。
    pub async fn accept(
        &self,
        envelope: Envelope,
    ) -> Result<Option<Envelope>, SyncError> {
        match envelope {
            Envelope::Offer { sdp } => {
                let offer =
                    RTCSessionDescription::offer(sdp)
                        .map_err(|e| {
                            SyncError::Peer(e.to_string())
                        })?;
                self.connection
                    .set_remote_description(offer)
                    .await
                    .map_err(|e| {
                        SyncError::Peer(e.to_string())
                    })?;

                let answer = self
                    .connection
                    .create_answer(None)
                    .await
                    .map_err(|e| {
                        SyncError::Peer(e.to_string())
                    })?;
                self.connection
                    .set_local_description(answer.clone())
                    .await
                    .map_err(|e| {
                        SyncError::Peer(e.to_string())
                    })?;

                Ok(Some(Envelope::Answer {
                    sdp: answer.sdp,
                }))
            }
            Envelope::Answer { sdp } => {
                let answer =
                    RTCSessionDescription::answer(sdp)
                        .map_err(|e| {
                            SyncError::Peer(e.to_string())
                        })?;
                self.connection
                    .set_remote_description(answer)
                    .await
                    .map_err(|e| {
                        SyncError::Peer(e.to_string())
                    })?;
                Ok(None)
            }
            Envelope::Candidate { candidate } => {
                let init: RTCIceCandidateInit =
                    serde_json::from_str(&candidate)
                        .map_err(|e| {
                            SyncError::Envelope(
                                e.to_string(),
                            )
                        })?;
                self.connection
                    .add_ice_candidate(init)
                    .await
                    .map_err(|e| {
                        SyncError::Peer(e.to_string())
                    })?;
                Ok(None)
            }
        }
    }

    /// 对端的音频轨到达时调用 `on_track`。
    ///
    /// 必须在协商**之前**挂上:轨是在 `set_remote_description` 期间到达的,
    /// 之后再挂就永远等不到那一次回调。
    pub fn on_track(
        &self,
        mut handler: impl FnMut() + Send + Sync + 'static,
    ) {
        self.connection.on_track(Box::new(
            move |_track, _receiver, _transceiver| {
                handler();
                Box::pin(async {})
            },
        ));
    }

    /// 等本端下一条要发出去的信令(ICE 候选)。收集完毕后返回 `None`。
    pub async fn next_outgoing(&self) -> Option<Envelope> {
        self.outgoing.lock().await.recv().await
    }

    /// 当前连接状态。
    pub fn state(&self) -> RTCPeerConnectionState {
        self.connection.connection_state()
    }

    /// 主控的音频轨。3c 往它灌 Opus 帧。
    pub fn track(
        &self,
    ) -> Option<&Arc<TrackLocalStaticSample>> {
        self.track.as_ref()
    }

    /// 关掉这条连接。
    pub async fn close(&self) -> Result<(), SyncError> {
        self.connection
            .close()
            .await
            .map_err(|e| SyncError::Peer(e.to_string()))
    }
}

/// 建一个只认 Opus 的 API 实例。
///
/// 只注册需要的编解码:注册全部的话,SDP 会带上一堆本项目永远用不到的
/// 视频编码,协商日志读起来也更难。
fn build_api() -> Result<webrtc::api::API, SyncError> {
    let mut media = MediaEngine::default();
    media
        .register_default_codecs()
        .map_err(|e| SyncError::Peer(e.to_string()))?;
    Ok(APIBuilder::new().with_media_engine(media).build())
}

/// 连接配置。
///
/// **不配 STUN/TURN**:同播的设备都在自家局域网里,host candidate 直接就能连上。
/// 公网穿透要等真有那个场景再说,而那时也该先问「该不该让音频出内网」。
fn configuration() -> RTCConfiguration {
    RTCConfiguration {
        ice_servers: Vec::<RTCIceServer>::new(),
        ..Default::default()
    }
}

/// 主控推流用的那条轨。
fn audio_track() -> Arc<TrackLocalStaticSample> {
    Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            ..Default::default()
        },
        "audio".to_owned(),
        "syncplay".to_owned(),
    ))
}
