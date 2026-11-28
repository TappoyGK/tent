use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, TextArea, Weight};

use crate::layout::{AppLayout, Rect};
use crate::terminal_pane::PaneId;

use super::primitives::{RectInstance, safe_bounds};
use super::{RenderOverlays, Renderer};

/// 描画された検索バーの矩形情報（テキスト配置用）
pub(super) struct SearchBarLayout {
    pub bar_rect: Rect,
    pub query_rect: Rect,
    pub count_rect: Rect,
}

impl Renderer {
    pub(super) fn build_search_bar(
        &mut self,
        active_pane_rects: &[(PaneId, Rect)],
        layout: &AppLayout,
        scale_factor: f32,
        overlays: &RenderOverlays<'_>,
        rects: &mut Vec<RectInstance>,
    ) -> Option<SearchBarLayout> {
        let sb = overlays.search_bar.as_ref()?;
        let bar_h = (26.0 * scale_factor).round().max(22.0);
        let pad = (8.0 * scale_factor).round();

        let bar_rect = if sb.is_browser {
            // ブラウザペインは WKWebView が最前面のため、上部ステータスバー内へ検索バーを重ねる
            let st = layout.main_status_rect;
            let bar_w = (320.0 * scale_factor).min((st.w - 16.0 * scale_factor).max(140.0));
            let bh = (st.h - 4.0 * scale_factor).max(18.0);
            let bx = st.x + ((st.w - bar_w) * 0.5).max(4.0);
            let by = st.y + (st.h - bh) * 0.5;
            Rect::new(bx, by, bar_w, bh)
        } else {
            let prect = active_pane_rects
                .iter()
                .find(|(pid, _)| *pid == sb.pane_id)
                .map(|(_, r)| *r)
                .unwrap_or(layout.main_inner_rect);
            let bar_w = (280.0 * scale_factor).min((prect.w - pad * 2.0).max(120.0));
            let bx = (prect.right() - bar_w - pad).max(prect.x + pad);
            let by = prect.y + pad;
            Rect::new(bx, by, bar_w, bar_h)
        };

        // 背景・枠線
        self.push_rect(rects, bar_rect, [0.12, 0.15, 0.21, 0.97]);
        self.push_border(
            rects,
            bar_rect,
            (1.5 * scale_factor).max(1.0),
            [0.36, 0.68, 0.96, 0.95],
        );

        let count_w = if sb.count_label.is_empty() {
            0.0
        } else {
            (sb.count_label.chars().count() as f32 * 7.2 * scale_factor + 12.0 * scale_factor)
                .min(bar_rect.w * 0.45)
        };
        let inner_pad = 8.0 * scale_factor;
        let query_rect = Rect::new(
            bar_rect.x + inner_pad,
            bar_rect.y,
            (bar_rect.w - inner_pad * 2.0 - count_w).max(24.0),
            bar_rect.h,
        );
        let count_rect = Rect::new(
            bar_rect.right() - inner_pad - count_w,
            bar_rect.y,
            count_w,
            bar_rect.h,
        );

        // 全選択ハイライト
        if sb.select_all && !sb.query.is_empty() {
            let sel_w = ((sb.query.chars().count() as f32) * 7.5 * scale_factor)
                .min(query_rect.w)
                .max(6.0);
            self.push_rect(
                rects,
                Rect::new(
                    query_rect.x,
                    bar_rect.y + 4.0 * scale_factor,
                    sel_w,
                    (bar_rect.h - 8.0 * scale_factor).max(8.0),
                ),
                [0.24, 0.46, 0.78, 0.55],
            );
        }

        let font_size = (11.5 * scale_factor).round().max(10.0);
        let metrics = Metrics::new(font_size, (font_size * 1.35).ceil());
        let scale_key = (scale_factor * 100.0).round() as u32;
        let display_query = if sb.query.is_empty() {
            "Find..."
        } else {
            sb.query
        };
        let key = (
            display_query.to_string(),
            sb.count_label.clone(),
            sb.query.is_empty(),
            bar_rect.w.round() as u32,
            scale_key,
        );

        if self.search_cache_key != key {
            self.search_query_buffer.set_metrics(metrics);
            self.search_query_buffer
                .set_size(Some(query_rect.w), Some(bar_rect.h));
            let query_color = if sb.query.is_empty() {
                Color::rgb(0x6c, 0x76, 0x8b)
            } else {
                Color::rgb(0xec, 0xf1, 0xf8)
            };
            self.search_query_buffer.set_text(
                display_query,
                &Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(query_color),
                Shaping::Advanced,
                None,
            );
            self.search_query_buffer
                .shape_until_scroll(&mut self.font_system, false);

            let count_fs = (10.5 * scale_factor).round().max(9.0);
            let count_metrics = Metrics::new(count_fs, (count_fs * 1.35).ceil());
            self.search_count_buffer.set_metrics(count_metrics);
            self.search_count_buffer
                .set_size(Some(count_rect.w.max(1.0)), Some(bar_rect.h));
            let count_color = if sb.count_label == "0/0" {
                Color::rgb(0xe8, 0x6a, 0x6a)
            } else {
                Color::rgb(0x9a, 0xa6, 0xbd)
            };
            self.search_count_buffer.set_text(
                &sb.count_label,
                &Attrs::new()
                    .family(Family::Name("Menlo"))
                    .weight(Weight::NORMAL)
                    .color(count_color),
                Shaping::Basic,
                None,
            );
            self.search_count_buffer
                .shape_until_scroll(&mut self.font_system, false);

            self.search_cache_key = key;
        }

        Some(SearchBarLayout {
            bar_rect,
            query_rect,
            count_rect,
        })
    }
}

pub(super) fn push_search_bar_text_areas<'a>(
    query_buffer: &'a Buffer,
    count_buffer: &'a Buffer,
    sb_layout: Option<&SearchBarLayout>,
    scale_factor: f32,
    text_areas: &mut Vec<TextArea<'a>>,
) {
    let Some(sb) = sb_layout else {
        return;
    };
    let font_size = (11.5 * scale_factor).round().max(10.0);
    let line_h = (font_size * 1.35).ceil();
    let top = sb.bar_rect.y + ((sb.bar_rect.h - line_h) * 0.5).max(1.0);

    text_areas.push(TextArea {
        buffer: query_buffer,
        left: sb.query_rect.x,
        top,
        scale: 1.0,
        bounds: safe_bounds(
            sb.query_rect.x,
            sb.bar_rect.y,
            sb.query_rect.right(),
            sb.bar_rect.bottom(),
        ),
        default_color: Color::rgb(0xec, 0xf1, 0xf8),
        custom_glyphs: &[],
    });

    if sb.count_rect.w > 1.0 {
        let count_fs = (10.5 * scale_factor).round().max(9.0);
        let count_lh = (count_fs * 1.35).ceil();
        let count_top = sb.bar_rect.y + ((sb.bar_rect.h - count_lh) * 0.5).max(1.0);
        text_areas.push(TextArea {
            buffer: count_buffer,
            left: sb.count_rect.x,
            top: count_top,
            scale: 1.0,
            bounds: safe_bounds(
                sb.count_rect.x,
                sb.bar_rect.y,
                sb.count_rect.right(),
                sb.bar_rect.bottom(),
            ),
            default_color: Color::rgb(0x9a, 0xa6, 0xbd),
            custom_glyphs: &[],
        });
    }
}
