//! 离屏目标的尺寸变化。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::RenderTarget;

use super::{Scene, make_target};

impl Scene {
    /// 按新尺寸重建离屏目标纹理,把相机的 RenderTarget 指过去,释放旧纹理。
    ///
    /// 相机的投影长宽比由 bevy 每帧依据渲染目标尺寸自动更新,无需在此处理;**距离**也不必动 ——
    /// 粒子场是铺满视野的环境效果,竖视口不后撤,让粒子自然溢出画面上下(见
    /// [`BASE_CAMERA_POS`])。重建后 `image` 置空,下一帧重新把新纹理导入 Slint。
    pub(crate) fn resize(
        &mut self,
        width: u32,
        height: u32,
    ) {
        let new_target =
            make_target(&mut self.app, width, height);
        self.app
            .world_mut()
            .entity_mut(self.camera)
            .insert(RenderTarget::Image(
                new_target.clone().into(),
            ));

        // 遮挡层必须与场景图同尺寸同视角,否则两层对不上,遮挡会整体错位。
        let new_occluder =
            make_target(&mut self.app, width, height);
        self.app
            .world_mut()
            .entity_mut(self.occluder_camera)
            .insert(RenderTarget::Image(
                new_occluder.clone().into(),
            ));

        let old =
            std::mem::replace(&mut self.target, new_target);
        let old_occluder = std::mem::replace(
            &mut self.occluder_target,
            new_occluder,
        );
        let mut images = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>();
        images.remove(&old);
        images.remove(&old_occluder);

        self.size = (width, height);
        self.image = None;
        self.image_key = None;
        self.occluder_image = None;
        self.occluder_key = None;
    }
}
