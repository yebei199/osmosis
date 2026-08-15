//! 指针落到场景上,以及卡墙那一帧的转发。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。

use super::Scene;
use crate::seam::Pointer;
use crate::wall;

impl Scene {
    /// 把这一帧的指针状态变成涟漪与拖动。
    ///
    /// 指针**按住**时是拖动(转点云),没按住时划过就起涟漪 —— 与原版一致:
    /// `orbit.rotating` 那一支只转,不转的时候才 `queueParticlePointerFrame`。
    pub(crate) fn apply_pointer(
        &mut self,
        pointer: &Pointer,
        delta: f32,
    ) {
        if !pointer.active {
            self.last_pointer = None;
            self.spin.coast(delta);
            return;
        }
        let previous = self.last_pointer;
        self.last_pointer = Some((pointer.x, pointer.y));

        if pointer.down {
            // 拖动量按**物理像素**算,不然同一段手势在不同窗口大小下转得不一样多。
            if let Some((px, py)) = previous {
                let dx =
                    (pointer.x - px) * self.size.0 as f32;
                let dy =
                    (pointer.y - py) * self.size.1 as f32;
                self.spin.drag(dx, dy, delta);
            }
            return;
        }

        self.spin.coast(delta);
    }

    /// 卡墙的一帧(docs/adr/0025):位姿与相机由 `ui::wall` 算好传入,
    /// 这里摆进场景、渲一帧、交回纹理。与点云互斥 —— 本入口把点云的
    /// 两台相机关掉,只亮卡墙那台。省电门在 ui 侧,静止的墙不会调到这。
    pub fn render_wall_frame(
        &mut self,
        frame: &wall::WallFrame,
    ) -> slint::Image {
        self.wall.set_active(&mut self.app, true);
        for cam in [self.camera, self.occluder_camera] {
            if let Some(mut c) =
                self.app.world_mut().get_mut::<Camera>(cam)
            {
                c.is_active = false;
            }
        }
        self.wall.apply(&mut self.app, frame);
        self.app.update();
        self.wall.finish(&self.app)
    }
}
