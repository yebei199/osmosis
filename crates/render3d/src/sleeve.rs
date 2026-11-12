//! 封面实物:播放页的默认效果。当前曲目的封面印在一块带倒角的薄封套上,
//! 立在一片暗色亮面地板之上,地板里有它淡出的倒影与一圈接触阴影。
//!
//! 一座「台面」(stage)实体挂着全部东西:封套、它的倒影、换歌时退场的那一张与其倒影、
//! 地面。台面随视口整体缩放并往前倾一点(见 `motion`),里面的几何建一次就不动。
//!
//! 随音乐动的**只有光**:低频点亮封套倒角那一圈边缘光(`sleeve.wgsl`)。几何不跟鼓点
//! 抖 —— 那是点云的语言;一张实物封套跟着节拍晃,看着像坏了。它自己的运动只有三种:
//! 很慢的呼吸、拖动后回弹、换歌时旧的甩走新的落位。
//!
//! 半透明分平台(`docs/adr/0012` 记的 Adreno 坑):桌面上倒影与地面走 `Blend`;
//! 安卓上走 `Mask` 加 4×4 有序抖动 —— 按透明度丢掉一部分像素,留下的是不透明的,
//! 远看就是那个透明度。封套本身两端都不透明。

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::Indices;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, PrimitiveTopology, ShaderType,
};
use bevy::shader::ShaderRef;

pub(crate) mod mesh;
pub(crate) mod motion;

/// `sleeve.wgsl` 在资产系统里的路径。着色器随二进制内嵌(`embedded_asset!`),
/// 应用无头运行、没有 `assets/` 目录。
const SHADER_PATH: &str = "embedded://render3d/sleeve.wgsl";

/// 地面在台面单位里的高度:封套底边之下留一条缝,封套是「放在」地面上方的,
/// 呼吸与回弹时下沿不至于扎进地里(扎进去的那一截由倒影那侧剪掉,见着色器)。
const FLOOR_Y: f32 = -(mesh::HALF + 0.035);

/// 着色器里按 `kind` 分三种画法,与 `sleeve.wgsl` 手工对齐。
const KIND_SLAB: u32 = 0;
const KIND_MIRROR: u32 = 1;
const KIND_FLOOR: u32 = 2;

/// 封面实物材质的 uniform 块,逐字段镜像 `sleeve.wgsl` 的 `SleeveParams`。
#[derive(Clone, Copy, Default, ShaderType)]
pub(crate) struct SleeveParams {
    /// 地面平面,世界坐标:xyz 是朝上的法线,w 是偏移(`dot(n, p) = w`)。
    /// 倒影只画在它下面。
    pub plane: Vec4,
    /// 地面 uv 里的脚印:(中心 u, 中心 v, 每台面单位合多少 u, 合多少 v),见 [`mesh::footprint`]。
    pub footprint: Vec4,
    /// 封套正面这一帧在世界里朝哪(xyz)。法线与它同向的片元印封面,其余是纸边。
    pub facing: Vec4,
    /// 封套相对台面的 (yaw, pitch):正面那一道淡淡的光泽跟着它滑。
    pub tilt: Vec2,
    /// 画法:封套 / 倒影 / 地面。
    pub kind: u32,
    /// 1 = 用有序抖动代替半透明混合(安卓)。
    pub dither: u32,
    /// 封面的显影程度,0 是空封套。
    pub cover: f32,
    /// 平滑过的低频,点亮边缘光。
    pub glow: f32,
    /// 台面缩放:一个台面单位合多少世界单位。倒影的淡出距离按它换算。
    pub scale: f32,
}

/// 封面实物的材质:封套、倒影、地面共用一份着色器,按 `kind` 分支。
///
/// 共用而不是三种材质:三者的光照、封面采样、抖动全是同一套,分开写就是三份要
/// 同步的 WGSL;合在一起只多一个 uniform 分支,而它对整个 draw 是一致的。
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct SleeveMaterial {
    #[uniform(0)]
    pub params: SleeveParams,
    /// 印在正面的封面。空封套与地面挂的是占位图,`params.cover` 为 0 时内容无所谓。
    #[texture(1)]
    #[sampler(2)]
    pub cover: Handle<Image>,
    /// 倒影与地面是半透明的;封套不是。不进绑定组,只决定混合模式。
    pub translucent: bool,
}

impl Material for SleeveMaterial {
    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    /// 封套不透明。半透明的两样分平台:桌面 `Blend`,安卓 `Mask` —— 那边由着色器
    /// 自己按抖动阈值丢像素,留下的写不透明,alpha 测试的门槛因此无所谓。
    fn alpha_mode(&self) -> AlphaMode {
        if !self.translucent {
            return AlphaMode::Opaque;
        }
        #[cfg(target_os = "android")]
        return AlphaMode::Mask(0.5);
        #[cfg(not(target_os = "android"))]
        return AlphaMode::Blend;
    }

    /// 关掉 prepass 与阴影:场景里没有 bevy 的光,也没有谁要读深度预通道。
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

/// 有序抖动开关:只有安卓走它(见模块文档)。
const DITHER: u32 =
    if cfg!(target_os = "android") { 1 } else { 0 };

/// 一张封套:它自己与它的倒影,各一个实体、各一份材质。
struct Slot {
    slab: Entity,
    mirror: Entity,
    slab_material: Handle<SleeveMaterial>,
    mirror_material: Handle<SleeveMaterial>,
}

/// 封面实物这一摊:台面、两张封套(在场的与退场的)、地面,以及它们的运动状态。
pub(crate) struct Sleeve {
    stage: Entity,
    current: Slot,
    outgoing: Slot,
    /// 没有封面时挂的占位图。
    placeholder: Handle<Image>,
    /// 在场那张封套正面印的封面(没有就是占位图)。
    cover: Handle<Image>,
    tilt: motion::Tilt,
    exchange: motion::Exchange,
    glow: motion::Glow,
    reveal: motion::Reveal,
    /// 退场那张封套正面的显影程度,交接开始那一刻从在场那张抄过去。
    outgoing_reveal: f32,
}

/// 驱动封面实物一帧要的输入。
pub(crate) struct Drive {
    /// 播放页时钟,秒。
    pub time: f32,
    /// 距上一帧的秒数(播放页时钟)。
    pub delta: f32,
    /// 这一帧的低频电平,0..=1。
    pub bass: f32,
    /// 离屏目标的物理像素尺寸,台面按它缩放。
    pub width: u32,
    pub height: u32,
    pub compact: bool,
}

impl Sleeve {
    /// 建好全部网格、材质与实体。几何一共几百个顶点,建一次不再动。
    pub(crate) fn spawn(app: &mut App) -> Self {
        let world = app.world_mut();
        let (slab_mesh, mirror_mesh, floor_mesh) = {
            let mut meshes =
                world.resource_mut::<Assets<Mesh>>();
            (
                meshes.add(build_mesh(mesh::slab())),
                meshes.add(build_mesh(mesh::mirrored(
                    mesh::slab(),
                ))),
                meshes.add(build_mesh(mesh::floor())),
            )
        };
        let placeholder = world
            .resource_mut::<Assets<Image>>()
            .add(Image::default());

        let [
            slab_material,
            mirror_material,
            old_slab_material,
            old_mirror_material,
            floor_material,
        ] = {
            let mut materials = world
                .resource_mut::<Assets<SleeveMaterial>>();
            let mut material =
                |kind: u32, translucent: bool| {
                    materials.add(SleeveMaterial {
                        params: SleeveParams {
                            kind,
                            dither: DITHER,
                            footprint: Vec4::from_array(
                                mesh::footprint(),
                            ),
                            ..default()
                        },
                        cover: placeholder.clone(),
                        translucent,
                    })
                };
            [
                material(KIND_SLAB, false),
                material(KIND_MIRROR, true),
                material(KIND_SLAB, false),
                material(KIND_MIRROR, true),
                material(KIND_FLOOR, true),
            ]
        };

        let stage = world
            .spawn((
                Transform::default(),
                Visibility::Inherited,
            ))
            .id();
        let mut part =
            |mesh: &Handle<Mesh>,
             material: &Handle<SleeveMaterial>,
             transform: Transform,
             visibility: Visibility| {
                world
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        transform,
                        visibility,
                        // 相机移轴之后,bevy 的视锥仍按不移轴的那一幅算(见 `camera::lens`),
                        // 偏到一边的东西会被误剔。台面总共五个实体,不剔也不值一提。
                        NoFrustumCulling,
                        ChildOf(stage),
                    ))
                    .id()
            };
        let current = Slot {
            slab: part(
                &slab_mesh,
                &slab_material,
                Transform::default(),
                Visibility::Inherited,
            ),
            mirror: part(
                &mirror_mesh,
                &mirror_material,
                Transform::default(),
                Visibility::Inherited,
            ),
            slab_material,
            mirror_material,
        };
        // 退场那张平时藏着,只在交接期间露面。
        let outgoing = Slot {
            slab: part(
                &slab_mesh,
                &old_slab_material,
                Transform::default(),
                Visibility::Hidden,
            ),
            mirror: part(
                &mirror_mesh,
                &old_mirror_material,
                Transform::default(),
                Visibility::Hidden,
            ),
            slab_material: old_slab_material,
            mirror_material: old_mirror_material,
        };
        part(
            &floor_mesh,
            &floor_material,
            Transform::from_xyz(0.0, FLOOR_Y, 0.0),
            Visibility::Inherited,
        );

        Self {
            stage,
            current,
            outgoing,
            cover: placeholder.clone(),
            placeholder,
            tilt: motion::Tilt::default(),
            exchange: motion::Exchange::default(),
            glow: motion::Glow::default(),
            reveal: motion::Reveal::default(),
            outgoing_reveal: 0.0,
        }
    }

    /// 整座台面显示或藏起来。藏起来的实体不进任何渲染阶段,一帧不画。
    pub(crate) fn set_visible(
        &self,
        app: &mut App,
        visible: bool,
    ) {
        app.world_mut().entity_mut(self.stage).insert(
            if visible {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            },
        );
    }

    /// 新封面到了。
    ///
    /// 正面正印着一张(这首歌没经过「清空」,新封面直接顶了上来):起一次交接,旧的
    /// 甩走,新的落位,新的那张直接印满。正面是空的(先换歌、封面后到 —— 绝大多数
    /// 情形):封套不动,封面在它上面显影,不再甩一次。
    pub(crate) fn show(
        &mut self,
        app: &mut App,
        cover: Handle<Image>,
    ) {
        if self.reveal.printed() {
            self.begin_exchange(app);
            self.reveal.snap(1.0);
        } else {
            self.reveal.fade_to(1.0);
        }
        self.cover = cover;
        self.print(app);
    }

    /// 换歌了,封面还没到。正面印着上一首的话,起一次交接:上一首甩走,落位的是
    /// 一张空封套 —— 旧封面配新歌比空着更误导(`CONTEXT.md`「封面点云」同一条)。
    pub(crate) fn clear(&mut self, app: &mut App) {
        if !self.reveal.printed() {
            return;
        }
        self.begin_exchange(app);
        self.reveal.snap(0.0);
        self.cover = self.placeholder.clone();
        self.print(app);
    }

    /// 把在场那张交给退场位:它的封面与显影程度原样抄过去,再起计时。
    fn begin_exchange(&mut self, app: &mut App) {
        self.outgoing_reveal = self.reveal.value();
        let cover = self.cover.clone();
        let mut materials = app
            .world_mut()
            .resource_mut::<Assets<SleeveMaterial>>();
        for handle in [
            &self.outgoing.slab_material,
            &self.outgoing.mirror_material,
        ] {
            if let Some(mut material) =
                materials.get_mut(handle)
            {
                material.cover = cover.clone();
            }
        }
        self.exchange.start();
    }

    /// 把在场那张的封面句柄写进它的两份材质。
    fn print(&self, app: &mut App) {
        let mut materials = app
            .world_mut()
            .resource_mut::<Assets<SleeveMaterial>>();
        for handle in [
            &self.current.slab_material,
            &self.current.mirror_material,
        ] {
            if let Some(mut material) =
                materials.get_mut(handle)
            {
                material.cover = self.cover.clone();
            }
        }
    }

    /// 不在显示时收到的换歌,直接落到终态:交接走完、显影到位、退场那张藏好并放掉封面。
    ///
    /// 运动只在台面露面时推进(`drive` 只为当前效果调)。不收尾的话,用户在点云上
    /// 换了几首歌再切回来,会看到一张早该退场的封套这时才被甩出去。
    pub(crate) fn settle(&mut self, app: &mut App) {
        self.reveal.settle();
        if !self.exchange.running() {
            return;
        }
        self.exchange.finish();
        let world = app.world_mut();
        for entity in
            [self.outgoing.slab, self.outgoing.mirror]
        {
            world
                .entity_mut(entity)
                .insert(Visibility::Hidden);
        }
        let mut materials =
            world.resource_mut::<Assets<SleeveMaterial>>();
        for handle in [
            &self.outgoing.slab_material,
            &self.outgoing.mirror_material,
        ] {
            if let Some(mut material) =
                materials.get_mut(handle)
            {
                material.cover = self.placeholder.clone();
            }
        }
    }

    /// 拖了 `(dx, dy)` 物理像素(按住但没动时为 0)。
    pub(crate) fn drag(
        &mut self,
        dx: f32,
        dy: f32,
        dt: f32,
    ) {
        self.tilt.drag(dx, dy, dt);
    }

    /// 松手:交给弹簧拉回原位。
    pub(crate) fn release(&mut self) {
        self.tilt.release();
    }

    /// 一帧:推进全部运动状态,写台面与四张封套的位姿,再写材质 uniform。
    pub(crate) fn drive(
        &mut self,
        app: &mut App,
        frame: &Drive,
    ) {
        let was_running = self.exchange.running();
        self.tilt.step(frame.delta);
        self.exchange.advance(frame.delta);
        self.glow.follow(frame.bass, frame.delta);
        self.reveal.advance(frame.delta);

        let scale = motion::stage_scale(
            frame.width,
            frame.height,
            frame.compact,
        );
        let stage = Transform {
            translation: Vec3::new(
                0.0,
                motion::stage_lift(),
                0.0,
            ),
            rotation: Quat::from_rotation_x(
                motion::STAGE_PITCH,
            ),
            scale: Vec3::splat(scale),
        };

        let (idle_pitch, idle_yaw) =
            motion::idle(frame.time);
        let (drag_pitch, drag_yaw) = self.tilt.angles();
        let (pitch, yaw) =
            (idle_pitch + drag_pitch, idle_yaw + drag_yaw);
        let e = self.exchange.progress();
        let running = self.exchange.running();
        let now = pose(motion::incoming(e), pitch, yaw);
        let old = pose(motion::outgoing(e), pitch, yaw);

        // 地面平面的世界表示:台面的 +y 转到世界里,过地面上那一点。
        let normal = stage.rotation * Vec3::Y;
        let on_floor = stage
            .transform_point(Vec3::new(0.0, FLOOR_Y, 0.0));
        let plane = normal.extend(normal.dot(on_floor));

        let world = app.world_mut();
        world.entity_mut(self.stage).insert(stage);
        for (entity, transform) in [
            (self.current.slab, now),
            (self.current.mirror, mirror(&now)),
            (self.outgoing.slab, old),
            (self.outgoing.mirror, mirror(&old)),
        ] {
            world.entity_mut(entity).insert(transform);
        }
        // 退场那张只在交接期间露面;交接刚结束那一帧藏起来,并放掉它的封面,
        // 好让上一首的纹理没人引用、随句柄一起释放。
        if running || was_running {
            let visibility = if running {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            for entity in
                [self.outgoing.slab, self.outgoing.mirror]
            {
                world.entity_mut(entity).insert(visibility);
            }
        }

        let glow = self.glow.level();
        let tilt = Vec2::new(yaw, pitch);
        let placeholder = self.placeholder.clone();
        let mut materials =
            world.resource_mut::<Assets<SleeveMaterial>>();
        let mut write =
            |handle: &Handle<SleeveMaterial>,
             facing: Vec3,
             cover: f32| {
                if let Some(mut material) =
                    materials.get_mut(handle)
                {
                    let p = &mut material.params;
                    p.plane = plane;
                    p.facing = facing.extend(0.0);
                    p.tilt = tilt;
                    p.cover = cover;
                    p.glow = glow;
                    p.scale = scale;
                }
            };
        let facing = |local: &Transform| {
            stage.rotation * local.rotation * Vec3::Z
        };
        let reveal = self.reveal.value();
        write(
            &self.current.slab_material,
            facing(&now),
            reveal,
        );
        write(
            &self.current.mirror_material,
            facing(&mirror(&now)),
            reveal,
        );
        if running {
            write(
                &self.outgoing.slab_material,
                facing(&old),
                self.outgoing_reveal,
            );
            write(
                &self.outgoing.mirror_material,
                facing(&mirror(&old)),
                self.outgoing_reveal,
            );
        } else if was_running {
            for handle in [
                &self.outgoing.slab_material,
                &self.outgoing.mirror_material,
            ] {
                if let Some(mut material) =
                    materials.get_mut(handle)
                {
                    material.cover = placeholder.clone();
                }
            }
        }
    }
}

/// 一张封套在台面里的位姿:交接偏移叠上呼吸与拖动的偏角。
fn pose(
    offset: motion::Offset,
    pitch: f32,
    yaw: f32,
) -> Transform {
    Transform {
        translation: offset.translation,
        rotation: Quat::from_euler(
            EulerRot::YXZ,
            yaw + offset.yaw,
            pitch + offset.pitch,
            offset.roll,
        ),
        scale: Vec3::ONE,
    }
}

/// 一张封套关于地面(台面里 y = `FLOOR_Y`)的倒影位姿,配镜像过的那份网格用。
///
/// 镜像 F 关于 y 反射;倒影 = F · 封套 · F(后一个 F 已烘进网格,见 [`mesh::mirrored`])。
/// 两次反射合起来是一个正常的旋转:四元数 (x, y, z, w) 变成 (−x, y, −z, w),
/// 平移的 y 关于地面翻过去。
fn mirror(source: &Transform) -> Transform {
    let q = source.rotation;
    Transform {
        translation: Vec3::new(
            source.translation.x,
            2.0 * FLOOR_Y - source.translation.y,
            source.translation.z,
        ),
        rotation: Quat::from_xyzw(-q.x, q.y, -q.z, q.w),
        scale: source.scale,
    }
}

/// 把顶点数据烘成一份 bevy `Mesh`:位置、法线、uv,走 bevy 默认的顶点着色器。
fn build_mesh(vertices: mesh::Vertices) -> Mesh {
    let mut built = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    built.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vertices.positions,
    );
    built.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vertices.normals,
    );
    built.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vertices.uvs,
    );
    built.insert_indices(Indices::U32(vertices.indices));
    built
}

#[cfg(test)]
mod tests;
