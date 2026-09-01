//! 卡墙的 bevy 场景侧(docs/adr/0025)。
//!
//! 几何与动力学的真相全在 `ui::wall`:那边把每张卡的**最终世界位姿**
//! (yaw/pitch/散布/塌回全部合成完)与相机位置经 POD seam 传来,这里只做
//! 三件事:把位姿摆进场景、把封面像素灌进材质、渲一帧交回 `slint::Image`。
//! 相机不带任何旋转 —— 透视原点 42% 的效果由相机下移 8% 实现,与 ui 侧
//! `wall::project` 严格同构,命中测试与画面才不会各说各话。
//!
//! 卡是 unlit 的 `StandardMaterial` 方片,圆角、描边、投影已由 ui 侧烘进
//! 纹理(见 `ui::wall::bake_card`),这里不需要为它们开自定义材质。唯一的
//! 例外是**正在放的那一张**:它换上 [`foil::FoilMaterial`],光泽常驻流动。
//! 卡墙实体挂在 [`WALL_LAYER`] 上,点云的两台相机看不见它,它的相机也
//! 看不见点云。
//!
//! 省电门在 ui 侧:静止的墙根本不会调到这里。

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;

use bevy::camera::{ClearColorConfig, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;

use crate::foil::{FoilMaterial, FoilParams};

/// 卡墙专用渲染层。0 是点云用的默认层。
const WALL_LAYER: usize = 1;

/// 一张卡这一帧的世界位姿,**物理像素**坐标(x 右、y 下、z 朝观者)。
/// POD,镜像 `ui::wall` 的世界位姿输出,apps/* 在 seam 处平凡拷来。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallCard {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// 弧度。
    pub rot_y: f32,
    pub rot_x: f32,
    /// 随深度压暗的系数(1 = 原亮度)。
    pub dim: f32,
    /// 卡边长。
    pub size: f32,
}

/// 一帧卡墙的相机与目标尺寸。
///
/// 没有平移:环面回绕已经把每张卡折算到「相对镜头」的位置上(见
/// `ui::wall::world_pose`),相机因此永远在原点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallCamera {
    pub dolly: f32,
    /// CSS 意义的透视距离,恒按容器宽缩放(ui 侧算好)。
    pub perspective: f32,
}

/// 新到的一张卡面:`slot` 对应 cards 下标。圆角、描边、投影已在 alpha 里。
#[derive(Clone, Debug)]
pub struct WallCover {
    pub slot: usize,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// 这张是封面还没到时的空白卡面。纯白,占位底色乘上去才有颜色。
    pub blank: bool,
}

/// 一帧卡墙的全部输入。
#[derive(Clone, Debug, Default)]
pub struct WallFrame {
    pub width: u32,
    pub height: u32,
    pub cam: WallCamera,
    /// 正在放的那首歌占的槽位,只有它走闪卡材质。
    pub foil: Option<usize>,
    pub cards: Vec<WallCard>,
    pub covers: Vec<WallCover>,
}

impl Default for WallCamera {
    fn default() -> Self {
        Self {
            dolly: 0.0,
            perspective: 1200.0,
        }
    }
}

/// 卡墙在 [`Scene`] 里的那一摊状态。跟 Scene 的其余字段分开放,
/// 免得点云那边的字段清单再长一截。
pub(crate) struct WallScene {
    target: Handle<Image>,
    camera: Entity,
    /// 卡实体池,按需增长,多出的藏掉(Visibility::Hidden)。
    cards: Vec<Entity>,
    materials: Vec<Handle<StandardMaterial>>,
    /// 各槽位的卡面纹理。闪卡换材质时要把同一张图接过去。
    textures: Vec<Option<Handle<Image>>>,
    /// 闪卡材质与它此刻挂在哪一格。只有一份 —— 同时只有一首歌在放。
    foil: Handle<FoilMaterial>,
    foil_slot: Option<usize>,
    /// 已驱动的帧数,闪卡的时钟。墙可见时恒满帧,除以 60 就是秒。
    frames: u32,
    /// 每张卡的底色(占位副色板;封面到了换成白,让纹理原色出来)。
    /// dim 每帧乘在它上面,别把占位色冲掉。
    tints: Vec<[f32; 3]>,
    size: (u32, u32),
    image: Option<slint::Image>,
    image_key: Option<(u32, u32)>,
}

impl WallScene {
    /// 建目标纹理与相机(初始不激活:音乐页不开卡墙就一帧也不渲)。
    pub(crate) fn new(app: &mut App) -> Self {
        let target = crate::scene::make_target(app, 4, 4);
        let foil = app
            .world_mut()
            .resource_mut::<Assets<FoilMaterial>>()
            .add(FoilMaterial {
                params: FoilParams::default(),
                cover: Handle::default(),
            });
        let camera = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Camera {
                    is_active: false,
                    // 底色由 Slint 画,墙外像素透明。
                    clear_color: ClearColorConfig::Custom(
                        Color::NONE,
                    ),
                    ..default()
                },
                RenderTarget::Image(target.clone().into()),
                Tonemapping::None,
                RenderLayers::layer(WALL_LAYER),
                Transform::default(),
            ))
            .id();
        Self {
            target,
            camera,
            cards: Vec::new(),
            materials: Vec::new(),
            textures: Vec::new(),
            foil,
            foil_slot: None,
            frames: 0,
            tints: Vec::new(),
            size: (4, 4),
            image: None,
            image_key: None,
        }
    }

    /// 激活/停用卡墙相机。点云与卡墙互斥渲染,由 Scene 的两个入口互相关灯。
    pub(crate) fn set_active(
        &self,
        app: &mut App,
        active: bool,
    ) {
        if let Some(mut cam) =
            app.world_mut().get_mut::<Camera>(self.camera)
        {
            cam.is_active = active;
        }
    }

    /// 应用一帧输入:尺寸、相机、卡位姿、新封面。
    pub(crate) fn apply(
        &mut self,
        app: &mut App,
        frame: &WallFrame,
    ) {
        if frame.width > 0
            && frame.height > 0
            && (frame.width, frame.height) != self.size
        {
            self.resize(app, frame.width, frame.height);
        }
        let cam = frame.cam;
        self.frames = self.frames.wrapping_add(1);

        // 相机:世界单位 = 物理像素,距 z=0 平面 perspective - dolly,
        // 下移 8% 高(透视原点 42%),竖直视野由目标高反推。
        // x 恒 0 —— 平移已经折进每张卡的回绕位置里了。
        let h = self.size.1 as f32;
        let dist = (cam.perspective - cam.dolly).max(2.0);
        if let Some(mut t) = app
            .world_mut()
            .get_mut::<Transform>(self.camera)
        {
            *t = Transform::from_xyz(0.0, -0.08 * h, dist);
        }
        let fov = 2.0 * (h * 0.5 / cam.perspective).atan();
        app.world_mut().entity_mut(self.camera).insert(
            Projection::Perspective(
                PerspectiveProjection {
                    fov,
                    near: 10.0,
                    far: 8000.0,
                    ..default()
                },
            ),
        );

        self.sync_cards(app, &frame.cards);
        for cover in &frame.covers {
            self.apply_cover(app, cover);
        }
        self.sync_foil(app, frame);
    }

    /// 让闪卡材质挂在这一帧该挂的那一格上,并推进它的时钟。
    ///
    /// 换材质靠增删组件:一个实体同时挂两种 `MeshMaterial3d` 会被画两遍。
    fn sync_foil(
        &mut self,
        app: &mut App,
        frame: &WallFrame,
    ) {
        let want = frame.foil.filter(|&i| {
            i < self.cards.len() && i < frame.cards.len()
        });
        if want != self.foil_slot {
            if let Some(old) = self.foil_slot
                && let Some(entity) = self.cards.get(old)
            {
                let mut e =
                    app.world_mut().entity_mut(*entity);
                e.remove::<MeshMaterial3d<FoilMaterial>>();
                e.insert(MeshMaterial3d(
                    self.materials[old].clone(),
                ));
            }
            if let Some(new) = want
                && let Some(entity) = self.cards.get(new)
            {
                let mut e =
                    app.world_mut().entity_mut(*entity);
                e.remove::<MeshMaterial3d<StandardMaterial>>();
                e.insert(MeshMaterial3d(self.foil.clone()));
            }
            self.foil_slot = want;
        }
        let Some(slot) = self.foil_slot else { return };
        let cover = self.textures[slot].clone();
        let dim =
            frame.cards.get(slot).map_or(1.0, |c| c.dim);
        if let Some(mut mat) = app
            .world_mut()
            .resource_mut::<Assets<FoilMaterial>>()
            .get_mut(&self.foil)
        {
            mat.params.time = self.frames as f32 / 60.0;
            mat.params.dim = dim;
            if let Some(handle) = cover {
                mat.cover = handle;
            }
        }
    }

    /// 池子对齐这一帧的卡数,逐张摆位姿。ui 的 y 向下,bevy 向上,翻号。
    fn sync_cards(
        &mut self,
        app: &mut App,
        cards: &[WallCard],
    ) {
        while self.cards.len() < cards.len() {
            let idx = self.cards.len();
            let mesh = app
                .world_mut()
                .resource_mut::<Assets<Mesh>>()
                .add(Mesh::from(Rectangle::new(1.0, 1.0)));
            let material = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(card_material());
            let entity = app
                .world_mut()
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(material.clone()),
                    RenderLayers::layer(WALL_LAYER),
                    Transform::default(),
                    Visibility::Hidden,
                ))
                .id();
            self.cards.push(entity);
            self.materials.push(material);
            self.textures.push(None);
            self.tints.push(placeholder_tint(idx));
        }

        for (i, entity) in self.cards.iter().enumerate() {
            let Some(card) = cards.get(i) else {
                if let Some(mut vis) =
                    app.world_mut()
                        .get_mut::<Visibility>(*entity)
                {
                    *vis = Visibility::Hidden;
                }
                continue;
            };
            if let Some(mut vis) = app
                .world_mut()
                .get_mut::<Visibility>(*entity)
            {
                *vis = Visibility::Visible;
            }
            if let Some(mut t) = app
                .world_mut()
                .get_mut::<Transform>(*entity)
            {
                *t = Transform {
                    translation: Vec3::new(
                        card.x, -card.y, card.z,
                    ),
                    // ui 的 rot_x 以「顶边向后」为正(y 下坐标系),
                    // 翻到 bevy 的 y 上坐标系要变号;rot_y 不变。
                    rotation: Quat::from_euler(
                        EulerRot::YXZ,
                        card.rot_y,
                        -card.rot_x,
                        0.0,
                    ),
                    scale: Vec3::splat(card.size),
                };
            }
            if self.foil_slot == Some(i) {
                // 闪卡的亮度走它自己的 uniform,见 sync_foil。
                continue;
            }
            if let Some(mut mat) = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .get_mut(&self.materials[i])
            {
                let [r, g, b] = self.tints[i];
                let d = card.dim;
                mat.base_color =
                    Color::linear_rgb(r * d, g * d, b * d);
            }
        }
    }

    /// 封面像素 → bevy 纹理 → 对应槽位的材质。
    fn apply_cover(
        &mut self,
        app: &mut App,
        cover: &WallCover,
    ) {
        let expected =
            (cover.width * cover.height * 4) as usize;
        if cover.rgba.len() != expected
            || cover.slot >= self.materials.len()
        {
            return;
        }
        let image = Image::new(
            bevy::render::render_resource::Extent3d {
                width: cover.width,
                height: cover.height,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            cover.rgba.clone(),
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        let handle = app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(image);
        if let Some(mut mat) = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .get_mut(&self.materials[cover.slot])
        {
            mat.base_color_texture = Some(handle.clone());
        }
        self.textures[cover.slot] = Some(handle);
        // 空白卡面是纯白,占位底色乘在它上面才有颜色;真封面到了换回白,
        // 让封面原色出场,底色只留 dim。
        if !cover.blank {
            self.tints[cover.slot] = [1.0, 1.0, 1.0];
        }
    }

    fn resize(
        &mut self,
        app: &mut App,
        width: u32,
        height: u32,
    ) {
        let new_target =
            crate::scene::make_target(app, width, height);
        app.world_mut().entity_mut(self.camera).insert(
            RenderTarget::Image(new_target.clone().into()),
        );
        let old =
            std::mem::replace(&mut self.target, new_target);
        app.world_mut()
            .resource_mut::<Assets<Image>>()
            .remove(&old);
        self.size = (width, height);
        self.image = None;
        self.image_key = None;
    }

    /// update 之后把目标纹理(按身份缓存)包装成 Image。
    pub(crate) fn finish(
        &mut self,
        app: &App,
    ) -> slint::Image {
        let Some(tex) = crate::scene::extract_texture(
            app,
            &self.target,
        ) else {
            return self.image.clone().unwrap_or_default();
        };
        let key = (tex.width(), tex.height());
        if self.image_key != Some(key) {
            match slint::Image::try_from(tex) {
                Ok(img) => {
                    log::info!(
                        "render3d: 卡墙纹理 {}x{} 已导入 Slint",
                        key.0,
                        key.1
                    );
                    self.image = Some(img);
                    self.image_key = Some(key);
                }
                Err(e) => log::error!(
                    "卡墙纹理导入 Slint 失败: {e:?}"
                ),
            }
        }
        self.image.clone().unwrap_or_default()
    }
}

/// 封面没到之前的占位底色:副色板灰绿,与列表占位同气质。
fn placeholder_tint(index: usize) -> [f32; 3] {
    let t = (index % 12) as f32 / 12.0;
    let g = 0.18
        + 0.10 * (t * std::f32::consts::TAU).sin().abs();
    [0.10 + 0.04 * t, g, 0.12]
}

/// 卡片材质:不受光、可透明(圆角与投影在纹理 alpha 里)。
///
/// **安卓走 alpha 测试,桌面走混合。** Adreno 上 `Blend` 的元素整片不显示
/// 是本仓的老坑(见 docs/adr/0012),2026-08-30 在努比亚平板上复验过:整面墙
/// 一张卡都画不出来,而同一批曲目在列表里封面齐全 —— 换成 `Mask` 立刻正常。
/// 换了设备、换了芯片,这个坑还在。
///
/// 代价是安卓上没有投影:投影的峰值 alpha 是 0.42,整段低于 0.5 的门槛,
/// 会被 alpha 测试整个丢掉 —— 干净地消失,而不是切成硬边光晕。圆角也从
/// 软边变硬边。描边不受影响,它长在 alpha 为 1 的卡面里。
fn card_material() -> StandardMaterial {
    StandardMaterial {
        unlit: true,
        #[cfg(target_os = "android")]
        alpha_mode: AlphaMode::Mask(0.5),
        #[cfg(not(target_os = "android"))]
        alpha_mode: AlphaMode::Blend,
        ..default()
    }
}
