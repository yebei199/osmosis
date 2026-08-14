//! 卡墙的每帧驱动与 slint 绑定(几何在 [`crate::wall`],渲染在 render3d)。
//!
//! 职责:把 slint 镜像来的指针/滚轮/点击变成相机与动画状态,每帧组装一份
//! [`WallControls`](POD seam)交给 apps/* 的卡墙闭包;封面缩略图到货后取出
//! 像素、烘上圆角,经同一条 seam 上传。渲染循环前台恒满帧
//! (见 change_log 2026-08-11 always-on-rendering),墙每帧照渲,没有冻结门。

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::MainWindow;
use crate::Player;
use crate::wall;

/// 一张卡的世界位姿,物理像素。镜像 `render3d::WallCard`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallCardControls {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rot_y: f32,
    pub rot_x: f32,
    pub dim: f32,
    pub size: f32,
}

/// 新到的一张封面(已烘圆角)。镜像 `render3d::WallCover`。
#[derive(Clone, Debug)]
pub struct WallCoverControls {
    pub slot: usize,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 一帧卡墙的全部控制量。镜像 `render3d::WallFrame`。
#[derive(Clone, Debug, Default)]
pub struct WallControls {
    pub width: u32,
    pub height: u32,
    pub pan_x: f32,
    pub dolly: f32,
    pub perspective: f32,
    pub cards: Vec<WallCardControls>,
    pub covers: Vec<WallCoverControls>,
}

/// 单击浮起的深度加成(相对卡边长的比例)。
const FOCUS_LIFT: f32 = 0.45;
/// 一帧最多上传几张封面,免得首屏三十多张一起解压卡一帧。
const COVERS_PER_FRAME: usize = 6;

/// 卡墙的跨帧状态。
pub struct WallDrive {
    cam: wall::WallCam,
    collapse: wall::Collapse,
    dolly: Option<wall::DollyRun>,
    /// dolly 落位后要播的曲目。
    pending_play: Option<slint::SharedString>,
    /// 单击浮起的卡。
    focus: Option<usize>,
    last_pointer: Option<(f32, f32)>,
    was_pressed: bool,
    /// 各槽位已上传的封面 URL;换了才重传。
    uploaded: Vec<slint::SharedString>,
    /// 已经喊过 needs-cover 的 URL,防止每帧重复喊。
    requested: Vec<slint::SharedString>,
}

impl Default for WallDrive {
    fn default() -> Self {
        Self::new()
    }
}

impl WallDrive {
    pub fn new() -> Self {
        Self {
            cam: wall::WallCam::default(),
            collapse: wall::Collapse::default(),
            dolly: None,
            pending_play: None,
            focus: None,
            last_pointer: None,
            was_pressed: false,
            uploaded: Vec::new(),
            requested: Vec::new(),
        }
    }

    /// 这一帧的布局(物理像素)。场区尺寸由 slint 元素回写到 root 属性。
    fn layout_now(ui: &MainWindow) -> wall::WallLayout {
        let dpr = ui.window().scale_factor();
        wall::layout(
            ui.get_wall_field_w() * dpr,
            ui.get_wall_field_h() * dpr,
            ui.get_compact(),
        )
    }

    /// 网格位姿(含塌回与浮起),供命中测试与组帧共用。
    fn poses(
        &self,
        lay: &wall::WallLayout,
        count: usize,
    ) -> Vec<wall::CardPose> {
        let n = count.min(wall::WALL_MAX_CARDS);
        (0..n)
            .map(|i| {
                let mut p = wall::card_pose(
                    lay,
                    i,
                    self.collapse.value,
                );
                if self.focus == Some(i) {
                    p.z += lay.card * FOCUS_LIFT;
                    p.dim = 1.0;
                }
                p
            })
            .collect()
    }

    /// 命中测试一次(物理像素坐标)。
    fn hit(
        &self,
        ui: &MainWindow,
        x: f32,
        y: f32,
    ) -> Option<usize> {
        let lay = Self::layout_now(ui);
        let count =
            ui.global::<Player>().get_tracks().row_count();
        let poses = self.poses(&lay, count);
        let dpr = ui.window().scale_factor();
        wall::hit_test(
            &lay,
            &self.cam,
            &poses,
            x * dpr,
            y * dpr,
        )
    }

    /// 组装这一帧交给渲染闭包的控制量。
    /// `None` 只剩一种情况:场区尺寸退化(还没量出来),没法组帧。
    pub fn frame(
        &mut self,
        ui: &MainWindow,
    ) -> Option<WallControls> {
        let lay = Self::layout_now(ui);
        if lay.w < 2.0 || lay.h < 2.0 {
            return None;
        }
        let dpr = ui.window().scale_factor();

        // 指针:按下时拖动,松开那一帧 release。
        let pressed = ui.get_wall_pressed();
        let px = ui.get_wall_px() * dpr;
        let py = ui.get_wall_py() * dpr;
        if pressed {
            if let Some((lx, ly)) = self.last_pointer {
                self.cam.drag(px - lx, py - ly);
            }
            self.last_pointer = Some((px, py));
        } else {
            if self.was_pressed {
                self.cam.release();
            }
            self.last_pointer = None;
        }
        self.was_pressed = pressed;

        self.cam.step(lay.col_pitch);
        let collapsing = self.collapse.step();

        // 塌回落地:把墙藏掉,列表接管。
        if !collapsing
            && self.collapse.target == 0.0
            && ui.get_wall_showing()
        {
            ui.set_wall_showing(false);
        }

        // dolly 落位:开播放页、放歌,墙退场。
        if let Some(run) = &mut self.dolly {
            let landed = run.step();
            if landed {
                if let Some(id) = self.pending_play.take() {
                    ui.global::<Player>().invoke_play(id);
                }
                ui.set_play_page_open(true);
                self.dolly = None;
                // 回来时相机回到静息位。
                self.cam = wall::WallCam::default();
            }
        }

        let count =
            ui.global::<Player>().get_tracks().row_count();
        let poses = self.poses(&lay, count);
        let covers = self.collect_covers(ui, poses.len());

        let dolly_extra = self
            .dolly
            .as_ref()
            .map_or(0.0, |run| run.dolly(&lay));

        let cards = poses
            .iter()
            .map(|p| {
                let w = wall::world_pose(&self.cam, p);
                WallCardControls {
                    x: w.x,
                    y: w.y,
                    z: w.z,
                    rot_y: w.rot_y,
                    rot_x: w.rot_x,
                    dim: w.dim,
                    size: lay.card,
                }
            })
            .collect();

        Some(WallControls {
            width: lay.w as u32,
            height: lay.h as u32,
            pan_x: self.cam.pan_x,
            dolly: self.cam.dolly + dolly_extra,
            perspective: lay.perspective,
            cards,
            covers,
        })
    }

    /// 收集这一帧新到的封面:有图、URL 换过、限量取出并烘圆角。
    /// 没图的槽顺便喊一声 needs-cover(墙上的卡多半还没进过列表可视区)。
    fn collect_covers(
        &mut self,
        ui: &MainWindow,
        count: usize,
    ) -> Vec<WallCoverControls> {
        let tracks = ui.global::<Player>().get_tracks();
        self.uploaded.resize(
            count.max(self.uploaded.len()),
            slint::SharedString::new(),
        );
        let mut out = Vec::new();
        for slot in 0..count {
            let Some(row) = tracks.row_data(slot) else {
                continue;
            };
            if row.cover_url.is_empty()
                || self.uploaded[slot] == row.cover_url
            {
                continue;
            }
            let Some(buf) = row.cover.to_rgba8() else {
                // 缩略图还没到:请求一次(列表虚拟化不会替看不见的行开口)。
                // 循环恒满帧,到货那一帧自然会被下面的上传逻辑接住。
                if !self.requested.contains(&row.cover_url)
                {
                    self.requested
                        .push(row.cover_url.clone());
                    ui.global::<Player>()
                        .invoke_needs_cover(
                            row.cover_url.clone(),
                        );
                }
                continue;
            };
            if out.len() >= COVERS_PER_FRAME {
                // 还有货没传完:下一帧继续。
                break;
            }
            let (w, h) = (buf.width(), buf.height());
            let mut rgba = buf.as_bytes().to_vec();
            wall::bake_rounded(&mut rgba, w, h);
            self.uploaded[slot] = row.cover_url.clone();
            out.push(WallCoverControls {
                slot,
                width: w,
                height: h,
                rgba,
            });
        }
        out
    }
}

/// 接上 slint 侧的回调。`drive` 由渲染循环共享。
pub(crate) fn bind(
    ui: &MainWindow,
    drive: &Rc<RefCell<WallDrive>>,
) {
    // GPU 构建才会走到 run_with_renderers,这里就是「支持卡墙」的判据。
    ui.set_wall_supported(true);

    let weak = ui.as_weak();
    let d = drive.clone();
    ui.on_wall_tap(move |x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let mut d = d.borrow_mut();
        d.focus = d.hit(&ui, x, y);
    });

    let weak = ui.as_weak();
    let d = drive.clone();
    ui.on_wall_double(move |x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let mut d = d.borrow_mut();
        let Some(index) = d.hit(&ui, x, y) else {
            return;
        };
        let Some(row) = ui
            .global::<Player>()
            .get_tracks()
            .row_data(index)
        else {
            return;
        };
        // 播放与开页都等 dolly 落位(设计稿:落位后才起点云)。
        let lay = WallDrive::layout_now(&ui);
        let pose =
            wall::card_pose(&lay, index, d.collapse.value);
        d.pending_play = Some(row.id);
        d.dolly = Some(wall::DollyRun {
            t: 0.0,
            target_z: pose.z,
        });
    });

    let d = drive.clone();
    ui.on_wall_wheel(move |delta| {
        d.borrow_mut().cam.wheel(delta);
    });

    // 列表 ⇄ 卡墙:目标一拨,插值自己走;塌回落地才真正藏墙(frame 里)。
    let weak = ui.as_weak();
    let d = drive.clone();
    ui.on_set_view_wall(move |to_wall| {
        let Some(ui) = weak.upgrade() else { return };
        let mut d = d.borrow_mut();
        d.collapse.target = if to_wall { 1.0 } else { 0.0 };
        if to_wall {
            ui.set_wall_showing(true);
        }
        ui.set_view_wall(to_wall);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 动画收敛只是插值到位,不再衍生冻结:step 收敛后照样可以每帧调用,
    /// 状态稳定不漂移(前台恒满帧,见 change_log 2026-08-11)。
    #[test]
    fn settled_steps_stay_stable_under_constant_calls() {
        let mut d = WallDrive::new();
        let lay = wall::layout(1808.0, 864.0, false);
        for _ in 0..300 {
            d.cam.step(lay.col_pitch);
            d.collapse.step();
        }
        let pan = d.cam.pan_x;
        let collapse = d.collapse.value;
        for _ in 0..100 {
            d.cam.step(lay.col_pitch);
            d.collapse.step();
        }
        assert_eq!(
            d.cam.pan_x, pan,
            "收敛后的相机不该漂移"
        );
        assert_eq!(
            d.collapse.value, collapse,
            "收敛后的塌回不该漂移"
        );
    }
}
