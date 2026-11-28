use std::collections::HashSet;

use ghostty_vt::render::CursorVisualStyle;
use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, Style, TextArea, Weight, Wrap};

use crate::layout::{AppLayout, Rect};
use crate::slot::Workspace;
use crate::terminal_pane::PaneId;

use super::primitives::{RectInstance, is_wide_char, safe_bounds};
use super::{CachedMainSpan, CachedPaneVisual, RenderOverlays, Renderer};

pub(super) struct MainPaneBuildResult {
    pub cursor_area: Option<(f32, f32, f32, f32)>,
    pub ime_occlude: Option<(PaneId, usize, f32, f32)>,
}

impl Renderer {
    pub(super) fn update_active_pane_cache(
        &mut self,
        workspace: &mut Workspace,
        active_idx: usize,
        active_pane_rects: &[(PaneId, Rect)],
    ) {
        let active_ids: HashSet<PaneId> = active_pane_rects.iter().map(|(id, _)| *id).collect();
        self.pane_cache.retain(|id, _| active_ids.contains(id));

        if let Some(root) = workspace.slots[active_idx].root.as_mut() {
            let now = std::time::Instant::now();
            let metrics = Metrics::new(self.font_size_px, self.cell_h);
            for &(pid, rect) in active_pane_rects {
                let width_px = rect.w.round() as u32;
                if let Some(pane) = root.terminal_pane_mut(pid) {
                    let prev_entry = self.pane_cache.remove(&pid);
                    let width_changed = prev_entry.as_ref().is_none_or(|c| c.width_px != width_px);
                    let gen_changed = prev_entry
                        .as_ref()
                        .is_none_or(|c| c.generation != pane.generation);

                    // Synchronized Output (Mode 2026) 中は中間フレームを描画せず前の完成フレームを維持する
                    if !width_changed && pane.is_render_held(now) && prev_entry.is_some() {
                        if let Some(entry) = prev_entry {
                            self.pane_cache.insert(pid, entry);
                        }
                        continue;
                    }

                    if !width_changed && !gen_changed {
                        if let Some(entry) = prev_entry {
                            self.pane_cache.insert(pid, entry);
                        }
                        continue;
                    }

                    let generation = pane.generation;
                    let (prev_snap, mut row_spans) = match prev_entry {
                        Some(entry) if !width_changed => (Some(entry.snapshot), entry.row_spans),
                        _ => (None, Vec::new()),
                    };

                    if let Some((snap, dirty_rows)) =
                        pane.update_main_snapshot(prev_snap, width_changed)
                    {
                        row_spans.resize_with(snap.rows.len(), Vec::new);
                        for row_i in dirty_rows {
                            let row_bufs = &mut row_spans[row_i];
                            row_bufs.clear();
                            for s in &snap.rows[row_i].spans {
                                let mut buf = Buffer::new(&mut self.font_system, metrics);
                                buf.set_wrap(Wrap::None);
                                buf.set_size(Some(rect.w), Some(self.cell_h));

                                let is_ascii = s.text.is_ascii();
                                let family = if is_ascii {
                                    Family::Name("Menlo")
                                } else {
                                    // 日本語漢字の正しい字形（ヒラギノ角ゴシック）を優先し中華フォント落ちを防ぐ
                                    Family::Name("Hiragino Sans")
                                };
                                let mut attrs = Attrs::new()
                                    .family(family)
                                    .color(Color::rgb(s.fg[0], s.fg[1], s.fg[2]));
                                if s.bold {
                                    attrs = attrs.weight(Weight::BOLD);
                                }
                                if s.italic {
                                    attrs = attrs.style(Style::Italic);
                                }
                                let shaping = if is_ascii {
                                    Shaping::Basic
                                } else {
                                    Shaping::Advanced
                                };
                                buf.set_text(&s.text, &attrs, shaping, None);
                                buf.shape_until_scroll(&mut self.font_system, false);

                                row_bufs.push(CachedMainSpan {
                                    start_col: s.start_col,
                                    is_wide: s.is_wide,
                                    buffer: buf,
                                });
                            }
                        }

                        self.pane_cache.insert(
                            pid,
                            CachedPaneVisual {
                                generation,
                                width_px,
                                snapshot: snap,
                                row_spans,
                            },
                        );
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_main_panes(
        &mut self,
        active_pane_rects: &[(PaneId, Rect)],
        focused_pane_id: Option<PaneId>,
        scale_factor: f32,
        overlays: &RenderOverlays<'_>,
        rects: &mut Vec<RectInstance>,
    ) -> MainPaneBuildResult {
        let mut cursor_area: Option<(f32, f32, f32, f32)> = None;
        let mut ime_occlude: Option<(PaneId, usize, f32, f32)> = None;
        let multi_pane = active_pane_rects.len() > 1;

        for &(pid, prect) in active_pane_rects {
            let is_focused = focused_pane_id == Some(pid);
            let Some(cached) = self.pane_cache.get(&pid) else {
                // ブラウザペイン（WKWebView が上に重なる）は背景とフォーカス枠線のみ描画
                self.push_rect(rects, prect, [0.09, 0.11, 0.15, 1.0]);
                if multi_pane {
                    let (bcolor, bw) = if is_focused {
                        ([0.30, 0.78, 0.88, 0.90], 2.0 * scale_factor)
                    } else {
                        ([0.20, 0.25, 0.32, 0.80], 1.0 * scale_factor)
                    };
                    let outer = Rect::new(
                        prect.x - bw,
                        prect.y - bw,
                        prect.w + bw * 2.0,
                        prect.h + bw * 2.0,
                    );
                    self.push_border(rects, outer, bw, bcolor);
                }
                continue;
            };
            let snap = &cached.snapshot;

            // ペイン背景
            self.push_rect(rects, prect, snap.default_bg);

            // セル背景・ブロック要素・罫線文字
            for row in &snap.rows {
                for bg in &row.cell_bgs {
                    let x = prect.x + (bg.col as f32) * self.cell_w;
                    let y = prect.y + (bg.row as f32) * self.cell_h;
                    self.push_rect(rects, Rect::new(x, y, self.cell_w, self.cell_h), bg.color);
                }
            }

            // Cmd ホバー中 URL の下線ハイライト
            if let Some(hover) = overlays.hovered_url
                && hover.pane_id == pid
            {
                let ux = prect.x + (hover.start_col as f32) * self.cell_w;
                let uy = prect.y + ((hover.row + 1) as f32) * self.cell_h
                    - (1.5 * scale_factor).max(1.5);
                let uw = ((hover.end_col - hover.start_col + 1) as f32) * self.cell_w;
                self.push_rect(
                    rects,
                    Rect::new(
                        ux,
                        uy,
                        uw.min((prect.right() - ux).max(0.0)),
                        (1.5 * scale_factor).max(1.5),
                    ),
                    [0.40, 0.76, 0.98, 0.95],
                );
            }

            // ペイン内検索マッチのハイライト
            if let Some(ref sh) = overlays.search_highlights
                && sh.pane_id == pid
            {
                for &(row, c0, c1, is_current) in &sh.matches {
                    if c1 >= c0 {
                        let sx = prect.x + (c0 as f32) * self.cell_w;
                        let sy = prect.y + (row as f32) * self.cell_h;
                        let sw = ((c1 - c0 + 1) as f32) * self.cell_w;
                        let color = if is_current {
                            [0.95, 0.52, 0.16, 0.65]
                        } else {
                            [0.85, 0.72, 0.20, 0.30]
                        };
                        self.push_rect(
                            rects,
                            Rect::new(sx, sy, sw.min((prect.right() - sx).max(0.0)), self.cell_h),
                            color,
                        );
                    }
                }
            }

            // マウス選択範囲のハイライト
            if let Some(sel) = overlays.selection
                && sel.pane_id == pid
            {
                let ((sc, sr), (ec, er)) = if (sel.start.1, sel.start.0) <= (sel.end.1, sel.end.0) {
                    (sel.start, sel.end)
                } else {
                    (sel.end, sel.start)
                };
                let max_cols = (prect.w / self.cell_w).floor().max(1.0) as u16;
                for row in sr..=er {
                    let c0 = if row == sr { sc } else { 0 };
                    let c1 = if row == er {
                        ec
                    } else {
                        max_cols.saturating_sub(1)
                    };
                    if c1 >= c0 {
                        let sx = prect.x + (c0 as f32) * self.cell_w;
                        let sy = prect.y + (row as f32) * self.cell_h;
                        let sw = ((c1 - c0 + 1) as f32) * self.cell_w;
                        self.push_rect(
                            rects,
                            Rect::new(sx, sy, sw.min(prect.right() - sx), self.cell_h),
                            [0.25, 0.48, 0.78, 0.45],
                        );
                    }
                }
            }

            for row in &snap.rows {
                for g in &row.box_glyphs {
                    let x = prect.x + (g.col as f32) * self.cell_w;
                    let y = prect.y + (g.row as f32) * self.cell_h;
                    self.push_box_or_block(
                        rects,
                        x,
                        y,
                        self.cell_w,
                        self.cell_h,
                        scale_factor,
                        g.ch,
                        g.color,
                    );
                }
            }

            // カーソルと IME（visible == false の TUI でも位置と Preedit は表示する）
            if let Some(cur) = snap.cursor {
                let x = prect.x + (cur.x as f32) * self.cell_w;
                let y = prect.y + (cur.y as f32) * self.cell_h;

                if is_focused {
                    cursor_area = Some((x, y, self.cell_w, self.cell_h));
                    if cur.visible {
                        let cursor_rect = match cur.style {
                            CursorVisualStyle::Bar => {
                                Rect::new(x, y, (2.0 * scale_factor).max(2.0), self.cell_h)
                            }
                            CursorVisualStyle::Underline => Rect::new(
                                x,
                                y + self.cell_h - (2.5 * scale_factor).max(2.0),
                                self.cell_w,
                                (2.5 * scale_factor).max(2.0),
                            ),
                            _ => Rect::new(x, y, self.cell_w, self.cell_h),
                        };
                        self.push_rect(
                            rects,
                            cursor_rect,
                            [cur.color[0], cur.color[1], cur.color[2], 0.72],
                        );
                    }

                    // IME変換中（Preedit）
                    if !overlays.ime_preedit.is_empty() {
                        if self.cached_ime_preedit != overlays.ime_preedit {
                            let metrics = Metrics::new(self.font_size_px, self.cell_h);
                            let ascii_attrs = Attrs::new()
                                .family(Family::Name("Menlo"))
                                .color(Color::rgb(0xff, 0xff, 0xff));
                            let ja_attrs = Attrs::new()
                                .family(Family::Name("Hiragino Sans"))
                                .color(Color::rgb(0xff, 0xff, 0xff));
                            self.ime_preedit_spans.clear();
                            let mut col_off: u16 = 0;
                            for ch in overlays.ime_preedit.chars() {
                                let is_wide = is_wide_char(ch);
                                let mut buf = Buffer::new(&mut self.font_system, metrics);
                                buf.set_wrap(Wrap::None);
                                buf.set_size(Some(self.cell_w * 3.0), Some(self.cell_h));
                                let s = ch.to_string();
                                let (attrs, shaping) = if ch.is_ascii() {
                                    (&ascii_attrs, Shaping::Basic)
                                } else {
                                    (&ja_attrs, Shaping::Advanced)
                                };
                                buf.set_text(&s, attrs, shaping, None);
                                buf.shape_until_scroll(&mut self.font_system, false);
                                self.ime_preedit_spans.push((col_off, is_wide, buf));
                                col_off = col_off.saturating_add(if is_wide { 2 } else { 1 });
                            }
                            self.cached_ime_preedit = overlays.ime_preedit.to_string();
                        }
                        let total_cols: u16 = self
                            .ime_preedit_spans
                            .last()
                            .map(|(col, is_wide, _)| col + if *is_wide { 2 } else { 1 })
                            .unwrap_or(1);
                        let preedit_w = ((total_cols as f32) * self.cell_w)
                            .min((prect.right() - x).max(self.cell_w));
                        ime_occlude = Some((pid, cur.y as usize, x, x + preedit_w));
                        self.push_rect(
                            rects,
                            Rect::new(x, y, preedit_w, self.cell_h),
                            [0.18, 0.24, 0.34, 0.96],
                        );
                        let ul_h = (2.0 * scale_factor).max(2.0);
                        self.push_rect(
                            rects,
                            Rect::new(x, y + self.cell_h - ul_h, preedit_w, ul_h),
                            [0.45, 0.78, 0.98, 1.0],
                        );
                    }
                } else if cur.visible {
                    self.push_border(
                        rects,
                        Rect::new(x, y, self.cell_w, self.cell_h),
                        1.0 * scale_factor,
                        [cur.color[0], cur.color[1], cur.color[2], 0.45],
                    );
                }
            }

            if multi_pane {
                let (bcolor, bw) = if is_focused {
                    ([0.36, 0.68, 0.96, 0.85], 1.5 * scale_factor)
                } else {
                    ([0.20, 0.23, 0.29, 0.80], 1.0 * scale_factor)
                };
                self.push_border(rects, prect, bw, bcolor);
            }
        }
        MainPaneBuildResult {
            cursor_area,
            ime_occlude,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_main_pane_text_areas<'a>(
    pane_cache: &'a std::collections::HashMap<PaneId, CachedPaneVisual>,
    cell_w: f32,
    cell_h: f32,
    ime_preedit_spans: &'a [(u16, bool, Buffer)],
    layout: &AppLayout,
    active_pane_rects: &[(PaneId, Rect)],
    overlays: &RenderOverlays<'_>,
    main_res: &MainPaneBuildResult,
    wide_pad_x: f32,
    text_areas: &mut Vec<TextArea<'a>>,
) {
    let cursor_area = main_res.cursor_area;
    let ime_occlude = main_res.ime_occlude;

    for &(pid, prect) in active_pane_rects {
        let Some(cached) = pane_cache.get(&pid) else {
            continue;
        };
        let pane_bounds = safe_bounds(prect.x, prect.y, prect.right(), prect.bottom());

        for (row_i, row_spans) in cached.row_spans.iter().enumerate() {
            let top = prect.y + (row_i as f32) * cell_h;
            for span in row_spans {
                let left = prect.x
                    + (span.start_col as f32) * cell_w
                    + if span.is_wide { wide_pad_x } else { 0.0 };

                if let Some((ime_pid, ime_row, occ_left, occ_right)) = ime_occlude
                    && ime_pid == pid
                    && row_i == ime_row
                {
                    let left_clip = safe_bounds(
                        prect.x,
                        prect.y,
                        occ_left.min(prect.right()),
                        prect.bottom(),
                    );
                    if left_clip.right > left_clip.left && left < occ_left {
                        text_areas.push(TextArea {
                            buffer: &span.buffer,
                            left,
                            top,
                            scale: 1.0,
                            bounds: left_clip,
                            default_color: Color::rgb(0xd8, 0xde, 0xe9),
                            custom_glyphs: &[],
                        });
                    }
                    let right_clip = safe_bounds(
                        occ_right.max(prect.x),
                        prect.y,
                        prect.right(),
                        prect.bottom(),
                    );
                    if right_clip.right > right_clip.left {
                        text_areas.push(TextArea {
                            buffer: &span.buffer,
                            left,
                            top,
                            scale: 1.0,
                            bounds: right_clip,
                            default_color: Color::rgb(0xd8, 0xde, 0xe9),
                            custom_glyphs: &[],
                        });
                    }
                    continue;
                }

                text_areas.push(TextArea {
                    buffer: &span.buffer,
                    left,
                    top,
                    scale: 1.0,
                    bounds: pane_bounds,
                    default_color: Color::rgb(0xd8, 0xde, 0xe9),
                    custom_glyphs: &[],
                });
            }
        }
    }

    // IME変換中テキスト（Preedit）
    if !overlays.ime_preedit.is_empty()
        && let Some((cx, cy, _, _)) = cursor_area
    {
        let main_bounds = safe_bounds(
            layout.main_inner_rect.x,
            layout.main_inner_rect.y,
            layout.main_inner_rect.right(),
            layout.main_inner_rect.bottom(),
        );
        for (col_off, is_wide, buf) in ime_preedit_spans {
            let left = cx + (*col_off as f32) * cell_w + if *is_wide { wide_pad_x } else { 0.0 };
            text_areas.push(TextArea {
                buffer: buf,
                left,
                top: cy,
                scale: 1.0,
                bounds: main_bounds,
                default_color: Color::rgb(0xff, 0xff, 0xff),
                custom_glyphs: &[],
            });
        }
    }
}
