//! 导航侧栏液态玻璃选中器的**独立 wgpu pass**(不经 bevy)。
//!
//! 为什么不长在 bevy 里:选中器是纯 2D 玻璃,没有 3D 场景、没有 ECS,拖上 bevy 的相机与
//! 提取管线只是负担。而 texture→`slint::Image` 的桥(`Image::try_from(wgpu::Texture)`)本就
//! 与 bevy 无关 —— 所以这里在同一块**共享 device** 上自起一个全屏片元 pass,画进一张离屏
//! 纹理,导入 Slint 当侧栏背景。shader 见 `navglass.wgsl`,思路见架构文档第八节。
//!
//! 只在切 tab 的转场期间被 UI 侧调用(省电门在 `ui::nav_glass`),静止时 Slint 复用上一帧纹理。

use slint::wgpu_29::wgpu;

/// 一帧导航选中器的控制量,**物理像素**。POD,由 ui 侧组装(见 `ui::NavGlassControls`),
/// 在 apps/* 的 seam 处平凡拷来 —— 与 `SceneParams` 同样的镜像分离(ui 与 render3d 互不依赖)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavParams {
    /// 导航条纹理尺寸(= 目标纹理尺寸)。
    pub strip_w: f32,
    pub strip_h: f32,
    /// 三颗球中心在移动轴上的位置,相对条首:头(快)/尾(慢)/小水滴(最慢)。
    /// 追随系数不同,同一个目标位置天然拉开先后(§11)。
    pub lead: f32,
    pub lag: f32,
    pub drop: f32,
    /// 三颗球在固定轴上的中心。侧栏是栏宽一半,底栏由 ui 侧按 inset 算好。
    pub cross: f32,
    /// 移动轴上一格的长度(侧栏是项高,底栏是格宽),用来定块的半尺寸/圆角/融合半径。
    pub slot: f32,
    /// 固定轴上的可用厚度(侧栏是栏宽,底栏是图标那一带的高,不含手势条 inset)。
    pub thick: f32,
    /// 移动轴是 x(手机底栏)还是 y(宽版式侧栏)。
    pub horizontal: bool,
    /// 三球的整体缩放,1 常态、0 缩没(侧栏底部两颗圆钮不在轨道上,#71)。
    pub ball: f32,
    /// 深色主题。导航背景由本 shader 自绘,所以主题得穿进 uniform ——
    /// 采不到背后的像素,没法"跟着背景走"。
    pub dark: bool,
}

/// uniform 缓冲字节数。对齐到 16 的整数倍(navglass.wgsl 的 Params 实占 52 字节,余下填 0)。
const UBO_BYTES: u64 = 64;

/// 导航选中器的 wgpu pass:持有管线与一张随尺寸重建的离屏目标纹理,渲染一帧后返回其
/// 包装成的 `slint::Image`。目标纹理身份稳定期间只导入一次,之后每帧重画同一张,Slint 实时采样。
pub struct NavGlassPass {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    ubo: wgpu::Buffer,
    /// (纹理, 宽, 高):尺寸变了就重建并重新导入。
    target: Option<(wgpu::Texture, u32, u32)>,
    /// 当前目标纹理导入 Slint 的结果,尺寸不变时复用。
    image: Option<slint::Image>,
}

impl NavGlassPass {
    /// 在共享 device 上建好管线与 uniform 缓冲。device/queue 由 `Scene` 暴露(同一套共享 wgpu)。
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let shader = device.create_shader_module(
            wgpu::ShaderModuleDescriptor {
                label: Some("navglass-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("navglass.wgsl").into(),
                ),
            },
        );

        let bind_layout = device.create_bind_group_layout(
            &wgpu::BindGroupLayoutDescriptor {
                label: Some("navglass-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            },
        );

        let ubo =
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("navglass-ubo"),
                size: UBO_BYTES,
                usage: wgpu::BufferUsages::UNIFORM
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

        let bind_group = device.create_bind_group(
            &wgpu::BindGroupDescriptor {
                label: Some("navglass-bg"),
                layout: &bind_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubo.as_entire_binding(),
                }],
            },
        );

        let pipeline_layout = device
            .create_pipeline_layout(
                &wgpu::PipelineLayoutDescriptor {
                    label: Some("navglass-pl"),
                    // 此 wgpu-29 的字段是 &[Option<&_>](稀疏 set 支持),不是 &[&_]。
                    bind_group_layouts: &[Some(
                        &bind_layout,
                    )],
                    // 不用 immediate/push-constant 数据。
                    immediate_size: 0,
                },
            );

        let pipeline = device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("navglass-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        // 与 bevy 离屏目标同格式(见 make_target):满足 Slint 导入要求。
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                // 不用 multiview,取 None(此 wgpu-29 的字段名是 multiview_mask)。
                multiview_mask: None,
                cache: None,
            },
        );

        Self {
            device,
            queue,
            pipeline,
            bind_group,
            ubo,
            target: None,
            image: None,
        }
    }

    /// 渲染一帧选中器,返回侧栏背景纹理包装成的 `slint::Image`。
    pub fn render_frame(
        &mut self,
        p: &NavParams,
    ) -> slint::Image {
        let w = (p.strip_w.max(1.0)) as u32;
        let h = (p.strip_h.max(1.0)) as u32;

        // 尺寸变了(首帧/窗口缩放)→ 重建目标纹理并重新导入 Slint;否则重画同一张。
        let need_new = self
            .target
            .as_ref()
            .is_none_or(|(_, tw, th)| *tw != w || *th != h);
        if need_new {
            let tex =
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("navglass-target"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
            match slint::Image::try_from(tex.clone()) {
                Ok(img) => self.image = Some(img),
                Err(e) => log::error!(
                    "navglass 纹理导入 Slint 失败: {e:?}"
                ),
            }
            self.target = Some((tex, w, h));
        }

        // 组 uniform(物理像素):三球沿移动轴走,固定轴上停在 cross;块的半尺寸
        // 一边按格长、一边按条的厚度取,轴对调时两者互换(#70)。
        let thick = p.thick.max(1.0);
        let ball = |pos: f32| {
            if p.horizontal {
                [pos, p.cross]
            } else {
                [p.cross, pos]
            }
        };
        // ball 是整体缩放:0 时半尺寸归零,SDF 退化成一个点,画面上只剩自绘背景。
        let s = p.ball.clamp(0.0, 1.0);
        // 融合半径按短边取:底栏一格比条厚得多(手机上 91 对 64),按格宽算出来的
        // 融合半径比条本身还宽,三球会糊成一团淌出格外。
        // 也跟着 ball 缩:不缩的话球都没了、颈还在,收尾那几帧会剩一团糊影。
        let smooth_k =
            (p.slot.min(thick) * 0.9 * s).max(0.001);
        // smin 把等值面往外推约 k/4(三球同心时 h=0.5,场值整体低 k/4),所以
        // 半尺寸先扣掉这一圈,画出来才是这里写的这个框。不扣的话球会胀出格外:
        // 侧栏里表现为胶囊顶满栏宽,底栏里表现为贴着屏幕边。
        let grow = smooth_k * 0.25;
        let box_half =
            |v: f32| (v * 0.42 * s - grow).max(1.0);
        let half = if p.horizontal {
            [box_half(p.slot), box_half(thick)]
        } else {
            [box_half(thick), box_half(p.slot)]
        };
        let radius = half[0].min(half[1]) * 0.6;
        let lead = ball(p.lead);
        let lag = ball(p.lag);
        let drop = ball(p.drop);
        // 一行一个 uniform,与 shader 里 UBO 的字段逐行对照 —— 那边是 vec2,
        // 这边就并排两个标量。rustfmt 会把它摊成一行一个数,对照关系随之消失,
        // 所以在这里按住它。全仓唯一一处。
        #[rustfmt::skip]
        let vals: [f32; 16] = [
            w as f32, h as f32, // tex_size
            lead[0], lead[1], // lead
            lag[0], lag[1], // lag
            drop[0], drop[1], // drop
            half[0], half[1], // half
            radius, smooth_k, // radius, smooth_k
            if p.dark { 1.0 } else { 0.0 }, // dark
            if p.horizontal { 1.0 } else { 0.0 }, // horizontal
            0.0, 0.0, // 尾部对齐填充
        ];
        let mut bytes =
            Vec::with_capacity(UBO_BYTES as usize);
        for v in vals {
            bytes.extend_from_slice(&v.to_ne_bytes());
        }
        self.queue.write_buffer(&self.ubo, 0, &bytes);

        let (tex, _, _) = self.target.as_ref().unwrap();
        let view = tex.create_view(
            &wgpu::TextureViewDescriptor::default(),
        );
        let mut enc = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("navglass-encoder"),
            },
        );
        {
            let mut rp = enc.begin_render_pass(
                &wgpu::RenderPassDescriptor {
                    label: Some("navglass-pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                // 片元覆盖每个像素,清成透明即可。
                                load: wgpu::LoadOp::Clear(
                                    wgpu::Color::TRANSPARENT,
                                ),
                                store: wgpu::StoreOp::Store,
                            },
                        },
                    )],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    // 不用 multiview,取 None(此 wgpu-29 的 RenderPassDescriptor 多这个字段)。
                    multiview_mask: None,
                },
            );
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &self.bind_group, &[]);
            rp.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);

        self.image.clone().unwrap_or_default()
    }
}
