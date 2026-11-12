//! 指针落到场景上,以及卡墙那一帧的转发。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。

use bevy::platform::time::Instant;

use super::Scene;
use crate::effect::Effect;
use crate::seam::Pointer;
use crate::wall;

impl Scene {
    /// 把这一帧的指针状态变成拖动,交给当前那个效果。
    ///
    /// 指针**按住**时是拖动:点云跟着转、松手后滑行(`cloud::Spin`);封面实物跟着偏、
    /// 松手后弹回原位(`sleeve::motion::Tilt`)。没按住、或指针离开,就是松手。
    pub(crate) fn apply_pointer(
        &mut self,
        pointer: &Pointer,
        delta: f32,
    ) {
        if !pointer.active {
            self.last_pointer = None;
            self.release_pointer(delta);
            return;
        }
        let previous = self.last_pointer;
        self.last_pointer = Some((pointer.x, pointer.y));

        if !pointer.down {
            self.release_pointer(delta);
            return;
        }
        // 拖动量按**物理像素**算,不然同一段手势在不同窗口大小下转得不一样多。
        // 刚按下那一帧没有上一点可减:点云什么也不做,封面实物记一笔「按住了」,
        // 弹簧这一帧起就不再往回拉。
        let (dx, dy) =
            previous.map_or((0.0, 0.0), |(px, py)| {
                (
                    (pointer.x - px) * self.size.0 as f32,
                    (pointer.y - py) * self.size.1 as f32,
                )
            });
        match self.effect {
            Effect::Sleeve => {
                self.sleeve.drag(dx, dy, delta)
            }
            Effect::Cloud => {
                if previous.is_some() {
                    self.spin.drag(dx, dy, delta);
                }
            }
        }
    }

    /// 没在拖:点云按惯性滑,封面实物交给弹簧。
    fn release_pointer(&mut self, delta: f32) {
        match self.effect {
            Effect::Sleeve => self.sleeve.release(),
            Effect::Cloud => self.spin.coast(delta),
        }
    }

    /// 卡墙的一帧(docs/adr/0025):位姿与相机由 `ui::wall` 算好传入,
    /// 这里摆进场景、渲一帧、交回纹理。与播放页互斥 —— 本入口把播放页的
    /// 相机关掉,只亮卡墙那台。省电门在 ui 侧,静止的墙不会调到这。
    pub fn render_wall_frame(
        &mut self,
        frame: &wall::WallFrame,
    ) -> slint::Image {
        self.wall.set_active(&mut self.app, true);
        self.deactivate_viz_camera();
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
        self.deactivate_viz_camera();
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

    /// 关掉播放页那台相机:卡墙在渲时它不能也渲,不然两边抢同一帧。
    fn deactivate_viz_camera(&mut self) {
        if let Some(mut camera) = self
            .app
            .world_mut()
            .get_mut::<Camera>(self.camera)
        {
            camera.is_active = false;
        }
    }
}
