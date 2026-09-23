//! API 客户端:把"取一份服务端健康状态"这样的意图,翻译成一次具体的网络往返。
//!
//! 这里是各端运行时能力差异(有无线程、能否阻塞)被吸收的地方,差异到此为止,
//! 不再向上传播。对外暴露的 `async fn` 在 native 与 wasm 上**签名完全相同**;
//! `Send` 约束只存在于本 crate 内部的 `platform` 模块里。见 `docs/adr/0002`。

mod artwork;

mod auth;

mod catalog;

#[cfg(not(target_arch = "wasm32"))]
mod download;

mod error;

mod history;

mod netease;

mod playlists;
mod queue;
mod url;

// 应用内升级(#129)。只有安卓装得了,但取数与校验两个原生端都编得过。
#[cfg(not(target_arch = "wasm32"))]
pub mod update;

pub(crate) mod platform;

pub mod session;

pub mod settings;

pub use artwork::{
    TRACK_ARTWORK_BUDGET, fetch_bytes, load_artwork,
    load_track_artwork, save_artwork, save_track_artwork,
    sweep_track_artwork,
};

pub use auth::{login, logout, register};

pub use catalog::{
    artist_tracks, daily, health, liked, lyric,
    play_source, search_artists, search_playlists,
    search_tracks,
};

// wasm 这一期不做:浏览器里"落到本机"是另一套东西(File System Access
// 或一次 <a download>),与两个原生端共用不了同一个写入口。
#[cfg(not(target_arch = "wasm32"))]
pub use download::download;

// 重 CPU 活的后台入口。wasm 没有线程,web 冻结中(#105),先只给原生。
#[cfg(not(target_arch = "wasm32"))]
pub use platform::off_thread;

// 状态目录的显式入口:安卓上环境变量给不出私有目录,由平台入口注入。
pub use platform::set_state_dir;

pub use error::{ApiError, base_url, is_release};

pub(crate) use error::server_error;

pub use history::{recent, record_play, stats};

pub use netease::{
    QrStep, netease_qr, netease_qr_state, netease_status,
    netease_unbind, qr_poll,
};

pub use queue::{
    create_queue, fetch_queue, publish_queue, queue_head,
    queue_page, report_queue_state, set_queue_intent,
};

pub use playlists::{
    add_playlist_tracks, create_playlist, delete_playlist,
    liked_ids, platform_playlist_tracks, playlist_tracks,
    playlists, remove_playlist_tracks, rename_playlist,
    set_liked, set_subscribed,
};

// 个人主页的统计类型顺着 api 走:ui 不直接依赖 contract,

// 它见到的形状都从取数的那一层拿(与 app-core 再导出播放类型同理)。

pub use contract::{
    NeteaseStatusDto, QrEventDto, QrLoginDto, StatsDto,
    TRIAL_ONLY, TopArtistDto, download_file_name,
};

// 队列那一组同理:ui 要发的请求体、要读的响应体,以及两侧共用的那两个上限,
// 都从取数的这一层见到(`docs/adr/0031`)。
pub use contract::{
    MAX_QUEUE_ENTRIES, QUEUE_PAGE_LIMIT, QueueEntryDto,
    QueueHeadDto, QueueIntentDto, QueueIntentState,
    QueueOperationOutcomeDto, QueuePageDto, QueueRefDto,
    QueueReportAckDto, QueueReportDto, SetQueueIntentDto,
};
