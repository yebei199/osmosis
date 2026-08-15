//! 封面与歌词的推送口:各自记住上一次给过什么,只在真的变了时才推。

use super::*;

/// 点云封面的取用口:播放页每帧问它「这一帧封面该怎么办」。
///
/// 只在换歌那一帧交出动作,取走即回到"没消息" —— 一张封面是兆级的字节,
/// 每帧搬一次过 seam 纯属白耗(见 `crates/render3d::cloud`)。
///
/// 三态而不是"有没有新图":换歌与拿到新图之间隔着几百毫秒的网络,而封面
/// 常常根本拿不到(CDN 会过期)。只有"有没有新图"的话,这两种情况长得一样,
/// 点云就会一直挂着上一首(见 `crate::viz::CoverUpdate`)。
#[derive(Clone, Default)]
pub(crate) struct CoverFeed {
    pending: Rc<RefCell<crate::viz::CoverUpdate>>,
}

impl CoverFeed {
    /// 取走这一帧的动作,取完回到 [`crate::viz::CoverUpdate::Unchanged`]。
    pub(crate) fn take(&self) -> crate::viz::CoverUpdate {
        core::mem::take(&mut *self.pending.borrow_mut())
    }

    /// 换歌了:先让点云退回渐变,别挂着上一首的图等新图。
    pub(super) fn clear(&self) {
        *self.pending.borrow_mut() =
            crate::viz::CoverUpdate::Clear;
    }

    /// 新封面解出来了:排上队等下一帧取走。上一个动作还没被取走就直接顶掉 ——
    /// 点云只显示当前这一首,过期的封面排队也没人要。
    pub(super) fn replace(
        &self,
        pixels: std::sync::Arc<crate::viz::CoverPixels>,
    ) {
        *self.pending.borrow_mut() =
            crate::viz::CoverUpdate::Show(pixels);
    }
}

/// 歌词的取用口:播放页每帧问它「现在该显示哪一行」。
///
/// 行表随换歌整批替换,`generation` 随之自增 —— 调用方靠 `(generation, 行号)`
/// 判断该不该推新值:每帧无脑推会把属性标脏,播放页的省电门就白设了。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub(crate) struct LyricFeed {
    pub(super) lines:
        Rc<RefCell<Vec<app_core::LyricLineDto>>>,
    pub(super) generation: Rc<std::cell::Cell<u64>>,
    pub(super) player:
        Arc<Result<audio::Player, audio::AudioError>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl LyricFeed {
    /// 当前该显示的 (代际, 行号, 原文, 译文)。没歌词、还在前奏、或没播放器时给 `None`。
    pub(crate) fn current(
        &self,
    ) -> Option<(u64, usize, String, String)> {
        let player = self.player.as_ref().as_ref().ok()?;
        let position = player.position().as_millis() as i64;
        let lines = self.lines.borrow();
        let index =
            app_core::current_line(&lines, position)?;
        let line = lines.get(index)?;
        Some((
            self.generation.get(),
            index,
            line.text.clone(),
            line.translation.clone().unwrap_or_default(),
        ))
    }

    /// 歌词页要画的那一窗行,连同「有没有译文」。
    ///
    /// `browse` 是拖动浏览叠在当前行上的偏移。窗口怎么取归
    /// `app_core::window`,这里只负责把行表接上去。
    pub(crate) fn window(
        &self,
        browse: i32,
    ) -> Option<(
        u64,
        usize,
        Vec<(i32, String, String)>,
        bool,
    )> {
        let (generation, current, _, _) = self.current()?;
        let lines = self.lines.borrow();
        let window =
            app_core::window(&lines, current, browse);
        if window.is_empty() {
            return None;
        }

        let rows = (window.first
            ..window.first + window.len())
            .filter_map(|index| {
                let line = lines.get(index)?;
                Some((
                    window.offset_of(index)?,
                    line.text.clone(),
                    line.translation
                        .clone()
                        .unwrap_or_default(),
                ))
            })
            .collect::<Vec<_>>();
        let translated = lines
            .iter()
            .any(|line| line.translation.is_some());

        Some((generation, window.focus, rows, translated))
    }

    /// 换歌:先清空(旧歌词配新歌比空着更误导),取到再整批换上并递增代际。
    pub(super) fn replace(
        &self,
        lines: Vec<app_core::LyricLineDto>,
    ) {
        *self.lines.borrow_mut() = lines;
        self.generation.set(self.generation.get() + 1);
    }
}

/// wasm 上没有播放器,也就没有位置可读。恒给 `None`,调用方无需平台判断。
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Default)]
pub(crate) struct LyricFeed;

#[cfg(target_arch = "wasm32")]
impl LyricFeed {
    pub(crate) fn current(
        &self,
    ) -> Option<(u64, usize, String, String)> {
        None
    }

    pub(crate) fn window(
        &self,
        _browse: i32,
    ) -> Option<(
        u64,
        usize,
        Vec<(i32, String, String)>,
        bool,
    )> {
        None
    }
}
