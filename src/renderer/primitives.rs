use bytemuck::{Pod, Zeroable};
use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, TextBounds, Weight};

use crate::layout::Rect;

use super::Renderer;

pub(super) const RECT_SHADER: &str = r#"
struct Uniforms {
    screen_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) rect_pos: vec2<f32>,
    @location(1) rect_size: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[in.vertex_index];
    let px = in.rect_pos + corner * in.rect_size;
    let ndc_x = (px.x / uniforms.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / uniforms.screen_size.y) * 2.0;

    var out: VertexOutput;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

pub(super) const IMAGE_SHADER: &str = r#"
struct Uniforms {
    screen_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(1) @binding(0)
var img_tex: texture_2d<f32>;

@group(1) @binding(1)
var img_sampler: sampler;

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) rect_pos: vec2<f32>,
    @location(1) rect_size: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[in.vertex_index];
    let px = in.rect_pos + corner * in.rect_size;
    let ndc_x = (px.x / uniforms.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / uniforms.screen_size.y) * 2.0;

    var out: VertexOutput;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(img_tex, img_sampler, in.uv);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct ScreenUniforms {
    pub(super) screen_size: [f32; 2],
    pub(super) _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct RectInstance {
    pub(super) pos: [f32; 2],
    pub(super) size: [f32; 2],
    pub(super) color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct ImageInstance {
    pub(super) pos: [f32; 2],
    pub(super) size: [f32; 2],
}

impl Renderer {
    pub(super) fn push_rect(&self, rects: &mut Vec<RectInstance>, rect: Rect, color: [f32; 4]) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let color = if self.is_srgb {
            [
                srgb_to_linear(color[0]),
                srgb_to_linear(color[1]),
                srgb_to_linear(color[2]),
                color[3],
            ]
        } else {
            color
        };
        rects.push(RectInstance {
            pos: [rect.x, rect.y],
            size: [rect.w, rect.h],
            color,
        });
    }

    /// ステータスバー右端付近に Wi-Fi 4本電波バーを描画する
    pub(super) fn push_wifi_icon(
        &self,
        rects: &mut Vec<RectInstance>,
        x: f32,
        y: f32,
        h: f32,
        scale: f32,
        wifi: crate::macos::WifiStatus,
    ) {
        let s = scale.max(1.0);
        let bar_w = (2.2 * s).max(2.0);
        let bar_gap = (1.5 * s).max(1.0);
        let bottom = y + h - 4.0 * s;
        let heights = [3.0 * s, 5.5 * s, 8.0 * s, 10.5 * s];

        let (active_bars, lit_color, dim_color) = match wifi {
            crate::macos::WifiStatus::Connected { bars, .. } => {
                let lit = if bars <= 1 {
                    [0.94, 0.72, 0.25, 1.0]
                } else {
                    [0.42, 0.84, 0.68, 1.0]
                };
                (bars, lit, [0.24, 0.27, 0.34, 1.0])
            }
            crate::macos::WifiStatus::Disconnected => {
                (0, [0.85, 0.35, 0.35, 1.0], [0.55, 0.25, 0.27, 0.85])
            }
        };

        for (i, bh) in heights.iter().enumerate() {
            let bx = x + (i as f32) * (bar_w + bar_gap);
            let color = if (i as u8) < active_bars {
                lit_color
            } else {
                dim_color
            };
            self.push_rect(rects, Rect::new(bx, bottom - *bh, bar_w, *bh), color);
        }
    }

    /// ブラウザヘッダ右端に音声再生中のスピーカーマークを描画する
    pub(super) fn push_speaker_icon(
        &self,
        rects: &mut Vec<RectInstance>,
        header: Rect,
        scale: f32,
    ) {
        let s = scale.max(1.0);
        let cy = header.y + header.h * 0.5;
        let rx = header.right() - 18.0 * s;
        self.push_speaker_icon_at(rects, rx, cy, scale);
    }

    pub(super) fn push_speaker_icon_at(
        &self,
        rects: &mut Vec<RectInstance>,
        rx: f32,
        cy: f32,
        scale: f32,
    ) {
        let color = [0.34, 0.88, 0.94, 1.0];
        let s = scale.max(1.0);

        // スピーカー本体（左の小さな箱＋広がるコーン）
        self.push_rect(rects, Rect::new(rx, cy - 2.0 * s, 2.5 * s, 4.0 * s), color);
        self.push_rect(
            rects,
            Rect::new(rx + 2.5 * s, cy - 3.5 * s, 2.0 * s, 7.0 * s),
            color,
        );
        self.push_rect(
            rects,
            Rect::new(rx + 4.5 * s, cy - 5.0 * s, 2.0 * s, 10.0 * s),
            color,
        );

        // 音波バー（内・外）
        let wave_w = (1.5 * s).max(1.5);
        self.push_rect(
            rects,
            Rect::new(rx + 8.5 * s, cy - 2.5 * s, wave_w, 5.0 * s),
            color,
        );
        self.push_rect(
            rects,
            Rect::new(rx + 11.5 * s, cy - 4.5 * s, wave_w, 9.0 * s),
            color,
        );
    }

    /// タイマー左隣の砂時計アイコン（稼働中は中央から砂粒がさらさら落ちるアニメーション）
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_hourglass_icon(
        &self,
        rects: &mut Vec<RectInstance>,
        rect: Rect,
        scale: f32,
        progress: f32,
        is_running: bool,
        is_finished: bool,
        anim_tick: u32,
    ) {
        let s = scale.max(1.0);
        let w = 10.0 * s;
        let h = 13.0 * s;
        let x0 = rect.x + (rect.w - w) * 0.5;
        let y0 = rect.y + (rect.h - h) * 0.5;
        let cx = x0 + w * 0.5;

        let frame_color = if is_finished {
            [0.95, 0.42, 0.30, 1.0]
        } else if is_running {
            [0.72, 0.80, 0.92, 1.0]
        } else {
            [0.44, 0.50, 0.62, 0.9]
        };
        let sand_color = if is_finished {
            [0.95, 0.42, 0.30, 1.0]
        } else if is_running {
            [0.95, 0.80, 0.38, 1.0]
        } else {
            [0.48, 0.54, 0.66, 0.7]
        };

        let cap_h = (1.5 * s).max(1.0);
        let wall = (1.1 * s).max(1.0);
        // 上下のフタ
        self.push_rect(rects, Rect::new(x0, y0, w, cap_h), frame_color);
        self.push_rect(rects, Rect::new(x0, y0 + h - cap_h, w, cap_h), frame_color);

        // ガラス側壁（上・下をくびれへ絞る4段のドット）
        let top_y = y0 + cap_h;
        let bot_y = y0 + h - cap_h;
        let mid_y = y0 + h * 0.5;
        let half_h = (mid_y - top_y).max(1.0);
        for step in 0..4 {
            let t = (step as f32) / 4.0;
            let seg_h = (half_h / 4.0).ceil();
            let inset = t * (w * 0.34);
            // 上半分（上からくびれへ）
            let uy = top_y + t * half_h;
            self.push_rect(rects, Rect::new(x0 + inset, uy, wall, seg_h), frame_color);
            self.push_rect(
                rects,
                Rect::new(x0 + w - inset - wall, uy, wall, seg_h),
                frame_color,
            );
            // 下半分（くびれから下へ）
            let ly = mid_y + (1.0 - t - 0.25) * half_h;
            self.push_rect(rects, Rect::new(x0 + inset, ly, wall, seg_h), frame_color);
            self.push_rect(
                rects,
                Rect::new(x0 + w - inset - wall, ly, wall, seg_h),
                frame_color,
            );
        }

        // 上の砂（残り割合 progress に応じて減る）
        let p = if is_finished {
            0.0
        } else if is_running {
            progress.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let top_sand_h = (half_h - 1.0 * s) * p;
        if top_sand_h >= 1.0 {
            let layers = 3;
            for l in 0..layers {
                let frac = (l as f32 + 0.5) / (layers as f32);
                let y = mid_y - top_sand_h * frac;
                let lw = ((w - wall * 2.4) * (0.25 + 0.65 * (mid_y - y) / half_h)).max(1.5 * s);
                self.push_rect(
                    rects,
                    Rect::new(cx - lw * 0.5, y, lw, (top_sand_h / layers as f32).ceil()),
                    sand_color,
                );
            }
        }

        // 下の砂（経過割合 1.0 - p に応じて積もる）
        let bot_fill = if is_finished {
            1.0
        } else if is_running {
            (1.0 - p).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let bot_sand_h = (half_h - 1.0 * s) * bot_fill;
        if bot_sand_h >= 1.0 {
            let layers = 3;
            for l in 0..layers {
                let frac = (l as f32 + 1.0) / (layers as f32);
                let y = bot_y - bot_sand_h * frac;
                let lw = ((w - wall * 2.4) * (0.25 + 0.65 * (y - mid_y) / half_h)).max(1.5 * s);
                self.push_rect(
                    rects,
                    Rect::new(cx - lw * 0.5, y, lw, (bot_sand_h / layers as f32).ceil()),
                    sand_color,
                );
            }
        }

        // 稼働中のみ：くびれから下の砂山へさらさら落ちる砂粒アニメーション
        if is_running && !is_finished {
            let grain = (1.3 * s).max(1.0);
            let stream_top = mid_y - 0.5 * s;
            let stream_bot = (bot_y - bot_sand_h).max(mid_y + 1.0 * s);
            let span = (stream_bot - stream_top).max(1.0);
            let phase = (anim_tick % 4) as f32 / 4.0;
            for k in 0..2 {
                let pos = (phase + (k as f32) * 0.5) % 1.0;
                let gy = stream_top + pos * span;
                self.push_rect(
                    rects,
                    Rect::new(cx - grain * 0.5, gy, grain, grain),
                    sand_color,
                );
            }
        }
    }

    pub(super) fn push_border(
        &self,
        rects: &mut Vec<RectInstance>,
        rect: Rect,
        w: f32,
        color: [f32; 4],
    ) {
        self.push_rect(rects, Rect::new(rect.x, rect.y, rect.w, w), color);
        self.push_rect(
            rects,
            Rect::new(rect.x, rect.bottom() - w, rect.w, w),
            color,
        );
        self.push_rect(rects, Rect::new(rect.x, rect.y, w, rect.h), color);
        self.push_rect(rects, Rect::new(rect.right() - w, rect.y, w, rect.h), color);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_box_or_block(
        &self,
        rects: &mut Vec<RectInstance>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scale: f32,
        ch: char,
        color: [f32; 4],
    ) {
        let thin = (1.0 * scale).round().max(1.0);
        let thick = (2.0 * scale).round().max(2.0);
        let cx = x + (w - thin) * 0.5;
        let cy = y + (h - thin) * 0.5;

        match ch {
            '\u{2588}' => self.push_rect(rects, Rect::new(x, y, w, h), color),
            '\u{2580}' => self.push_rect(rects, Rect::new(x, y, w, h * 0.5), color),
            '\u{2581}'..='\u{2587}' => {
                let frac = ((ch as u32) - 0x2580) as f32 / 8.0;
                let bh = h * frac;
                self.push_rect(rects, Rect::new(x, y + h - bh, w, bh), color);
            }
            '\u{2589}'..='\u{258F}' => {
                let frac = (0x2590 - (ch as u32)) as f32 / 8.0;
                self.push_rect(rects, Rect::new(x, y, w * frac, h), color);
            }
            '\u{2590}' => self.push_rect(rects, Rect::new(x + w * 0.5, y, w * 0.5, h), color),
            '\u{2591}' => self.push_rect(
                rects,
                Rect::new(x, y, w, h),
                [color[0], color[1], color[2], color[3] * 0.25],
            ),
            '\u{2592}' => self.push_rect(
                rects,
                Rect::new(x, y, w, h),
                [color[0], color[1], color[2], color[3] * 0.50],
            ),
            '\u{2593}' => self.push_rect(
                rects,
                Rect::new(x, y, w, h),
                [color[0], color[1], color[2], color[3] * 0.75],
            ),
            '\u{2594}' => self.push_rect(rects, Rect::new(x, y, w, h / 8.0), color),
            '\u{2595}' => {
                let bw = w / 8.0;
                self.push_rect(rects, Rect::new(x + w - bw, y, bw, h), color);
            }

            '\u{2500}' => self.push_rect(rects, Rect::new(x, cy, w, thin), color),
            '\u{2501}' => {
                self.push_rect(rects, Rect::new(x, y + (h - thick) * 0.5, w, thick), color);
            }
            '\u{2502}' => self.push_rect(rects, Rect::new(cx, y, thin, h), color),
            '\u{2503}' => {
                self.push_rect(rects, Rect::new(x + (w - thick) * 0.5, y, thick, h), color);
            }

            '\u{250C}' | '\u{256D}' => {
                self.push_rect(rects, Rect::new(cx, cy, x + w - cx, thin), color);
                self.push_rect(rects, Rect::new(cx, cy, thin, y + h - cy), color);
            }
            '\u{2510}' | '\u{256E}' => {
                self.push_rect(rects, Rect::new(x, cy, cx + thin - x, thin), color);
                self.push_rect(rects, Rect::new(cx, cy, thin, y + h - cy), color);
            }
            '\u{2514}' | '\u{2570}' => {
                self.push_rect(rects, Rect::new(cx, y, thin, cy + thin - y), color);
                self.push_rect(rects, Rect::new(cx, cy, x + w - cx, thin), color);
            }
            '\u{2518}' | '\u{256F}' => {
                self.push_rect(rects, Rect::new(cx, y, thin, cy + thin - y), color);
                self.push_rect(rects, Rect::new(x, cy, cx + thin - x, thin), color);
            }

            '\u{251C}' => {
                self.push_rect(rects, Rect::new(cx, y, thin, h), color);
                self.push_rect(rects, Rect::new(cx, cy, x + w - cx, thin), color);
            }
            '\u{2524}' => {
                self.push_rect(rects, Rect::new(cx, y, thin, h), color);
                self.push_rect(rects, Rect::new(x, cy, cx + thin - x, thin), color);
            }
            '\u{252C}' => {
                self.push_rect(rects, Rect::new(x, cy, w, thin), color);
                self.push_rect(rects, Rect::new(cx, cy, thin, y + h - cy), color);
            }
            '\u{2534}' => {
                self.push_rect(rects, Rect::new(x, cy, w, thin), color);
                self.push_rect(rects, Rect::new(cx, y, thin, cy + thin - y), color);
            }
            '\u{253C}' => {
                self.push_rect(rects, Rect::new(x, cy, w, thin), color);
                self.push_rect(rects, Rect::new(cx, y, thin, h), color);
            }
            _ => {}
        }
    }
}

pub(super) fn shorten_home_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = std::path::Path::new(&home);
        if let Ok(rel) = path.strip_prefix(home_path) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub(super) fn is_wide_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x1100..=0x115F
            | 0x2329..=0x232A
            | 0x2E80..=0x303E
            | 0x3040..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x2FFFD
            | 0x30000..=0x3FFFD
    )
}

pub(super) fn safe_bounds(left: f32, top: f32, right: f32, bottom: f32) -> TextBounds {
    let l = left.round() as i32;
    let t = top.round() as i32;
    let r = (right.round() as i32).max(l);
    let b = (bottom.round() as i32).max(t);
    TextBounds {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

pub(super) fn build_help_spans(text: &str) -> Vec<(String, Attrs<'static>)> {
    let mut spans = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with('[') {
            spans.push((
                line.to_string(),
                Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(Color::rgb(0x5c, 0xad, 0xf5))
                    .weight(Weight::BOLD),
            ));
        } else if line.starts_with("  ") && line.len() > 19 {
            let split_at = line
                .char_indices()
                .nth(19)
                .map(|(idx, _)| idx)
                .unwrap_or(line.len());
            spans.push((
                line[..split_at].to_string(),
                Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(Color::rgb(0xf2, 0xd4, 0x6b))
                    .weight(Weight::BOLD),
            ));
            spans.push((
                line[split_at..].to_string(),
                Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(Color::rgb(0xd4, 0xdc, 0xea)),
            ));
        } else {
            spans.push((
                line.to_string(),
                Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(Color::rgb(0xd4, 0xdc, 0xea)),
            ));
        }
    }
    spans
}

pub(super) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn measure_mono_cell(font_system: &mut FontSystem, font_size: f32) -> (f32, f32) {
    let line_h = (font_size * 1.32).ceil();
    let metrics = Metrics::new(font_size, line_h);
    let mut buf = Buffer::new(font_system, metrics);
    buf.set_text(
        "MMMMMMMMMM",
        &Attrs::new().family(Family::Name("Menlo")),
        Shaping::Basic,
        None,
    );
    buf.shape_until_scroll(font_system, false);

    let measured_w = buf
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first().map(|g| g.w))
        .unwrap_or(font_size * 0.6);

    (measured_w.max(4.0), line_h)
}
