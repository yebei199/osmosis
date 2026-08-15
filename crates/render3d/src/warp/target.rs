//! 离屏目标的按需重建:尺寸变了才重开纹理。

use super::*;

impl WarpPass {
    /// 尺寸变了(首帧/窗口缩放)就重建 ping-pong 目标对与两个 bind group,
    /// 并把两张纹理各导入 Slint 一次;否则原样复用。
    pub(super) fn ensure_targets(
        &mut self,
        w: u32,
        h: u32,
    ) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.size == (w, h))
        {
            return;
        }
        let make_texture = || {
            self.device.create_texture(
                &wgpu::TextureDescriptor {
                    label: Some("warp-target"),
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
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
            )
        };
        let textures = [make_texture(), make_texture()];
        let images = [0, 1].map(|i| {
            slint::Image::try_from(textures[i].clone())
                .inspect_err(|e| {
                    log::error!(
                        "warp 纹理导入 Slint 失败: {e:?}"
                    );
                })
                .unwrap_or_default()
        });
        let audio_view = self.audio_tex.create_view(
            &wgpu::TextureViewDescriptor::default(),
        );
        // bind_groups[i]:画第 i 张时采样第 1-i 张(反馈的上一帧)。
        let bind_groups = [0usize, 1].map(|i| {
            let prev_view = textures[1 - i].create_view(
                &wgpu::TextureViewDescriptor::default(),
            );
            self.device.create_bind_group(
                &wgpu::BindGroupDescriptor {
                    label: Some("warp-bg"),
                    layout: &self.bind_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self
                                .ubo
                                .as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource:
                                wgpu::BindingResource::TextureView(
                                    &prev_view,
                                ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource:
                                wgpu::BindingResource::Sampler(
                                    &self.sampler,
                                ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource:
                                wgpu::BindingResource::TextureView(
                                    &audio_view,
                                ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource:
                                wgpu::BindingResource::Sampler(
                                    &self.sampler,
                                ),
                        },
                    ],
                },
            )
        });
        self.targets = Some(Targets {
            textures,
            images,
            bind_groups,
            size: (w, h),
        });
        self.cur = 0;
    }
}
