//! 客户端领域:应用持有的状态,以及改变该状态的规则。
//!
//! 这一层不知道自己被画成了什么样子(不依赖 slint),也不知道数据是怎么拿到的
//! (不依赖 `api`、不依赖 HTTP)。它必须能编到 `wasm32-unknown-unknown`,
//! 因此不碰文件系统、不开线程,其 future 也不要求 `Send`。见 `docs/adr/0002`。
//!
//! 需要网络的地方由调用方**注入**一个返回 future 的闭包 —— 见 [`health::refresh`]。
//! 这既让本 crate 可以脱离网络单测,也让它不必依赖 `api`。

mod counter;
mod health;
mod lyric;
mod playback;
mod queue;

pub use counter::Counter;
pub use health::{Health, HealthState, refresh};
pub use lyric::current_line;
pub use playback::{Playback, PlaybackState, play};
pub use queue::{LoopMode, Queue};

/// 从 `contract` 透传,免得 UI 层为了一个 DTO 再声明一次依赖。
pub use contract::{
    ArtistDto, HealthDto, LyricDto, LyricLineDto,
    PlaylistDto, PlaylistSource, TrackDto, TracksDto,
};
