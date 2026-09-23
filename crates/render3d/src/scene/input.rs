//! 指针落到场景上,以及卡墙那一帧的转发。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。

use bevy::platform::time::Instant;

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
        super::probed_update(&mut self.app, "卡墙");
        self.wall.finish(&self.app)
    }

    /// 预热卡墙(#121):第一次由 [`Scene::new_async`] 在建窗口前调,之后墙还没露面时
    /// 由 ui 隔一会儿调一次,返回 `true` 表示编完了。
    ///
    /// 每次都离屏渲一帧 [`wall::WallFrame::prewarm`] —— 64×64 的小目标、一张闪卡
    /// 一张普通卡。第一次把卡墙要的管线全部排进异步编译队列;之后几次让
    /// `process_queue` 把后台编好的收进来,顺带让那些要等前一批就绪才排队的
    /// 管线也排上。相机开着是必须的:材质管线只为「开着的相机看得见的网格」
    /// 特化。渲完就关掉卡墙相机,不交纹理给 Slint。
    pub fn prewarm_wall(&mut self) -> bool {
        let (started, ready_at_start) =
            *self.prewarm.get_or_insert_with(|| {
                log::info!("render3d: 卡墙预热开始");
                (
                    Instant::now(),
                    super::ready_pipelines(&self.app).len(),
                )
            });
        self.wall.set_active(&mut self.app, true);
        for cam in [self.camera, self.occluder_camera] {
            if let Some(mut c) =
                self.app.world_mut().get_mut::<Camera>(cam)
            {
                c.is_active = false;
            }
        }
        self.wall.apply(
            &mut self.app,
            &wall::WallFrame::prewarm(),
        );
        super::probed_update(&mut self.app, "卡墙预热");
        self.wall.set_active(&mut self.app, false);

        let waiting = super::waiting_pipelines(&self.app);
        if waiting > 0 {
            return false;
        }
        log::info!(
            "render3d: 卡墙预热结束 {}ms,期间新建管线 {} 条",
            started.elapsed().as_millis(),
            super::ready_pipelines(&self.app)
                .len()
                .saturating_sub(ready_at_start),
        );
        true
    }
}
