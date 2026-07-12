//! 液态玻璃后处理 pass:在 bevy 产出的离屏纹理上,对一个圆角矩形区域做模糊 + 边缘折射,
//! 结果写进另一张同尺寸纹理,再由 Slint 采样。
//!
//! 存在的理由:**Slint 没有 backdrop blur,也拿不到自己渲染的像素**(`GraphicsAPI::WGPU29`
//! 只给 instance/device/queue,没有 surface texture)。但玻璃背后这块背景是我们自己在 GPU 上
//! 画的,所以我们能采样它 —— 这就是 `docs/slint/visual-effects-and-shaders.md` 第五节说的
//! "把顺序翻过来":背景层归 GPU,控件层归 Slint,玻璃在两者之间的 GPU 层里合成。
//!
//! 代价同样写在那篇文档里:**玻璃背后不能有 Slint 控件** —— 这里模糊的只是 bevy 的画面。

use slint::wgpu_29::wgpu;

/// 要做成玻璃的那个圆角矩形,**物理像素**,坐标系与离屏纹理一致(左上角为原点)。
///
/// 由 UI 侧给出(`app.slint` 里工具条的几何量 × 窗口缩放系数),不在这里重复那些常量 ——
/// 否则 .slint 改了留白,shader 这边会静默错位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

impl GlassRect {
    /// 宽高有一个不为正就没什么可做的(3D 页未激活、或面板还没量出尺寸)。
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// 模糊半径(物理像素)。够糊出磨砂感,又不至于把工具条背后糊成一片死白。
const BLUR_PX: f32 = 14.0;

/// uniform 的字段数:rect(4) + tex_size(2) + radius + blur。
/// 顺序必须与 glass.wgsl 里的 `struct Params` 逐字段对齐 —— vec4 要 16 字节对齐,
/// 这么排下来正好 32 字节,不需要填充。
const UNIFORM_FLOATS: usize = 8;

/// 把 uniform 打成字节。没引 bytemuck —— 八个 f32 而已,三行的事。
fn uniform_bytes(
    data: [f32; UNIFORM_FLOATS],
) -> [u8; UNIFORM_FLOATS * 4] {
    let mut out = [0u8; UNIFORM_FLOATS * 4];
    for (i, v) in data.iter().enumerate() {
        out[i * 4..i * 4 + 4]
            .copy_from_slice(&v.to_ne_bytes());
    }
    out
}

/// 后处理 pass 的常驻资源。管线/采样器/uniform 建一次;输出纹理随尺寸重建。
pub struct GlassPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    /// 输出纹理及其尺寸。源纹理尺寸变了就重建。
    out: Option<(u32, u32, wgpu::Texture)>,
}

impl GlassPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(
            wgpu::ShaderModuleDescriptor {
                label: Some("glass-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("glass.wgsl").into(),
                ),
            },
        );

        let layout = device.create_bind_group_layout(
            &wgpu::BindGroupLayoutDescriptor {
                label: Some("glass-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type:
                                wgpu::TextureSampleType::Float {
                                    filterable: true,
                                },
                            view_dimension:
                                wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            },
        );

        let pipeline_layout = device.create_pipeline_layout(
            &wgpu::PipelineLayoutDescriptor {
                label: Some("glass-pl"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            },
        );

        let pipeline = device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("glass-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    // 与源纹理同格式:采样与写回都走非 sRGB 视图,原样进原样出,
                    // 玻璃之外的像素与 bevy 直出的那张一模一样(见 run 的注释)。
                    targets: &[Some(
                        wgpu::TextureFormat::Rgba8Unorm.into(),
                    )],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        let sampler = device.create_sampler(
            &wgpu::SamplerDescriptor {
                label: Some("glass-sampler"),
                // 边缘钳制:模糊核在贴边处会采到纹理外,钳制比环绕安全(不会把对侧像素卷进来)。
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            },
        );

        let uniform = device.create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("glass-uniform"),
                size: (UNIFORM_FLOATS * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );

        Self {
            pipeline,
            layout,
            sampler,
            uniform,
            out: None,
        }
    }

    /// 在 `src` 上跑一遍玻璃 pass,返回合成后的纹理。
    ///
    /// `rect` 为空(3D 页刚切进来还没量出尺寸)时直接返回 `src`,不白跑一趟。
    ///
    /// 色彩:源纹理是 `Rgba8Unorm`(带一个 sRGB 视图供 bevy 写入)。这里采样与写回都用
    /// **默认的非 sRGB 视图**,即原始字节进、原始字节出 —— 于是玻璃之外的像素与 bevy 直出的
    /// 那张逐字节相同,Slint 拿到手的观感不变。模糊因此发生在非线性空间里,严格说不符合物理,
    /// 但 CSS 的 backdrop-filter 也是这么干的,UI 模糊上看不出来。
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &wgpu::Texture,
        rect: GlassRect,
    ) -> wgpu::Texture {
        if rect.is_empty() {
            return src.clone();
        }
        let (w, h) = (src.width(), src.height());
        self.ensure_out(device, w, h);
        let out = &self.out.as_ref().expect("刚建过").2;

        queue.write_buffer(
            &self.uniform,
            0,
            &uniform_bytes([
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                w as f32,
                h as f32,
                rect.radius,
                BLUR_PX,
            ]),
        );

        // ponytail: 每帧重建 view + bind group。源纹理的身份会随 resize 变,与其跟踪它
        // 什么时候换了(错一次就是采样到已释放的纹理),不如每帧几微秒重建掉。真成为热点
        // 再按纹理 id 缓存。
        let src_view = src.create_view(
            &wgpu::TextureViewDescriptor::default(),
        );
        let out_view = out.create_view(
            &wgpu::TextureViewDescriptor::default(),
        );
        let bind = device.create_bind_group(
            &wgpu::BindGroupDescriptor {
                label: Some("glass-bind"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &src_view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            &self.sampler,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self
                            .uniform
                            .as_entire_binding(),
                    },
                ],
            },
        );

        let mut encoder = device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("glass-encoder"),
            },
        );
        {
            let mut pass = encoder.begin_render_pass(
                &wgpu::RenderPassDescriptor {
                    label: Some("glass-pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: &out_view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                // 整屏都会被全屏三角形覆盖,不必清屏。
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        },
                    )],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);

        out.clone()
    }

    /// 按需重建输出纹理。格式与 view_formats 必须与 bevy 的离屏目标一致,
    /// 否则 Slint 的 `Image::try_from` 会拒绝导入。
    fn ensure_out(
        &mut self,
        device: &wgpu::Device,
        w: u32,
        h: u32,
    ) {
        if matches!(self.out, Some((ow, oh, _)) if (ow, oh) == (w, h))
        {
            return;
        }
        let tex = device.create_texture(
            &wgpu::TextureDescriptor {
                label: Some("glass-out"),
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
                view_formats: &[
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                ],
            },
        );
        self.out = Some((w, h, tex));
    }
}
