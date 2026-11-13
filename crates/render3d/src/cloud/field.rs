//! 点云的「场」:视口在点云平面上框住哪一块,封面那 96×96 格之外要往四边再铺多少格
//! 才铺得满,以及什么时候值得为此重烘一次网格。
//!
//! 封面那一块是固定的(同一份格点、同一份 uv),只有外面那一圈随视口长宽比与版式伸缩:
//! 宽版式主轴在窗宽 0.3 处,右边要补的远多于左边;紧凑版式主轴偏上,下面要补的多。
//! 格距与立方体大小一律不变 —— 场大了是方块多了,不是方块被拉宽。

use bevy::prelude::*;

use super::{
    CELL_WORLD, CLOUD_GRID, CLOUD_ORIGIN, PLANE_SIZE,
};
use crate::camera::{BASE_CAMERA_POS, BASE_FOV, shifts};

/// 相机到点云平面的距离。
fn plane_distance() -> f32 {
    BASE_CAMERA_POS.z - CLOUD_ORIGIN.z
}

/// 视口在点云平面上框住的那一块,点云的本地坐标(原点是封面中心,y 朝上),世界单位。
///
/// 镜头移轴只平移不缩放(见 `camera::lens`),所以框的高恒为 2·d·tan(视野/2),
/// 宽是它乘长宽比;主轴在框里的位置由版式的偏移量定(`camera::shifts`)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Framing {
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
    pub top: f32,
}

impl Framing {
    /// 这个视口框住哪一块。0 尺寸(首帧、刚重建)没有可框的,给 `None`。
    pub(crate) fn of_viewport(
        width: u32,
        height: u32,
        compact: bool,
    ) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let tall =
            2.0 * plane_distance() * (BASE_FOV * 0.5).tan();
        let wide = tall * width as f32 / height as f32;
        let shift = shifts(compact);
        Some(Self {
            left: -(0.5 - shift.x) * wide,
            right: (0.5 + shift.x) * wide,
            bottom: -(0.5 + shift.y) * tall,
            top: (0.5 - shift.y) * tall,
        })
    }

    /// 同一块在封面 uv 里的四边:(u 左, v 上, u 右, v 下)。封面本身是 0..1,
    /// 越出去的部分就是外圈。v 朝下,所以上边对的是 v 小的那一端。
    ///
    /// 换算与网格里格心的 uv 是同一条式子(第 g 格在 x = g·格距 − 半边长、
    /// u = (g + ½)/96),着色器拿它与顶点的 uv 直接比。
    pub(crate) fn cover_uv(&self) -> Vec4 {
        Vec4::new(
            cover_u(self.left),
            cover_v(self.top),
            cover_u(self.right),
            cover_v(self.bottom),
        )
    }
}

/// 平面 x → 封面 u。
fn cover_u(x: f32) -> f32 {
    ((x + PLANE_SIZE * 0.5) / CELL_WORLD + 0.5)
        / CLOUD_GRID as f32
}

/// 平面 y → 封面 v(翻过来,见 `mesh::cloud_vertices`)。
fn cover_v(y: f32) -> f32 {
    1.0 - cover_u(y)
}

/// 外圈的方块随声音往后退时,透视会把它们往主轴拉,视口边上就露出缝来。
/// 按「退到平面后面这么远也还铺得满」来算框,世界单位。外圈的起伏只有封面那块的
/// 三成多(见 `cloud.wgsl` 的 `OUTER_MOTION`),平常退不到这么远。
const DEPTH_SLACK: f32 = 0.25;

/// 再多铺几格:立方体有半格大,换歌脉冲还会把它们往外推一点。
const MARGIN_CELLS: f32 = 2.0;

/// 每一边最多补多少格。超宽的窗(32:9 那一档右边要两百多格)之外再宽的,
/// 不值得为它让顶点缓冲无上限地涨。
const MAX_CELLS: u32 = 256;

/// 已建的场比视口要的多出这么多格以内,不重建:视口缩一点就重烘一次不值,
/// 多出来的那一圈在视口外,不画也不碍事。
const SHRINK_SLACK: u32 = 8;

/// 封面那一块之外,四边各补多少格。全 0 就是只有封面那 96×96。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Field {
    pub left: u32,
    pub right: u32,
    pub bottom: u32,
    pub top: u32,
}

impl Field {
    /// 铺满这一块要补多少格(含退后的余量与边上那两格)。框落在封面之内的那一边补 0 ——
    /// 封面那一块总是整块建的,即使有一部分在视口外。
    pub(crate) fn covering(window: &Framing) -> Self {
        let distance = plane_distance();
        let reach = (distance + DEPTH_SLACK) / distance;
        let half = PLANE_SIZE * 0.5;
        let cells = |edge: f32| -> u32 {
            let beyond = (edge * reach - half) / CELL_WORLD;
            // 有限且被夹在 0..=MAX_CELLS 里,转换不会截断出意外的值。
            (beyond.ceil() + MARGIN_CELLS)
                .clamp(0.0, MAX_CELLS as f32)
                as u32
        };
        Self {
            left: cells(-window.left),
            right: cells(window.right),
            bottom: cells(-window.bottom),
            top: cells(window.top),
        }
    }

    /// 视口的场:0 尺寸时只有封面那一块。
    pub(crate) fn for_viewport(
        width: u32,
        height: u32,
        compact: bool,
    ) -> Self {
        Framing::of_viewport(width, height, compact)
            .map_or_else(Self::default, |window| {
                Self::covering(&window)
            })
    }

    /// 横向一共几列(含封面那 96 列)。
    pub(crate) fn columns(&self) -> usize {
        CLOUD_GRID
            + self.left as usize
            + self.right as usize
    }

    /// 纵向一共几行。
    pub(crate) fn rows(&self) -> usize {
        CLOUD_GRID
            + self.bottom as usize
            + self.top as usize
    }

    /// 一共几颗方块。
    pub(crate) fn cubes(&self) -> usize {
        self.columns() * self.rows()
    }

    /// 已建的这一圈配不配得上视口要的 `need`:每一边都盖得住,而且多出来的
    /// 不超过 [`SHRINK_SLACK`] 格。
    pub(crate) fn fits(&self, need: &Self) -> bool {
        [
            (self.left, need.left),
            (self.right, need.right),
            (self.bottom, need.bottom),
            (self.top, need.top),
        ]
        .into_iter()
        .all(|(built, want)| {
            built >= want && built - want <= SHRINK_SLACK
        })
    }
}

/// 视口要的场变了之后,要它稳住多久才重烘,秒(墙上时间)。
///
/// 拖着窗口边改大小时,视口每帧都在变;每帧重烘一两万颗方块的顶点缓冲是白干。
/// 等手停下来再烘一次;这期间露出来的那一条边,是拖动途中一闪而过的东西。
pub(crate) const REFIT_SETTLE_SECS: f32 = 0.2;

/// 场的重建时机:视口要的那一圈变了、而且稳住 [`REFIT_SETTLE_SECS`] 之后,重建一次。
pub(crate) struct Refit {
    built: Field,
    /// 正在等它稳住的那个场,与等了多久。
    pending: Option<(Field, f32)>,
}

impl Refit {
    /// 已经建好了 `built`。
    pub(crate) fn new(built: Field) -> Self {
        Self {
            built,
            pending: None,
        }
    }

    /// 这一帧视口要 `need`,距上一帧 `dt` 秒。该重建时返回要建的场,并记为已建。
    ///
    /// 坏 `dt` 当作没过时间:等的那一段只会被拖长,不会被跳过。
    pub(crate) fn step(
        &mut self,
        need: Field,
        dt: f32,
    ) -> Option<Field> {
        if self.built.fits(&need) {
            self.pending = None;
            return None;
        }
        let dt = if dt.is_finite() && dt > 0.0 {
            dt
        } else {
            0.0
        };
        let waited = match self.pending {
            Some((field, waited)) if field == need => {
                waited + dt
            }
            // 刚变、或者还在变:从头等。
            _ => 0.0,
        };
        if waited < REFIT_SETTLE_SECS {
            self.pending = Some((need, waited));
            return None;
        }
        self.pending = None;
        self.built = need;
        Some(need)
    }
}

#[cfg(test)]
mod tests;
