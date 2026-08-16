//! 光带按钮的 wgpu pass(docs/design/handoff-shaders.md §9/§10)。
//!
//! 与 [`NavGlassPass`](crate::NavGlassPass) 同构:共享 device 上的独立
//! pass,渲进离屏纹理,包装成 `slint::Image` 由 Slint 合成。五个变体共用
//! `aurorabtn.wgsl` 一个管线;**合批**体现在一个 pipeline、一个 uniform
//! 缓冲(动态偏移)、一次 submit —— 参考实现里"一颗按钮一个 context"的
//! 结构禁止照抄。每颗按钮一张自己的纹理:尺寸互不相同,拼图集省不了
//! 什么,反而让 Slint 侧多一套裁剪坐标。
//!
//! 省电门在 ui 侧(`ui::aurora_btn`):hover 动画收敛后根本不会调到这里。

use slint::wgpu_29::wgpu;

/// 一颗按钮这一帧的控制量,**物理像素**。POD,由 ui 侧组装
/// (`ui::AuroraBtnSlotControls`),apps/* 在 seam 处平凡拷来。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuroraBtnSlot {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    /// 每颗不同的相位种子与流速 —— 合批下唯一的 per-instance 个性。
    pub seed: f32,
    pub speed: f32,
    /// 静息 0.12 → 悬停 1.0,由 ui 侧收敛。
    pub amp: f32,
    /// 0 = 全光谱,1 = 绿色板。只影响 ribbon 的色相。
    pub mode: f32,
    /// ribbon 的光带条数 1..4。
    pub bands: f32,
    /// 0 ribbon 1 nebula 2 fluid 3 glass 4 progress 5 prism。
    pub variant: f32,
    pub progress: f32,
    /// 棱柱转到哪儿了,单位是「面」。只有 prism 变体读它。
    pub flip: f32,
    /// 按钮内归一化指针位置。
    pub pointer: (f32, f32),
    /// 四色板:底 / 主 / 次 / 高光。
    pub colors: [[f32; 3]; 4],
}

/// 一帧全部按钮的控制量。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AuroraBtnParams {
    /// 按钮时钟,秒。ui 侧只在动画进行中推进 —— 冻结即静止画面。
    pub time: f32,
    pub slots: Vec<AuroraBtnSlot>,
}

/// WGSL Params 实占 128 字节;动态偏移要按设备的 uniform 对齐(常见 256)取整。
const SLOT_BYTES: u64 = 128;
const SLOT_STRIDE: u64 = 256;
/// 界面上同时在动的极光按钮不会多于这个数;超出的这一帧不画,下一帧轮上。
const MAX_SLOTS: usize = 8;

pub struct AuroraBtnPass {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    ubo: wgpu::Buffer,
    /// 每颗按钮一张 (纹理, 宽, 高) 与其 Slint 导入,尺寸稳定时只导一次。
    targets: Vec<Option<(wgpu::Texture, u32, u32)>>,
    images: Vec<Option<slint::Image>>,
}

impl AuroraBtnPass {
    /// 在共享 device 上建好管线与 uniform 缓冲。device/queue 由 `Scene` 暴露。
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let shader = device.create_shader_module(
            wgpu::ShaderModuleDescriptor {
                label: Some("aurorabtn-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("aurorabtn.wgsl").into(),
                ),
            },
        );

        let bind_layout = device.create_bind_group_layout(
            &wgpu::BindGroupLayoutDescriptor {
                label: Some("aurorabtn-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size:
                            wgpu::BufferSize::new(SLOT_BYTES),
                    },
                    count: None,
                }],
            },
        );

        let ubo =
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aurorabtn-ubo"),
                size: SLOT_STRIDE * MAX_SLOTS as u64,
                usage: wgpu::BufferUsages::UNIFORM
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

        let bind_group = device.create_bind_group(
            &wgpu::BindGroupDescriptor {
                label: Some("aurorabtn-bg"),
                layout: &bind_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(
                        wgpu::BufferBinding {
                            buffer: &ubo,
                            offset: 0,
                            size: wgpu::BufferSize::new(
                                SLOT_BYTES,
                            ),
                        },
                    ),
                }],
            },
        );

        let pipeline_layout = device
            .create_pipeline_layout(
                &wgpu::PipelineLayoutDescriptor {
                    label: Some("aurorabtn-pl"),
                    bind_group_layouts: &[Some(
                        &bind_layout,
                    )],
                    immediate_size: 0,
                },
            );

        let pipeline = device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("aurorabtn-pipeline"),
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
            targets: Vec::new(),
            images: Vec::new(),
        }
    }

    /// 渲一帧全部按钮,返回与 `slots` 一一对应的 `slint::Image`
    /// (0 尺寸槽给空图)。一个 encoder、一次 submit;每颗自己的小纹理。
    pub fn render_frame(
        &mut self,
        p: &AuroraBtnParams,
    ) -> Vec<slint::Image> {
        let n = p.slots.len().min(MAX_SLOTS);
        self.targets.resize_with(n, || None);
        self.images.resize_with(n, || None);

        // 先写全部 uniform,再录制 —— write_buffer 在 submit 前生效即可,
        // 槽间偏移互不相交。
        for (i, slot) in p.slots.iter().take(n).enumerate()
        {
            let c = &slot.colors;
            #[rustfmt::skip]
            let vals: [f32; 32] = [
                0.0, 0.0,                    // origin(独立纹理恒为 0)
                slot.w, slot.h,              // res
                p.time, slot.seed, slot.speed, slot.amp,
                slot.mode, slot.radius, slot.bands, slot.variant,
                slot.progress, slot.flip,    // progress, flip
                slot.pointer.0, slot.pointer.1,
                c[0][0], c[0][1], c[0][2], 0.0,
                c[1][0], c[1][1], c[1][2], 0.0,
                c[2][0], c[2][1], c[2][2], 0.0,
                c[3][0], c[3][1], c[3][2], 0.0,
            ];
            let mut bytes =
                Vec::with_capacity(SLOT_BYTES as usize);
            for v in vals {
                bytes.extend_from_slice(&v.to_ne_bytes());
            }
            self.queue.write_buffer(
                &self.ubo,
                SLOT_STRIDE * i as u64,
                &bytes,
            );
        }

        let mut enc = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("aurorabtn-encoder"),
            },
        );

        for (i, slot) in p.slots.iter().take(n).enumerate()
        {
            let w = slot.w as u32;
            let h = slot.h as u32;
            if w == 0 || h == 0 {
                self.images[i] = None;
                continue;
            }

            let need_new =
                self.targets[i].as_ref().is_none_or(
                    |(_, tw, th)| *tw != w || *th != h,
                );
            if need_new {
                let tex = self.device.create_texture(
                    &wgpu::TextureDescriptor {
                        label: Some("aurorabtn-target"),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension:
                            wgpu::TextureDimension::D2,
                        format:
                            wgpu::TextureFormat::Rgba8Unorm,
                        usage:
                            wgpu::TextureUsages::RENDER_ATTACHMENT
                                | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                );
                match slint::Image::try_from(tex.clone()) {
                    Ok(img) => self.images[i] = Some(img),
                    Err(e) => log::error!(
                        "aurorabtn 纹理导入 Slint 失败: {e:?}"
                    ),
                }
                self.targets[i] = Some((tex, w, h));
            }

            let Some((tex, _, _)) =
                self.targets[i].as_ref()
            else {
                continue;
            };
            let view = tex.create_view(
                &wgpu::TextureViewDescriptor::default(),
            );
            let mut rp = enc.begin_render_pass(
                &wgpu::RenderPassDescriptor {
                    label: Some("aurorabtn-pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
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
                    multiview_mask: None,
                },
            );
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(
                0,
                &self.bind_group,
                &[(SLOT_STRIDE * i as u64) as u32],
            );
            rp.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);

        (0..p.slots.len())
            .map(|i| {
                self.images
                    .get(i)
                    .and_then(Clone::clone)
                    .unwrap_or_default()
            })
            .collect()
    }
}
