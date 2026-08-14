//! 歌词推给界面前的去重。
//!
//! 每帧无脑 `set` 会把属性标脏,暂停定格与失焦零重绘就都白设了;整窗行更是个
//! model,每帧重建会把整页标脏。所以这里记住上一次推的是哪一行、哪一窗。

use slint::ComponentHandle;

use crate::Viz;
use crate::music::LyricFeed;
use crate::{LyricRow, MainWindow};

/// 上一次推出去的 (代际, 行号) 与 (代际, 焦点行, 浏览偏移)。
#[derive(Default)]
pub(crate) struct LyricPush {
    line: Option<(u64, usize)>,
    window: Option<(u64, usize, i32)>,
}

impl LyricPush {
    /// 播放页那一行,只在换行或换歌时推。
    pub(crate) fn tick_line(
        &mut self,
        ui: &MainWindow,
        lyrics: &LyricFeed,
    ) {
        // ── 播放页歌词 ──
        // 只在覆层展开时跟随:收起时歌词不可见,读位置纯属白耗。
        // 暂停时位置不前进,行自然定格,与省电门天然一致。
        if ui.get_play_page_open() {
            match lyrics.current() {
                Some((generation, index, text, tr))
                    if self.line
                        != Some((generation, index)) =>
                {
                    ui.global::<Viz>()
                        .set_lyric_line(text.into());
                    ui.global::<Viz>()
                        .set_lyric_translation(tr.into());
                    self.line = Some((generation, index));
                }
                None if self.line.is_some() => {
                    // 换歌后的前奏:清空,不留上一首的最后一行。
                    ui.global::<Viz>().set_lyric_line(
                        slint::SharedString::new(),
                    );
                    ui.global::<Viz>()
                        .set_lyric_translation(
                            slint::SharedString::new(),
                        );
                    self.line = None;
                }
                _ => {}
            }
        }
    }

    /// 歌词页整窗,只在换窗或换歌时重建。
    pub(crate) fn tick_window(
        &mut self,
        ui: &MainWindow,
        lyrics: &LyricFeed,
    ) {
        // ── 歌词页整窗 ──
        // 只在歌词页展开时算:收起时那一窗谁也看不见。
        if ui.global::<Viz>().get_lyrics_page_open() {
            let browse =
                ui.global::<Viz>().get_lyric_browse();
            match lyrics.window(browse) {
                Some((
                    generation,
                    focus,
                    rows,
                    translated,
                )) if self.window
                    != Some((
                        generation, focus, browse,
                    )) =>
                {
                    let rows = rows
                        .into_iter()
                        .map(|(offset, text, tr)| {
                            LyricRow {
                                offset,
                                text: text.into(),
                                translation: tr.into(),
                            }
                        })
                        .collect::<Vec<_>>();
                    ui.global::<Viz>().set_lyric_rows(
                        slint::ModelRc::new(
                            slint::VecModel::from(rows),
                        ),
                    );
                    ui.global::<Viz>()
                        .set_lyric_has_translation(
                            translated,
                        );
                    self.window =
                        Some((generation, focus, browse));
                }
                None if self.window.is_some() => {
                    // 换歌后的前奏:清空整窗,顺手收起页 ——
                    // 一页空行不是「歌词页」,是个走不掉的空屏。
                    ui.global::<Viz>()
                        .set_lyric_rows(slint::ModelRc::new(
                        slint::VecModel::<LyricRow>::from(
                            Vec::new(),
                        ),
                    ));
                    ui.global::<Viz>()
                        .set_lyrics_page_open(false);
                    self.window = None;
                }
                _ => {}
            }
        }
    }
}
