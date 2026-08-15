//! warp 的每一帧:推 uniform、跑管线、把结果交出去。

use super::*;

impl WarpPass {
    /// 渲染一帧:上传音频字节 → 采样上一张目标画进当前张 → 返回当前张的图。
    ///
    /// `time` 是播放页时钟(秒),门关着时调用方不走这里,时钟随之冻结;
    /// `audio` 是 `spectrum::VizFrame` 拼出的 [`AUDIO_BYTES`] 字节
    /// (频谱行在前、波形行在后),长度不符则本帧沿用上一次的音频纹理。
    pub fn render_frame(
        &mut self,
        time: f32,
        audio: &[u8],
        w: u32,
        h: u32,
    ) -> slint::Image {
        let (w, h) = (w.max(1), h.max(1));
        self.ensure_targets(w, h);

        if audio.len() == AUDIO_BYTES {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.audio_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                audio,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(AUDIO_BINS),
                    rows_per_image: Some(2),
                },
                wgpu::Extent3d {
                    width: AUDIO_BINS,
                    height: 2,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            log::warn!(
                "warp: 音频载荷 {} 字节,期望 {AUDIO_BYTES},本帧跳过上传",
                audio.len()
            );
        }

        // 低频包络:频谱行开头几个 bin 的均值,驱动 shader 里的缩放与旋转脉动。
        let bass = audio
            .get(..BASS_BINS)
            .map(|head| {
                head.iter()
                    .map(|v| f32::from(*v))
                    .sum::<f32>()
                    / (BASS_BINS as f32 * 255.0)
            })
            .unwrap_or(0.0);

        let vals: [f32; 4] =
            [w as f32, h as f32, time, bass];
        let mut bytes =
            Vec::with_capacity(UBO_BYTES as usize);
        for v in vals {
            bytes.extend_from_slice(&v.to_ne_bytes());
        }
        self.queue.write_buffer(&self.ubo, 0, &bytes);

        let targets = self
            .targets
            .as_ref()
            .expect("ensure_targets 刚建过");
        let view = targets.textures[self.cur].create_view(
            &wgpu::TextureViewDescriptor::default(),
        );
        let mut enc = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("warp-encoder"),
            },
        );
        {
            let mut rp = enc.begin_render_pass(
                &wgpu::RenderPassDescriptor {
                    label: Some("warp-pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                // 片元覆盖每个像素,load 值无所谓,清透明最便宜。
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
                &targets.bind_groups[self.cur],
                &[],
            );
            rp.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);

        let image = targets.images[self.cur].clone();
        self.cur ^= 1;
        image
    }
}
