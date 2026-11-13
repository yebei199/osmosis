//! 换封面:两个效果都收 —— 点云换色渐变,封面实物交接或显影 —— 以及没有封面时的清空。
//!
//! 当前没在显示的那个效果也照收:切过去时才不会挂着上一首。纹理只传一次,两边共用
//! 同一个句柄;旧纹理随最后一个句柄放掉而释放,不手动删 —— 点云的「上一首」与封面实物
//! 退场那张各自引用着它,谁先放手都不该把另一边的图抽走。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat,
};

use bevy::camera::visibility::NoFrustumCulling;

use super::Scene;
use super::mips::with_mips;
use crate::cloud;
use crate::cloud::CLOUD_ORIGIN;

impl Scene {
    /// 换歌那一刻:不挂着上一首的封面等新图。
    ///
    /// 点云只把 `has_cover` 落回 0 —— 着色器据此走默认渐变色,纹理本身留着不动:
    /// 换下一首时 [`Self::apply_cover`] 会把它轮成「上一首」,渐变过渡还要用。不起过渡:
    /// 这不是"换到另一张封面",而是"暂时没有封面",没有可渐变的两端。
    ///
    /// 封面实物把印着上一首的那张甩走,落位一张空封套(见 `Sleeve::clear`)。
    pub(crate) fn clear_cover(&mut self) {
        self.sleeve.clear(&mut self.app);
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

    /// 换歌:把新封面的像素传成纹理,交给两个效果。
    ///
    /// 尺寸不合法(0 边、字节数与宽高对不上)就整帧忽略 —— 像素来自跨 crate 的
    /// seam,坏载荷不该让画面黑掉,更不该 panic。
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

        let (pixels, levels) =
            with_mips(width, height, rgba);
        let mut image = Image::new_uninit(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            // 封面是 sRGB 编码的;按线性读会整体发暗,画出来的就不是封面的颜色了。
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        // 整条 mip 链一起传:点云外圈要一份糊过的封面(见 `cloud.wgsl` 的 `haze`),
        // 取高几级的 mip 就是。各级首尾相接、逐级减半,正是 wgpu 按 mip 顺序读的布局。
        image.texture_descriptor.mip_level_count = levels;
        image.data = Some(pixels);
        let next = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(image);

        self.sleeve.show(&mut self.app, next.clone());
        self.cloud_show(next);
    }

    /// 点云那一侧:新封面挂上去,旧的那张退成「上一首」,并起一次过渡
    /// (颜色渐变 + burst)。再旧的那张这时没人要了,随句柄放掉。
    fn cloud_show(&mut self, next: Handle<Image>) {
        let Some(handle) = self.cloud_material.clone()
        else {
            return;
        };
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
            previous != stale
        };

        // 「上一首」与「再上一首」是同一张只在首曲成立:两者都还是那张占位图。
        // 此时没有可渐变的旧封面,否则第一首歌会从一片纯白淡入。
        if rotated {
            self.transition.start();
        }
    }

    /// 重建播放页封面点云:despawn 旧内容,把封面那 [`cloud::CLOUD_GRID`]² 颗加上
    /// 铺满当前视口的外圈烘成**一份** mesh,配一份自定义材质,spawn 成单个实体。
    ///
    /// 位移不在这里 —— 几何建好就不动,每帧只有材质的 uniform 在换
    /// (见 `docs/adr/0012`);视口变了由 `refit_cloud` 只换网格。
    pub(crate) fn rebuild_viz_content(
        &mut self,
        compact: bool,
    ) {
        let field = cloud::Field::for_viewport(
            self.size.0,
            self.size.1,
            compact,
        );
        self.cloud_refit = cloud::Refit::new(field);
        self.app
            .world_mut()
            .entity_mut(self.root)
            .despawn();

        let mesh = self
            .app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(cloud::build_cloud_mesh(
                cloud::cloud_vertices(field),
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

        let visibility = self.cloud_visibility();
        self.root = self
            .app
            .world_mut()
            .spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(CLOUD_ORIGIN),
                visibility,
                // 相机移轴之后,bevy 的视锥仍按不移轴的那一幅算(见 `camera::lens`);
                // 点云只有一个实体,不剔也不值一提。
                NoFrustumCulling,
            ))
            .id();
        self.cloud_material = Some(material);

        log::info!(
            "render3d: 封面点云重建完成,{}x{} = {} 颗",
            field.columns(),
            field.rows(),
            field.cubes(),
        );
    }
}
