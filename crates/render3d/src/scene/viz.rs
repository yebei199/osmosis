//! 可视化每一帧:选效果、装镜头、推进 bevy、等 GPU 落盘、把结果包成一张 slint 图。

use bevy::platform::time::Instant;
use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。

use super::Scene;
use super::{PERF_WINDOW, extract_texture};
use crate::camera::lens;
use crate::cloud;
use crate::effect::Effect;
use crate::seam::{CoverUpdate, SharedTexture, VizFrame};
use crate::sleeve;

impl Scene {
    /// 播放页的一帧:本 crate 里 bevy 侧播放页唯一的驱动入口。
    ///
    /// `time` 是播放页时钟(秒,门关即冻结,见 crates/ui);`audio` 是
    /// `spectrum` 布局的载荷(频谱行在前),只用前 512 字节拆频段。
    ///
    /// 只画当前那一个效果(`effect`),另一个整摊藏着。返回透明底的场景图,UI 侧
    /// 把它铺满整窗;主体偏左(宽版式)或偏上(紧凑版式)由相机移轴做(`compact`)。
    ///
    /// `width` / `height` 为窗口物理像素尺寸,与当前纹理不同就按需重建纹理
    /// (动态分辨率),0 尺寸忽略。纹理未就绪(首帧或刚重建)时返回空图;尺寸不变时
    /// 纹理身份稳定,只包装一次、之后复用 —— 内容每帧由 bevy 重画,Slint 重绘时实时采样。
    pub fn render_viz_frame(
        &mut self,
        frame: &VizFrame<'_>,
    ) -> slint::Image {
        let VizFrame {
            time,
            audio,
            cover,
            pointer,
            effect,
            compact,
            width,
            height,
        } = *frame;
        if width > 0
            && height > 0
            && (width, height) != self.size
        {
            self.resize(width, height);
        }

        // 播放页与卡墙互斥:播放页开着就不该有人渲卡墙,反之亦然。
        // 各自的入口把对方的相机关掉,谁也不会白渲一整面。
        self.wall.set_active(&mut self.app, false);
        if let Some(mut cam) = self
            .app
            .world_mut()
            .get_mut::<Camera>(self.camera)
        {
            cam.is_active = true;
        }

        if !self.cloud_built {
            self.rebuild_viz_content(compact);
            self.cloud_built = true;
        }
        self.set_effect(Effect::from_index(effect));
        self.apply_lens(compact);

        // 过渡按播放页时钟推进 —— 门关着时钟不走,换歌渐变跟着定格,
        // 重开门从定格处继续。首帧没有上一帧可减,当作没走。
        let delta =
            self.last_time.map_or(0.0, |last| time - last);
        self.last_time = Some(time);
        self.transition.advance(delta);

        match cover {
            CoverUpdate::Unchanged => {}
            CoverUpdate::Clear => self.clear_cover(),
            CoverUpdate::Show(w, h, rgba) => {
                self.apply_cover(w, h, rgba);
            }
        }

        self.apply_pointer(&pointer, delta);

        let levels = cloud::band_levels(
            audio.get(..512).unwrap_or(&[]),
        );
        match self.effect {
            Effect::Sleeve => self.sleeve.drive(
                &mut self.app,
                &sleeve::Drive {
                    time,
                    delta,
                    bass: levels.bass,
                    mid: levels.mid,
                    treble: levels.treble,
                    width: self.size.0,
                    height: self.size.1,
                    compact,
                },
            ),
            Effect::Cloud => {
                self.sleeve.settle(&mut self.app);
                self.refit_cloud(compact);
                self.drive_cloud(time, levels, compact);
            }
        }

        self.drive_and_finish()
    }

    /// 换效果:当前那一摊露面,另一摊整个藏起来 —— 藏起来的实体不进任何渲染阶段。
    ///
    /// 两摊都常驻、都收着封面,换过去不重建、不等封面。点云首帧才建,建的那一刻
    /// 按当时的效果带上可见性(见 `rebuild_viz_content`),所以这里只管「变了」。
    pub(crate) fn set_effect(&mut self, effect: Effect) {
        if effect == self.effect {
            return;
        }
        self.effect = effect;
        self.sleeve.set_visible(
            &mut self.app,
            effect == Effect::Sleeve,
        );
        let cloud = self.cloud_visibility();
        self.app
            .world_mut()
            .entity_mut(self.root)
            .insert(cloud);
    }

    /// 点云这一摊在当前效果下该不该露面。
    pub(crate) fn cloud_visibility(&self) -> Visibility {
        if self.effect == Effect::Cloud {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    }

    /// 按版式给相机装镜头(移轴),与上一次不同才写回 —— 写 `Camera` 或 `Projection`
    /// 会让 bevy 重算投影。
    fn apply_lens(&mut self, compact: bool) {
        let next = lens(self.size.0, self.size.1, compact);
        if self.lens == Some(next) {
            return;
        }
        self.lens = Some(next);
        let mut camera =
            self.app.world_mut().entity_mut(self.camera);
        if let Some(mut cam) = camera.get_mut::<Camera>() {
            cam.sub_camera_view = next.sub_view;
        }
        if let Some(mut projection) =
            camera.get_mut::<Projection>()
            && let Projection::Perspective(perspective) =
                &mut *projection
        {
            perspective.fov = next.fov;
        }
    }

    /// 视口变了、点云的场不再配得上它时,等它稳住再换一份网格(见 `cloud::Refit`)。
    ///
    /// 只换 `Mesh3d`:实体、材质、两张封面与换歌过渡都不动,重烘那一帧画面上只是
    /// 外圈多了或少了几列方块。只在点云露面时看 —— 藏着的时候视口怎么变都不花这份工,
    /// 切过来后由这里补上。
    fn refit_cloud(&mut self, compact: bool) {
        let now = Instant::now();
        let waited =
            self.refit_checked.map_or(0.0, |then| {
                now.duration_since(then).as_secs_f32()
            });
        self.refit_checked = Some(now);
        let need = cloud::Field::for_viewport(
            self.size.0,
            self.size.1,
            compact,
        );
        let Some(field) =
            self.cloud_refit.step(need, waited)
        else {
            return;
        };
        let mesh = self
            .app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(cloud::build_cloud_mesh(
                cloud::cloud_vertices(field),
            ));
        self.app
            .world_mut()
            .entity_mut(self.root)
            .insert(Mesh3d(mesh));
        log::info!(
            "render3d: 点云场重烘 {}x{} = {} 颗({}x{})",
            field.columns(),
            field.rows(),
            field.cubes(),
            self.size.0,
            self.size.1,
        );
    }

    /// 点云的一帧:几何一动不动,只换材质那一块 uniform —— 一两万个方块的位移在
    /// 顶点着色器里算(见 docs/adr/0012)。拖动转的是点云自己,不是相机。
    fn drive_cloud(
        &mut self,
        time: f32,
        levels: cloud::Levels,
        compact: bool,
    ) {
        // 外圈往视口边淡下去,淡到哪儿由视口在封面 uv 里的四边定;0 尺寸那一帧沿用上次的。
        let bounds = cloud::Framing::of_viewport(
            self.size.0,
            self.size.1,
            compact,
        )
        .map(|window| window.cover_uv());
        let color_mix = self.transition.color_mix();
        let burst = self.transition.burst();
        let object_scale =
            cloud::object_scale(self.size.0, self.size.1);
        if let Some(handle) = self.cloud_material.clone()
            && let Some(mut material) = self
                .app
                .world_mut()
                .resource_mut::<Assets<cloud::CloudMaterial>>()
                .get_mut(&handle)
        {
            material.params.time = time;
            material.params.bass = levels.bass;
            material.params.mid = levels.mid;
            material.params.treble = levels.treble;
            material.params.color_mix = color_mix;
            material.params.burst = burst;
            // 物体类预设在竖屏会左右出画,按长宽比再收一档(见 cloud.rs)。
            material.params.object_scale = object_scale;
            if let Some(bounds) = bounds {
                material.params.bounds = bounds;
            }
        }

        let (pitch, yaw) = self.spin.angles();
        let rotation = Quat::from_euler(
            EulerRot::YXZ,
            yaw,
            pitch,
            0.0,
        );
        if let Some(mut transform) =
            self.app
                .world_mut()
                .get_mut::<Transform>(self.root)
        {
            transform.rotation = rotation;
        }
    }

    /// update 一帧并把离屏纹理(按身份缓存)包装成 Image 交回。
    pub(crate) fn drive_and_finish(
        &mut self,
    ) -> slint::Image {
        self.perf +=
            super::probed_update(&mut self.app, "播放页");
        self.frames += 1;

        let Some(tex) = self.extract_texture(&self.target)
        else {
            if self.frames == 120 {
                // 两秒还没就绪,多半是渲染子世界里没准备出 GpuImage —— 值得告警排查。
                log::warn!(
                    "render3d: 已 120 帧仍未取到离屏纹理,3D 面板不会显示"
                );
            }
            return self.image.clone().unwrap_or_default();
        };

        if self.frames.is_multiple_of(PERF_WINDOW) {
            log::info!(
                "render3d: 近 {PERF_WINDOW} 帧均耗时 —— app.update() {:.2}ms({}x{},{:?})",
                self.perf / f64::from(PERF_WINDOW),
                self.size.0,
                self.size.1,
                self.effect,
            );
            self.perf = 0.0;
        }

        // 尺寸稳定时纹理身份稳定 → 只包装一次 Image,之后每帧由 bevy 重画内容,
        // Slint 重绘时实时采样同一张。
        let key = (tex.width(), tex.height());
        if self.image_key != Some(key) {
            let (w, h) = key;
            match slint::Image::try_from(tex) {
                Ok(img) => {
                    log::info!(
                        "render3d: 纹理就绪(第 {} 帧),{w}x{h} 已导入 Slint",
                        self.frames,
                    );
                    self.image = Some(img);
                    self.image_key = Some(key);
                }
                Err(e) => log::error!(
                    "wgpu 纹理导入 Slint 失败: {e:?}"
                ),
            }
        }

        self.image.clone().unwrap_or_default()
    }

    /// 从 bevy 的渲染子世界里取出某张离屏目标图对应的 `wgpu::Texture`。
    pub(crate) fn extract_texture(
        &self,
        handle: &Handle<Image>,
    ) -> Option<SharedTexture> {
        extract_texture(&self.app, handle)
    }
}
