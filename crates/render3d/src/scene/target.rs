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
    /// 点云是铺满视野的环境效果,竖视口不后撤(见 `camera::BASE_CAMERA_POS`),封面实物自己
    /// 按视口收尺寸。移轴镜头跟着尺寸走,下一帧由 `apply_lens` 重算。
    /// 重建后 `image` 置空,下一帧重新把新纹理导入 Slint。
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

        let old =
            std::mem::replace(&mut self.target, new_target);
        self.app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .remove(&old);

        self.size = (width, height);
        self.image = None;
        self.image_key = None;
    }
}
