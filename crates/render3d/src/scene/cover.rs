//! 换封面:旧图淡出、新图重建点云,以及没有封面时的清空。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat,
};

use super::{CLOUD_ORIGIN, Scene};
use crate::cloud;

impl Scene {
    /// 换歌那一刻:点云退回渐变,不挂着上一首的封面等新图。
    ///
    /// 只把 `has_cover` 落回 0 —— 着色器据此走默认渐变色,纹理本身留着不动:
    /// 换下一首时 [`Self::apply_cover`] 会把它轮成「上一首」,渐变过渡还要用。
    ///
    /// 不起过渡:这不是"换到另一张封面",而是"暂时没有封面",没有可渐变的两端。
    pub(crate) fn clear_cover(&mut self) {
        let Some(handle) = self.cloud_material.clone()
        else {
            return;
        };
        let mut materials = self
            .app
            .world_mut()
            .resource_mut::<Assets<cloud::CloudMaterial>>();
        if let Some(mut material) =
            materials.get_mut(&handle)
        {
            material.params.has_cover = 0.0;
        }
    }

    /// 换歌:把新封面的像素传成纹理挂上点云,旧的那张退成「上一首」,
    /// 并起一次过渡(颜色渐变 + burst)。
    ///
    /// 尺寸不合法(0 边、字节数与宽高对不上)就整帧忽略 —— 像素来自跨 crate 的
    /// seam,坏载荷不该让点云黑掉,更不该 panic。
    pub(crate) fn apply_cover(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        let expected =
            (width as usize) * (height as usize) * 4;
        if width == 0
            || height == 0
            || rgba.len() != expected
        {
            log::warn!(
                "render3d: 封面像素不合法({width}x{height},{} 字节),这一次不换",
                rgba.len()
            );
            return;
        }
        let Some(handle) = self.cloud_material.clone()
        else {
            return;
        };

        let image = Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            rgba.to_vec(),
            // 封面是 sRGB 编码的;按线性读会整体发暗,点云的颜色就不是封面的颜色了。
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        let next = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(image);

        // 旧的那张退成「上一首」,渐变期间还看得见;再旧的那张这时才没人要。
        let rotated = {
            let mut materials = self
                .app
                .world_mut()
                .resource_mut::<Assets<cloud::CloudMaterial>>();
            let Some(mut material) =
                materials.get_mut(&handle)
            else {
                return;
            };
            let previous = material.cover.clone();
            let stale = material.prev_cover.clone();
            material.prev_cover = previous.clone();
            material.cover = next;
            material.params.has_cover = 1.0;
            (previous, stale)
        };

        // `stale == previous` 只在首曲成立:两者都还是那张占位图。此时既没有
        // 可渐变的旧封面(否则第一首歌会从一片纯白淡入),占位图也还挂在
        // `prev_cover` 上,不能释放。
        let (previous, stale) = rotated;
        if stale != previous {
            self.transition.start();
            self.app
                .world_mut()
                .resource_mut::<Assets<Image>>()
                .remove(&stale);
        }
    }

    /// 重建播放页封面点云:despawn 旧内容,把 [`cloud::CLOUD_GRID`]² 颗粒子烘成
    /// **一份** mesh,配一份自定义材质,spawn 成单个实体。
    ///
    /// 位移不在这里 —— 几何建好就不动,每帧只有材质的 uniform 在换
    /// (见 `docs/adr/0012`)。
    pub(crate) fn rebuild_viz_content(&mut self) {
        self.app
            .world_mut()
            .entity_mut(self.root)
            .despawn();

        let mesh = self
            .app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(cloud::build_cloud_mesh(
                cloud::cloud_vertices(),
            ));
        // 还没有封面时的占位图:`has_cover` 为 0,着色器走默认渐变色 ——
        // 点云在没有封面时不该消失。取到封面后由 `apply_cover` 换掉。
        let cover = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(Image::default());
        let material = self
            .app
            .world_mut()
            .resource_mut::<Assets<cloud::CloudMaterial>>()
            .add(cloud::CloudMaterial {
                params: cloud::CloudParams::default(),
                prev_cover: cover.clone(),
                cover,
            });

        self.root = self
            .app
            .world_mut()
            .spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(CLOUD_ORIGIN),
            ))
            .id();
        self.cloud_material = Some(material);

        log::info!(
            "render3d: 封面点云重建完成,{0}x{0} = {1} 颗",
            cloud::CLOUD_GRID,
            cloud::CLOUD_GRID * cloud::CLOUD_GRID,
        );
    }
}
