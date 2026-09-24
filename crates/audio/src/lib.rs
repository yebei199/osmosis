//! 音频播放能力层:把一条直链变成声音。
//!
//! 与 `api`、`render3d` 平行 —— `app-core` 不认识本 crate,由 `ui` 注入。
//! 各端音频后端的差异(linux 走 alsa、android 走 AAudio、web 将来走 WebAudio)
//! 到此为止,不向上传播。
//!
//! **边下边播,不整曲下载。** [`load`] 给出的流句柄实现 `Read + Seek`,
//! rodio 读多少就下多少,首播不必等整首下完。
//!
//! 解码与出声刻意分开:出声需要真实声卡,断言不了;而解码才是真会出故障的地方 ——
//! 直链过期时上游返回的是一个 HTML 页面,不是音频。

pub mod pcm;
mod range_stream;
pub mod spectrum;
mod stream_client;
mod stream_source;
pub mod sync;

pub use stream_source::{
    BUFFER_SAMPLES, ChannelSource, SeekState, buffered,
    buffered_with,
};

mod error;
mod loader;
mod player;
mod runtime;

pub use error::AudioError;
pub use loader::{
    Loaded, PREFETCH_BYTES, Source, StreamHealth, Tuning,
    load, load_with,
};
pub use player::{Player, clamped_volume};
