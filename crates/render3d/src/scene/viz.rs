//! 可视化每一帧:推进 bevy、等 GPU 落盘、把结果包成两张 slint 图。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::Camera3dDepthLoadOp;
use bevy::platform::time::Instant;

use super::Scene;
use super::{PERF_WINDOW, extract_texture};
use crate::camera::{anchor_viewport, occluder_depth};
use crate::seam::{CoverUpdate, SharedTexture, VizFrame};
use crate::{cloud, marker};

impl Scene {
    /// 播放页粒子场的一帧:本 crate 唯一的驱动入口。
    ///
    /// `time` 是播放页时钟(秒,门关即冻结,见 crates/ui);`audio` 是
    /// `spectrum` 布局的载荷(频谱行在前),只用前 512 字节拆频段;
    /// 粒子位置与缩放每帧由 [`particles::particle_pose`] 算好直写 Transform。
    ///
    /// 返回 **(场景, 遮挡层, 卡片锚点)**。两张图的用法见 [`spawn_occluder_camera`]:
    /// UI 侧把它们夹着标注卡叠三层,卡片就被更近的粒子逐像素挡住。锚点是
    /// [`CARD_ANCHOR_LOCAL`] 投到视口的归一化位置(见 [`anchor_viewport`]),
    /// `None` 表示这一帧它不在画面里,卡片该收起来。
    ///
    /// 返回裸元组而不是自定义结构体 —— `slint::Image` 是 ui 与 render3d 本就共有的
    /// 类型,新造一个镜像结构体只是多一份要同步的字段。
    ///
    /// `width` / `height` 为窗口物理像素尺寸,与当前纹理不同就按需重建纹理
    /// (动态分辨率),0 尺寸忽略。纹理未就绪(首帧或刚重建)时返回空图;尺寸不变时
    /// 纹理身份稳定,只包装一次、之后复用 —— 内容每帧由 bevy 重画,Slint 重绘时实时采样。
    pub fn render_viz_frame(
        &mut self,
        frame: &VizFrame<'_>,
    ) -> (slint::Image, slint::Image, Option<(f32, f32)>)
    {
        let VizFrame {
            time,
            audio,
            cover,
            pointer,
            preset,
            needs_occluder,
            width,
            height,
        } = *frame;
        if width > 0
            && height > 0
            && (width, height) != self.size
        {
            self.resize(width, height);
        }

        // 点云与卡墙互斥:播放页开着就不该有人渲卡墙,反之亦然。
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
            self.rebuild_viz_content();
            self.cloud_built = true;
        }

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

        // 几何一动不动,一帧只换这一块 uniform:三万多颗粒子的位移在顶点
        // 着色器里算(见 docs/adr/0012)。
        let levels = cloud::band_levels(
            audio.get(..512).unwrap_or(&[]),
        );
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
            material.params.preset =
                cloud::preset_index(preset);
            // 物体类预设在竖屏会左右出画,按长宽比再收一档(见 cloud.rs)。
            material.params.object_scale = object_scale;
        }

        // 拖动转的是点云自己,不是相机 —— 相机一动遮挡层那台就得跟着动,
        // 两层还要逐像素对齐(见 cloud::Spin)。
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

        // 没有深度卡片就整台相机关掉:不渲、不导入纹理。这是第二次全场景绘制,
        // 白渲一帧就是白花一帧(见 VizFrame::needs_occluder)。
        if let Some(mut cam) = self
            .app
            .world_mut()
            .get_mut::<Camera>(self.occluder_camera)
        {
            cam.is_active = needs_occluder;
        }

        // 标记体沿轨道走这一帧。用播放页时钟而不是墙钟:门关着时钟不走,
        // 方块跟着定格,重开门从原处继续 —— 与换歌过渡同一套时基。
        let marker_pose = marker::pose(time);
        if let Some(mut transform) =
            self.app
                .world_mut()
                .get_mut::<Transform>(self.marker)
        {
            *transform = marker_pose;
        }

        // 锚点一次投影,深度门槛与卡片挂点都从它出来。
        let ndc = self.anchor_ndc(&marker_pose);
        let depth = occluder_depth(ndc);
        if let Some(mut cam3d) =
            self.app
                .world_mut()
                .get_mut::<Camera3d>(self.occluder_camera)
        {
            cam3d.depth_load_op =
                Camera3dDepthLoadOp::Clear(depth);
        }

        let (scene, occluder) =
            self.drive_and_finish(depth, needs_occluder);
        (scene, occluder, anchor_viewport(ndc))
    }

    /// update 一帧并把两张离屏纹理(按身份缓存)包装成 Image 交回。
    ///
    /// `needs_occluder` 为假时遮挡层那半整个跳过 —— 相机已经关了,纹理里
    /// 是上一次的残留,导进去只会让 UI 拿到一张过期的图。
    pub(crate) fn drive_and_finish(
        &mut self,
        depth: f32,
        needs_occluder: bool,
    ) -> (slint::Image, slint::Image) {
        let t_update = Instant::now();
        self.app.update();
        self.perf +=
            t_update.elapsed().as_secs_f64() * 1000.0;
        self.frames += 1;

        let Some(tex) = self.extract_texture(&self.target)
        else {
            if self.frames == 120 {
                // 两秒还没就绪,多半是渲染子世界里没准备出 GpuImage —— 值得告警排查。
                log::warn!(
                    "render3d: 已 120 帧仍未取到离屏纹理,3D 面板不会显示"
                );
            }
            return self.frame_images(needs_occluder);
        };

        if self.frames.is_multiple_of(PERF_WINDOW) {
            log::info!(
                "render3d: 近 {PERF_WINDOW} 帧均耗时 —— app.update() {:.2}ms({}x{},遮挡门槛 {:.5})",
                self.perf / f64::from(PERF_WINDOW),
                self.size.0,
                self.size.1,
                depth,
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

        // 遮挡层同理:身份稳定就只包一次。它比场景图晚一帧就绪也无妨 —— UI 侧的
        // `occluder-3d.width > 0` 守卫会让卡片先以不被遮挡的样子出现。
        if needs_occluder
            && let Some(tex) =
                self.extract_texture(&self.occluder_target)
        {
            let key = (tex.width(), tex.height());
            if self.occluder_key != Some(key) {
                match slint::Image::try_from(tex) {
                    Ok(img) => {
                        log::info!(
                            "render3d: 遮挡层就绪(第 {} 帧),{}x{} 已导入 Slint",
                            self.frames,
                            key.0,
                            key.1
                        );
                        self.occluder_image = Some(img);
                        self.occluder_key = Some(key);
                    }
                    Err(e) => log::error!(
                        "遮挡层纹理导入 Slint 失败: {e:?}"
                    ),
                }
            }
        }

        self.frame_images(needs_occluder)
    }

    /// 当前这一帧交给 UI 的两张图:(场景, 遮挡层)。任一未就绪时给空图。
    pub(crate) fn frame_images(
        &self,
        needs_occluder: bool,
    ) -> (slint::Image, slint::Image) {
        (
            self.image.clone().unwrap_or_default(),
            // 不需要就给空图。UI 侧的 `viz-occluder.width > 0` 守卫据此
            // 不摆那一层 —— 给一张过期的图会让卡片被上一帧的粒子挡着。
            if needs_occluder {
                self.occluder_image
                    .clone()
                    .unwrap_or_default()
            } else {
                slint::Image::default()
            },
        )
    }

    /// 从 bevy 的渲染子世界里取出某张离屏目标图对应的 `wgpu::Texture`。
    pub(crate) fn extract_texture(
        &self,
        handle: &Handle<Image>,
    ) -> Option<SharedTexture> {
        extract_texture(&self.app, handle)
    }

    /// 卡片锚点这一帧在主相机里的 NDC。**一次投影,两个去处**:z 给遮挡层当深度
    /// 门槛(见 [`occluder_depth`]),xy 给标注卡当挂点(见 [`anchor_viewport`])。
    ///
    /// `marker_pose` 是这一帧刚写进标记体的位姿,而不是从 `GlobalTransform` 读回来的 ——
    /// 传播要等 `app.update()`,读它拿到的是上一帧的位置,卡片会慢半拍跟在方块后面。
    /// 标记体没有父实体,`GlobalTransform` 与 `Transform` 恒等,直接用就是准的。
    pub(crate) fn anchor_ndc(
        &self,
        marker_pose: &Transform,
    ) -> Option<Vec3> {
        let camera_entity =
            self.app.world().entity(self.camera);
        let (Some(camera), Some(camera_pose)) = (
            camera_entity.get::<Camera>(),
            camera_entity.get::<GlobalTransform>(),
        ) else {
            return None;
        };
        camera.world_to_ndc(
            camera_pose,
            marker::front_face(marker_pose),
        )
    }
}
