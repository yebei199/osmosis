//! 播放页反馈 warp 视觉的**独立 wgpu pass**(不经 bevy)。
//!
//! 与 [`crate::NavGlassPass`] 同一个骨架:共享 device 上自起全屏片元 pass,
//! 画进离屏纹理导入 Slint。区别在两处:这里是**两张**目标纹理 ping-pong ——
//! 反馈机制要求每帧采样上一帧;以及多一张 512×2 的音频纹理,每帧由调用方
//! 送来 `audio::spectrum` 产的频谱与波形字节(Shadertoy 布局,shader 侧的
//! 采样代码可与 Shadertoy 素材互通)。
//!
//! 省电门在 ui 侧(展开 ∧ 播放 ∧ 可见):门关着没人调 [`WarpPass::render_frame`],
//! Slint 复用上一帧纹理,GPU 归零。shader 见 `warp.wgsl`。

use slint::wgpu_29::wgpu;

/// warp 目标纹理的边长,物理像素。
///
/// 它曾经按视口尺寸铺满整个播放页当背景;现在收进播放键那颗圆里(见
/// `crates/ui/slint/app.slint` 的 `ControlCluster`),只要够那颗按钮清楚就行。
/// 192 在 2 倍缩放下也还有富余,而全屏时它是这个面积的一百多倍 —— 反馈 pass
/// 每帧都要采上一帧,面积就是它的全部开销。
pub const WARP_SIDE: u32 = 192;

/// 音频纹理宽(点数),与 `audio::spectrum::BINS` 手工对齐 —— 两个 crate 互不依赖,
/// 由 apps 在 seam 处传字节,长度错了 [`WarpPass::render_frame`] 直接跳过上传。
pub const AUDIO_BINS: u32 = 512;

/// 音频载荷字节数:频谱行 + 波形行。
pub const AUDIO_BYTES: usize = (AUDIO_BINS * 2) as usize;

/// uniform 缓冲字节数:vec2 尺寸 + time + bass,恰 16 字节。
const UBO_BYTES: u64 = 16;

/// 反馈衰减之外,低频包络取频谱行开头这么多个 bin 的均值 —— 鼓点集中在最低那一撮。
const BASS_BINS: usize = 12;

/// 一对 ping-pong 目标:纹理、导入 Slint 的图、以及各自的 bind group
/// (第 i 张作为目标时,采样的是第 1-i 张)。
struct Targets {
    textures: [wgpu::Texture; 2],
    images: [slint::Image; 2],
    bind_groups: [wgpu::BindGroup; 2],
    size: (u32, u32),
}

/// 播放页 warp 的 wgpu pass:持有管线、音频纹理与随尺寸重建的 ping-pong 目标。
/// 每帧渲染后返回当前目标包装成的 `slint::Image`,两张图各自只导入一次。
pub struct WarpPass {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    ubo: wgpu::Buffer,
    audio_tex: wgpu::Texture,
    sampler: wgpu::Sampler,
    targets: Option<Targets>,
    /// 这一帧要画进哪张目标(0/1),每帧翻转。
    cur: usize,
}

mod frame;
mod target;

impl WarpPass {
    /// 在共享 device 上建好管线、采样器与音频纹理。device/queue 由 `Scene` 暴露。
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let shader = device.create_shader_module(
            wgpu::ShaderModuleDescriptor {
                label: Some("warp-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("warp.wgsl").into(),
                ),
            },
        );

        let texture_entry =
            |binding| wgpu::BindGroupLayoutEntry {
                binding,
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
            };
        let sampler_entry =
            |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(
                    wgpu::SamplerBindingType::Filtering,
                ),
                count: None,
            };
        let bind_layout = device.create_bind_group_layout(
            &wgpu::BindGroupLayoutDescriptor {
                label: Some("warp-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility:
                            wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    texture_entry(1),
                    sampler_entry(2),
                    texture_entry(3),
                    sampler_entry(4),
                ],
            },
        );

        let ubo =
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("warp-ubo"),
                size: UBO_BYTES,
                usage: wgpu::BufferUsages::UNIFORM
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

        // 音频纹理:512×2 单通道。每帧 write_texture 整张重写,1KB 的量级。
        let audio_tex = device.create_texture(
            &wgpu::TextureDescriptor {
                label: Some("warp-audio"),
                size: wgpu::Extent3d {
                    width: AUDIO_BINS,
                    height: 2,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
        );

        // 线性过滤 + 边缘钳制:反馈采样越界(旋转后的角落)取边缘色,不回卷。
        let sampler = device.create_sampler(
            &wgpu::SamplerDescriptor {
                label: Some("warp-sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            },
        );

        let pipeline_layout = device
            .create_pipeline_layout(
                &wgpu::PipelineLayoutDescriptor {
                    label: Some("warp-pl"),
                    // 此 wgpu-29 的字段是 &[Option<&_>](稀疏 set 支持)。
                    bind_group_layouts: &[Some(
                        &bind_layout,
                    )],
                    immediate_size: 0,
                },
            );

        let pipeline = device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("warp-pipeline"),
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
                        // 与其余离屏目标同格式,满足 Slint 导入要求。
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
                // 不用 multiview(此 wgpu-29 的字段名是 multiview_mask)。
                multiview_mask: None,
                cache: None,
            },
        );

        Self {
            device,
            queue,
            pipeline,
            bind_layout,
            ubo,
            audio_tex,
            sampler,
            targets: None,
            cur: 0,
        }
    }
}
