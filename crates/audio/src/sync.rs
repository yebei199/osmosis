//! 同步播放：把媒体位置锁到一条共享时间线上(#137 ⑤)。见 `sync/README.md`。

mod follower;
mod source;
mod timeline;

pub use follower::{
    ALIGNED_NS, Decision, Follower, JUMP_NS, MAX_CORR,
    SKIP_MAX_NS, Stats,
};
pub use source::{
    Feed, Pulled, Report, SourceFeed, SyncShared,
    SyncSource,
};
pub use timeline::{Anchor, Target};

#[cfg(test)]
mod tests;
