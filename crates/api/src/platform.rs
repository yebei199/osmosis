//! 唯一按 target 分叉的地方。两个实现的**签名相同**,差异不外泄。

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::*;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{off_thread, set_state_dir, test_media_url};

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::set_state_dir;
#[cfg(target_arch = "wasm32")]
pub(crate) use web::*;
