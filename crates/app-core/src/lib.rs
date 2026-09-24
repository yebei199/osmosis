//! 客户端领域:应用持有的状态,以及改变该状态的规则。
//!
//! 这一层不知道自己被画成了什么样子(不依赖 slint),也不知道数据是怎么拿到的
//! (不依赖 `api`、不依赖 HTTP)。它是纯规则层:不碰时钟、不碰文件系统、不开线程,
//! 其 future 也不要求 `Send`。没有隐式的时间源和 IO,同一份输入永远同一个答案,
//! 所以可测、确定。见 `docs/adr/0002`。
//!
//! 需要网络的地方由调用方**注入**一个返回 future 的闭包 —— 见 [`health::refresh`]。
//! 这既让本 crate 可以脱离网络单测,也让它不必依赖 `api`。

mod counter;
mod group;
mod health;
mod lyric;
mod output;
mod playback;
mod queue;
mod session;

pub use counter::Counter;
pub use health::{Health, HealthState, refresh};
pub use lyric::{LyricWindow, current_line, window};
pub use output::{Output, RemoteView};
pub use playback::{Playback, PlaybackState, play};
pub use queue::{LoopMode, Queue};
pub use group::{
    Cue, Draft, Effective, Group, GroupRole, Intercepts, LEAD_US,
    MASTER_SILENT_MS, PREANNOUNCE_US, REANCHOR_US, Verdict,
};
pub use session::{
    Doubt, Effect, Move, PREPARE_TIMEOUT_MS, Party, Phase,
    Plan, Progress, READY_GRACE_MS, Refused, Role,
    START_TIMEOUT_MS, STOP_TIMEOUT_MS, Session, Step,
};

/// 从 `contract` 透传,免得 UI 层为了一个 DTO 再声明一次依赖。
pub use contract::{
    ArtistDto, DeviceDto, GroupPlanDto, HealthDto,
    LoopModeDto, NextEntryDto, LyricDto, OutputRouteDto,
    LyricLineDto, MAX_SIGNAL_BYTES, OperationAckDto,
    OperationPhase, PlaylistDto, PlaylistSource,
    RemoteCommand, RemotePlayState, RemoteStateDto,
    TrackDto, TracksDto,
};
