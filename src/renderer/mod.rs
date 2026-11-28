mod footer;
mod help_modal;
mod main_pane;
mod primitives;
mod search_bar;
mod sidebar;
mod status_bar;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use glyphon::{
    Buffer, Cache, FontSystem, Metrics, PrepareError, Resolution, SwashCache, TextArea, TextAtlas,
    TextRenderer, Viewport, Wrap,
};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::layout::{AppLayout, Rect};
use crate::slot::{NUM_SLOTS, Workspace};
use crate::terminal_pane::{MainScreenSnapshot, PaneId};

use primitives::{
    IMAGE_SHADER, ImageInstance, RECT_SHADER, RectInstance, ScreenUniforms, measure_mono_cell,
};

#[derive(Clone, PartialEq, Eq, Default)]
struct CardHeaderCacheKey {
    left_str: String,
    right_str: String,
    is_active: bool,
    is_outputting: bool,
    is_waiting: bool,
    is_dormant: bool,
    is_collapsed: bool,
    is_playing_audio: bool,
    width_px: u32,
}

struct CachedMainSpan {
    start_col: u16,
    is_wide: bool,
    buffer: Buffer,
}

struct CachedPaneVisual {
    generation: u64,
    width_px: u32,
    snapshot: MainScreenSnapshot,
    /// 行単位のテキストキャッシュ（dirty な行のみ再シェイプ）
    row_spans: Vec<Vec<CachedMainSpan>>,
}

/// マウス選択範囲
#[derive(Clone, Copy, Debug)]
pub struct SelectionOverlay {
    pub pane_id: PaneId,
    pub start: (u16, u16),
    pub end: (u16, u16),
}

/// ホバー中 URL の下線ハイライト範囲
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UrlHoverOverlay {
    pub pane_id: PaneId,
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

/// ステータスバー横長タイマーの表示状態
#[derive(Clone, Debug, Default)]
pub struct TimerOverlay {
    /// 残りまたは設定中の割合（0.0 = 0分, 1.0 = 60分）
    pub fraction: f32,
    /// タイマー設定時間に対する残り割合（1.0 = 開始直後, 0.0 = 終了。砂時計用）
    pub progress: f32,
    /// 表示する分数ラベル（例: "25m"、停止中は空）
    pub label: String,
    pub is_dragging: bool,
    pub is_running: bool,
    pub is_finished: bool,
    /// 砂粒落下アニメーション用フレームカウンタ
    pub anim_tick: u32,
}

/// ペイン内検索のビューポート内ハイライト一覧
#[derive(Clone, Debug)]
pub struct SearchHighlightOverlay {
    pub pane_id: PaneId,
    /// `(vp_row, start_col, end_col, is_current)`
    pub matches: Vec<(u16, u16, u16, bool)>,
}

/// ペイン内検索バーの表示状態
#[derive(Clone, Debug)]
pub struct SearchBarOverlay<'a> {
    pub pane_id: PaneId,
    pub is_browser: bool,
    pub query: &'a str,
    pub select_all: bool,
    pub count_label: String,
}

/// ステータスバーのメモリ使用量バー表示状態
/// 80% ライン = 512 MiB（「これだけあれば足りる」目安）、100% フルスケール = 640 MiB
#[derive(Clone, Debug, Default)]
pub struct MemoryOverlay {
    /// フルスケール（640 MiB）に対する使用率（0.0..=1.0）
    pub fraction: f32,
    /// 80% 警告ライン（512 MiB）を超過しているか
    pub is_over_budget: bool,
    /// バー中央に表示する現在値ラベル（例: "185M", "1.2G"）
    pub label: String,
}

impl MemoryOverlay {
    /// ターミナル＋デーモン合計の「これだけあれば足りる」基準ライン（512 MiB = バーの 80%）
    pub const BUDGET_BYTES: u64 = 512 * 1024 * 1024;
    /// バーの 100% フルスケール（640 MiB = 512 MiB / 0.8）
    pub const FULL_SCALE_BYTES: u64 = 640 * 1024 * 1024;

    pub fn from_bytes(bytes: u64) -> Self {
        if bytes == 0 {
            return Self::default();
        }
        let fraction = (bytes as f64 / Self::FULL_SCALE_BYTES as f64).clamp(0.0, 1.0) as f32;
        Self {
            fraction,
            is_over_budget: bytes > Self::BUDGET_BYTES,
            label: crate::macos::format_memory_bytes(bytes),
        }
    }
}

/// 描画時のオーバーレイ状態（IME・D&D・テキスト選択・URLホバー・URLバー編集・検索・システム状態・ヘルプ）
pub struct RenderOverlays<'a> {
    pub ime_preedit: &'a str,
    pub drag_source_slot: Option<usize>,
    pub drag_hover_slot: Option<usize>,
    pub selection: Option<SelectionOverlay>,
    pub hovered_url: Option<UrlHoverOverlay>,
    pub url_bar_editing: Option<(&'a str, bool)>,
    pub search_highlights: Option<SearchHighlightOverlay>,
    pub search_bar: Option<SearchBarOverlay<'a>>,
    pub battery: Option<crate::macos::BatteryStatus>,
    pub wifi: Option<crate::macos::WifiStatus>,
    pub show_help: bool,
    pub daemon_version_mismatch: bool,
    pub daemon_upgrade_confirm: bool,
    pub version_copied: bool,
    pub timer: TimerOverlay,
    pub memory: &'a MemoryOverlay,
}

/// 1フレーム描画の戻り値
pub struct RenderOutcome {
    pub cursor_area: Option<(f32, f32, f32, f32)>,
    pub needs_redraw: bool,
}

type FooterCacheKey = (Vec<(String, bool)>, u32, u32);
type SearchCacheKey = (String, String, bool, u32, u32);

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    is_srgb: bool,

    rect_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,

    image_pipeline: wgpu::RenderPipeline,
    image_bind_group_layout: wgpu::BindGroupLayout,
    image_sampler: wgpu::Sampler,
    image_instance_buffer: wgpu::Buffer,
    image_instance_capacity: usize,
    browser_textures: HashMap<PaneId, (wgpu::Texture, wgpu::BindGroup, u32, u32)>,

    font_system: FontSystem,
    swash_cache: SwashCache,
    _glyphon_cache: Cache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,

    pub font_size_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,

    // ペインごとの行単位メイン画面テキストキャッシュ
    pane_cache: HashMap<PaneId, CachedPaneVisual>,

    // IME 変換中テキスト用バッファ（セル位置揃え）
    ime_preedit_spans: Vec<(u16, bool, Buffer)>,
    cached_ime_preedit: String,

    // サイドバーカードヘッダキャッシュ
    card_left_buffers: Vec<Buffer>,
    card_right_buffers: Vec<Buffer>,
    card_header_keys: Vec<CardHeaderCacheKey>,

    // 列折りたたみトグルボタン用テキストバッファ
    col_toggle_buffers: [Buffer; 2],
    col_toggle_labels: [String; 2],

    // メインパネル上部ステータスバー用バッファ
    main_status_buffer: Buffer,
    main_status_cache_key: (String, u32),
    url_bar_buffer: Buffer,
    url_bar_cache_key: (String, bool, u32),
    clock_buffer: Buffer,
    clock_text: String,
    version_buffer: Buffer,
    version_cache_key: u32,
    timer_buffer: Buffer,
    timer_cache_key: String,
    mem_buffer: Buffer,
    mem_cache_key: (String, bool, u32),
    status_btn_buffers: [Buffer; 8],
    status_btn_labels: [String; 8],

    // メインパネル下端フッター（ペイン別最終更新時刻）用バッファ
    footer_buffers: Vec<Buffer>,
    footer_cache_keys: Vec<FooterCacheKey>,

    // ペイン内検索バー用バッファ
    search_query_buffer: Buffer,
    search_count_buffer: Buffer,
    search_cache_key: SearchCacheKey,

    // ショートカットヘルプダイアログ用バッファ
    help_header_buffer: Buffer,
    help_left_buffer: Buffer,
    help_right_buffer: Buffer,
    help_cache_key: (u32, u32),
}

impl Renderer {
    pub fn new(window: Arc<Window>, scale_factor: f32) -> Result<Self, Box<dyn std::error::Error>> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window)?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let is_srgb = format.is_srgb();

        let mut config = surface
            .get_default_config(&adapter, width, height)
            .ok_or("surface not supported by adapter")?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect_shader"),
            source: wgpu::ShaderSource::Wgsl(RECT_SHADER.into()),
        });

        let uniforms = ScreenUniforms {
            screen_size: [width as f32, height as f32],
            _pad: [0.0, 0.0],
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("screen_uniforms"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rect_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rect_bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rect_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RectInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_capacity = 4096;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect_instances"),
            size: (instance_capacity * std::mem::size_of::<RectInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image_shader"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
        });
        let image_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("image_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("image_pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout), Some(&image_bind_group_layout)],
                immediate_size: 0,
            });
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image_pipeline"),
            layout: Some(&image_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ImageInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let image_instance_capacity = 32;
        let image_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image_instances"),
            size: (image_instance_capacity * std::mem::size_of::<ImageInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 日本語漢字が中国語簡体字フォント（PingFang SC 等）へフォールバックしないよう ja-JP ロケールを明示
        let mut font_db = glyphon::fontdb::Database::new();
        font_db.load_system_fonts();
        let mut font_system = FontSystem::new_with_locale_and_db("ja-JP".to_string(), font_db);
        font_system.db_mut().set_monospace_family("Menlo");
        let swash_cache = SwashCache::new();
        let glyphon_cache = Cache::new(&device);
        let mut viewport = Viewport::new(&device, &glyphon_cache);
        viewport.update(&queue, Resolution { width, height });
        let mut atlas = TextAtlas::new(&device, &queue, &glyphon_cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        let font_size_px = (13.5 * scale_factor).round().max(10.0);
        let (cell_w, cell_h) = measure_mono_cell(&mut font_system, font_size_px);

        let card_font_size = (11.0 * scale_factor).round().max(9.0);
        let card_metrics = Metrics::new(card_font_size, (card_font_size * 1.35).ceil());
        let mut card_left_buffers = Vec::with_capacity(NUM_SLOTS);
        let mut card_right_buffers = Vec::with_capacity(NUM_SLOTS);
        for _ in 0..NUM_SLOTS {
            let mut lb = Buffer::new(&mut font_system, card_metrics);
            lb.set_wrap(Wrap::None);
            card_left_buffers.push(lb);

            let mut rb = Buffer::new(&mut font_system, card_metrics);
            rb.set_wrap(Wrap::None);
            card_right_buffers.push(rb);
        }

        let mut tb0 = Buffer::new(&mut font_system, card_metrics);
        tb0.set_wrap(Wrap::None);
        let mut tb1 = Buffer::new(&mut font_system, card_metrics);
        tb1.set_wrap(Wrap::None);

        let mut main_status_buffer = Buffer::new(&mut font_system, card_metrics);
        main_status_buffer.set_wrap(Wrap::None);
        let mut url_bar_buffer = Buffer::new(&mut font_system, card_metrics);
        url_bar_buffer.set_wrap(Wrap::None);
        let mut clock_buffer = Buffer::new(&mut font_system, card_metrics);
        clock_buffer.set_wrap(Wrap::None);
        let mut version_buffer = Buffer::new(&mut font_system, card_metrics);
        version_buffer.set_wrap(Wrap::None);
        let mut timer_buffer = Buffer::new(&mut font_system, card_metrics);
        timer_buffer.set_wrap(Wrap::None);
        let mut mem_buffer = Buffer::new(&mut font_system, card_metrics);
        mem_buffer.set_wrap(Wrap::None);
        let status_btn_buffers: [Buffer; 8] = std::array::from_fn(|_| {
            let mut b = Buffer::new(&mut font_system, card_metrics);
            b.set_wrap(Wrap::None);
            b
        });
        let help_header_buffer = Buffer::new(&mut font_system, card_metrics);
        let help_left_buffer = Buffer::new(&mut font_system, card_metrics);
        let help_right_buffer = Buffer::new(&mut font_system, card_metrics);
        let mut search_query_buffer = Buffer::new(&mut font_system, card_metrics);
        search_query_buffer.set_wrap(Wrap::None);
        let mut search_count_buffer = Buffer::new(&mut font_system, card_metrics);
        search_count_buffer.set_wrap(Wrap::None);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            is_srgb,
            rect_pipeline,
            uniform_buffer,
            uniform_bind_group,
            instance_buffer,
            instance_capacity,
            image_pipeline,
            image_bind_group_layout,
            image_sampler,
            image_instance_buffer,
            image_instance_capacity,
            browser_textures: HashMap::new(),
            font_system,
            swash_cache,
            _glyphon_cache: glyphon_cache,
            viewport,
            atlas,
            text_renderer,
            font_size_px,
            cell_w,
            cell_h,
            pane_cache: HashMap::new(),
            ime_preedit_spans: Vec::new(),
            cached_ime_preedit: String::new(),
            card_left_buffers,
            card_right_buffers,
            card_header_keys: vec![CardHeaderCacheKey::default(); NUM_SLOTS],
            col_toggle_buffers: [tb0, tb1],
            col_toggle_labels: [String::new(), String::new()],
            main_status_buffer,
            main_status_cache_key: (String::new(), 0),
            url_bar_buffer,
            url_bar_cache_key: (String::new(), false, 0),
            clock_buffer,
            clock_text: String::new(),
            version_buffer,
            version_cache_key: 0,
            timer_buffer,
            timer_cache_key: String::new(),
            mem_buffer,
            mem_cache_key: (String::new(), false, 0),
            status_btn_buffers,
            status_btn_labels: std::array::from_fn(|_| String::new()),
            footer_buffers: Vec::new(),
            footer_cache_keys: Vec::new(),
            search_query_buffer,
            search_count_buffer,
            search_cache_key: (String::new(), String::new(), true, 0, 0),
            help_header_buffer,
            help_left_buffer,
            help_right_buffer,
            help_cache_key: (0, 0),
        })
    }

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

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) {
        let width = width.max(1);
        let height = height.max(1);
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);

        let uniforms = ScreenUniforms {
            screen_size: [width as f32, height as f32],
            _pad: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        self.viewport
            .update(&self.queue, Resolution { width, height });

        let new_font_size = (13.5 * scale_factor).round().max(10.0);
        if (new_font_size - self.font_size_px).abs() > 0.1 {
            self.font_size_px = new_font_size;
            let (cw, ch) = measure_mono_cell(&mut self.font_system, self.font_size_px);
            self.cell_w = cw;
            self.cell_h = ch;
            self.pane_cache.clear();
            self.cached_ime_preedit.clear();
        }
        for key in &mut self.card_header_keys {
            key.width_px = 0;
        }
        // スケール変更時もトグルボタンとステータスバーのフォントメトリクスを再生成させる
        self.col_toggle_labels = [String::new(), String::new()];
        self.main_status_cache_key = (String::new(), 0);
        self.url_bar_cache_key = (String::new(), false, 0);
        self.clock_text.clear();
        self.mem_cache_key.2 = 0;
        self.status_btn_labels = std::array::from_fn(|_| String::new());
        for key in &mut self.footer_cache_keys {
            key.2 = 0;
        }
        self.search_cache_key.4 = 0;
        self.help_cache_key = (0, 0);
    }

    /// 描画を実行し、フォーカス中ペインのカーソル物理矩形と再描画要求を返す
    pub fn render(
        &mut self,
        workspace: &mut Workspace,
        layout: &AppLayout,
        scale_factor: f32,
        pane_gap: f32,
        overlays: &RenderOverlays<'_>,
    ) -> RenderOutcome {
        let now = Instant::now();
        let active_idx = workspace.active_slot;
        let focused_pane_id = workspace.slots[active_idx].active_focused_pane();

        // 1. アクティブスロットの全ペインを行単位 dirty 追跡で差分更新
        let active_pane_rects =
            workspace.slots[active_idx].compute_pane_rects(layout.main_inner_rect, pane_gap);
        self.update_active_pane_cache(workspace, active_idx, &active_pane_rects);

        // 2. 展開されている列の非アクティブスロットのみ縮小プレビューを最新化
        for (i, slot) in workspace.slots.iter_mut().enumerate() {
            if i != active_idx
                && !layout.sidebar_state.is_col_collapsed(i / 5)
                && let Some(root) = slot.root.as_mut()
            {
                root.for_each_terminal_mut(&mut |p| p.ensure_minimap_updated());
            }
        }

        let mut rects: Vec<RectInstance> = Vec::with_capacity(2048);
        let mut image_draws: Vec<(PaneId, ImageInstance)> = Vec::new();

        // サイドバー背景・境界線
        self.push_rect(&mut rects, layout.sidebar_rect, [0.07, 0.08, 0.10, 1.0]);
        self.push_rect(
            &mut rects,
            Rect::new(
                layout.sidebar_rect.x,
                0.0,
                1.0 * scale_factor,
                layout.window_h,
            ),
            [0.20, 0.23, 0.29, 1.0],
        );

        // メイン領域背景＋上部ステータスバー
        self.push_rect(&mut rects, layout.main_rect, [0.08, 0.09, 0.12, 1.0]);
        self.push_rect(&mut rects, layout.main_status_rect, [0.10, 0.12, 0.16, 1.0]);
        self.push_rect(
            &mut rects,
            Rect::new(
                layout.main_status_rect.x,
                layout.main_status_rect.bottom() - 1.0 * scale_factor,
                layout.main_status_rect.w,
                1.0 * scale_factor,
            ),
            [0.20, 0.23, 0.29, 1.0],
        );

        let (sb_ctrl, is_browser_bar) = self.build_status_bar(
            workspace,
            active_idx,
            layout,
            scale_factor,
            overlays,
            &mut rects,
        );
        let main_res = self.build_main_panes(
            &active_pane_rects,
            focused_pane_id,
            scale_factor,
            overlays,
            &mut rects,
        );
        let footer_columns = self.build_footer(
            &workspace.slots[active_idx],
            &active_pane_rects,
            layout,
            scale_factor,
            pane_gap,
            &mut rects,
        );
        let search_bar_layout = self.build_search_bar(
            &active_pane_rects,
            layout,
            scale_factor,
            overlays,
            &mut rects,
        );
        let cursor_area = if let Some(ref sb) = search_bar_layout {
            Some((
                sb.query_rect.x,
                sb.bar_rect.y,
                sb.query_rect.w,
                sb.bar_rect.h,
            ))
        } else {
            main_res.cursor_area
        };
        let time_w = self.build_sidebar(
            workspace,
            active_idx,
            layout,
            scale_factor,
            now,
            overlays,
            &mut rects,
            &mut image_draws,
        );
        let help_modal_rect = self.build_help_modal(scale_factor, overlays, &mut rects);

        // インスタンスバッファへアップロード
        if rects.len() > self.instance_capacity {
            self.instance_capacity = rects.len().next_power_of_two();
            self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rect_instances"),
                size: (self.instance_capacity * std::mem::size_of::<RectInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !rects.is_empty() {
            self.queue
                .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&rects));
        }

        if image_draws.len() > self.image_instance_capacity {
            self.image_instance_capacity = image_draws.len().next_power_of_two();
            self.image_instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("image_instances"),
                size: (self.image_instance_capacity * std::mem::size_of::<ImageInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !image_draws.is_empty() {
            let instances: Vec<ImageInstance> = image_draws.iter().map(|(_, i)| *i).collect();
            self.queue.write_buffer(
                &self.image_instance_buffer,
                0,
                bytemuck::cast_slice(&instances),
            );
        }

        // TextArea 構築
        let mut text_areas: Vec<TextArea> = Vec::with_capacity(256);
        let wide_pad_x = ((self.cell_w * 2.0 - self.font_size_px) * 0.5).max(0.0);

        status_bar::push_status_bar_text_areas(
            &self.main_status_buffer,
            &self.clock_text,
            &self.clock_buffer,
            &self.version_buffer,
            &self.timer_buffer,
            &overlays.timer.label,
            &self.mem_buffer,
            &overlays.memory.label,
            &self.url_bar_buffer,
            &self.status_btn_buffers,
            layout,
            scale_factor,
            &sb_ctrl,
            is_browser_bar,
            overlays.daemon_version_mismatch,
            &mut text_areas,
        );
        main_pane::push_main_pane_text_areas(
            &self.pane_cache,
            self.cell_w,
            self.cell_h,
            &self.ime_preedit_spans,
            layout,
            &active_pane_rects,
            overlays,
            &main_res,
            wide_pad_x,
            &mut text_areas,
        );
        footer::push_footer_text_areas(
            &self.footer_buffers,
            &footer_columns,
            layout,
            scale_factor,
            &mut text_areas,
        );
        search_bar::push_search_bar_text_areas(
            &self.search_query_buffer,
            &self.search_count_buffer,
            search_bar_layout.as_ref(),
            scale_factor,
            &mut text_areas,
        );
        sidebar::push_sidebar_text_areas(
            &self.col_toggle_buffers,
            &self.card_left_buffers,
            &self.card_right_buffers,
            &self.card_header_keys,
            layout,
            scale_factor,
            time_w,
            &mut text_areas,
        );
        help_modal::push_help_modal_text_areas(
            &self.help_header_buffer,
            &self.help_left_buffer,
            &self.help_right_buffer,
            scale_factor,
            help_modal_rect,
            &mut text_areas,
        );

        if let Err(PrepareError::AtlasFull) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas.iter().cloned(),
            &mut self.swash_cache,
        ) {
            self.atlas.trim();
            let _ = self.text_renderer.prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            );
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return RenderOutcome {
                    cursor_area,
                    needs_redraw: false,
                };
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return RenderOutcome {
                    cursor_area,
                    needs_redraw: true,
                };
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return RenderOutcome {
                    cursor_area,
                    needs_redraw: false,
                };
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tent_frame"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tent_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.06,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if !rects.is_empty() {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                pass.draw(0..6, 0..(rects.len() as u32));
            }

            if !image_draws.is_empty() {
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.image_instance_buffer.slice(..));
                for (idx, (pid, _)) in image_draws.iter().enumerate() {
                    if let Some((_, bg, _, _)) = self.browser_textures.get(pid) {
                        pass.set_bind_group(1, bg, &[]);
                        let i = idx as u32;
                        pass.draw(0..6, i..(i + 1));
                    }
                }
            }

            let _ = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
        RenderOutcome {
            cursor_area,
            needs_redraw: false,
        }
    }
}
