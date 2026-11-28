use super::Renderer;
use crate::terminal_pane::PaneId;

impl Renderer {
    /// ブラウザサムネイルの RGBA 画像を GPU テクスチャへアップロードする
    pub fn update_browser_snapshot(
        &mut self,
        pane_id: PaneId,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        if width == 0 || height == 0 || rgba.len() < (width as usize * height as usize * 4) {
            return;
        }
        let write_pixels = |texture: &wgpu::Texture| {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        };

        // 同じ解像度の既存テクスチャがあれば再利用し、不要な GPU テクスチャ再確保を避ける
        if let Some((texture, _, cur_w, cur_h)) = self.browser_textures.get(&pane_id)
            && *cur_w == width
            && *cur_h == height
        {
            write_pixels(texture);
            return;
        }

        let tex_format = if self.is_srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("browser_thumb"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: tex_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_pixels(&texture);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("browser_thumb_bg"),
            layout: &self.image_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.image_sampler),
                },
            ],
        });
        self.browser_textures
            .insert(pane_id, (texture, bind_group, width, height));
    }

    pub fn retain_browser_textures(&mut self, live_ids: &[PaneId]) {
        self.browser_textures.retain(|id, _| live_ids.contains(id));
    }
}
