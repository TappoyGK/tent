use std::time::Instant;

use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, TextArea, Weight};

use crate::layout::{AppLayout, Rect};
use crate::slot::{NUM_SLOTS, Pane, Slot, SlotViewMode, Workspace};
use crate::terminal_pane::PaneId;

use super::primitives::{ImageInstance, RectInstance, safe_bounds};
use super::{CardHeaderCacheKey, RenderOverlays, Renderer};

impl Renderer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_sidebar(
        &mut self,
        workspace: &Workspace,
        active_idx: usize,
        layout: &AppLayout,
        scale_factor: f32,
        now: Instant,
        overlays: &RenderOverlays<'_>,
        rects: &mut Vec<RectInstance>,
        image_draws: &mut Vec<(PaneId, ImageInstance)>,
    ) -> f32 {
        // 右サイドバーの列トグルボタン＋10スロット描画
        let card_font_size = (11.0 * scale_factor).round().max(9.0);
        let card_metrics = Metrics::new(card_font_size, (card_font_size * 1.35).ceil());
        let time_w = 42.0 * scale_factor;

        for col in 0..2 {
            let collapsed = layout.sidebar_state.is_col_collapsed(col);
            let btn = layout.col_toggle_rects[col];
            self.push_rect(rects, btn, [0.12, 0.14, 0.18, 1.0]);
            self.push_border(rects, btn, 1.0 * scale_factor, [0.22, 0.25, 0.32, 1.0]);

            let label = match (col, collapsed) {
                (0, false) => "1-5  >",
                (_, false) => "6-0  >",
                (_, true) => "<",
            };
            if self.col_toggle_labels[col] != label {
                let tb = &mut self.col_toggle_buffers[col];
                tb.set_metrics(card_metrics);
                tb.set_size(Some(btn.w), Some(btn.h));
                tb.set_text(
                    label,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(Color::rgb(0x9a, 0xa6, 0xbd)),
                    Shaping::Basic,
                    None,
                );
                tb.shape_until_scroll(&mut self.font_system, false);
                self.col_toggle_labels[col] = label.to_string();
            }
        }

        for i in 0..NUM_SLOTS {
            let col = i / 5;
            let is_collapsed = layout.sidebar_state.is_col_collapsed(col);
            let slot = &workspace.slots[i];
            let card = layout.slot_rects[i];
            let header = layout.slot_header_rects[i];
            let preview = layout.slot_preview_rects[i];

            let is_active = i == active_idx;
            let is_dormant = slot.is_dormant();
            let is_outputting = slot.is_actively_outputting(now);
            let is_waiting = slot.is_waiting_input();
            let has_notif = slot.has_notification();
            let is_drag_hover =
                overlays.drag_hover_slot == Some(i) && overlays.drag_source_slot != Some(i);
            let is_drag_source = overlays.drag_source_slot == Some(i);
            let num = Slot::number_label(i);

            let is_browser_slot = slot.is_browser_focused();
            let is_playing_audio = slot.is_playing_audio();
            let left_str = if is_dormant || is_collapsed {
                num.to_string()
            } else {
                format!("{}  {}", num, slot.folder_name())
            };
            let time_str = if is_dormant || is_collapsed || is_browser_slot {
                String::new()
            } else if is_outputting {
                " ... ".to_string()
            } else {
                slot.latest_output_time_str()
            };

            let card_bg = if is_drag_source {
                [0.14, 0.16, 0.22, 0.75]
            } else if is_dormant {
                [0.08, 0.09, 0.11, 1.0]
            } else if is_active {
                [0.12, 0.15, 0.21, 1.0]
            } else if is_waiting {
                [0.16, 0.14, 0.09, 1.0]
            } else {
                [0.10, 0.11, 0.14, 1.0]
            };
            self.push_rect(rects, card, card_bg);

            let (border_color, border_w) = if is_drag_hover {
                ([0.96, 0.84, 0.30, 1.0], 2.5 * scale_factor)
            } else if has_notif {
                ([0.95, 0.42, 0.23, 1.0], 2.0 * scale_factor)
            } else if is_waiting {
                ([0.94, 0.77, 0.22, 1.0], 2.0 * scale_factor)
            } else if is_active {
                ([0.36, 0.68, 0.96, 1.0], 2.0 * scale_factor)
            } else if is_outputting {
                ([0.30, 0.80, 0.50, 1.0], 1.5 * scale_factor)
            } else if is_dormant {
                ([0.14, 0.15, 0.19, 1.0], 1.0 * scale_factor)
            } else {
                ([0.22, 0.25, 0.31, 1.0], 1.0 * scale_factor)
            };

            if is_collapsed {
                self.push_border(rects, card, border_w, border_color);
                let led_color = if has_notif {
                    [0.95, 0.42, 0.23, 1.0]
                } else if is_waiting {
                    [0.94, 0.77, 0.22, 1.0]
                } else if is_outputting {
                    [0.25, 0.90, 0.50, 1.0]
                } else if is_dormant {
                    [0.13, 0.14, 0.18, 1.0]
                } else if is_active {
                    [0.38, 0.41, 0.48, 1.0]
                } else {
                    [0.22, 0.24, 0.29, 1.0]
                };

                let body_bottom = if is_playing_audio {
                    card.bottom() - 22.0 * scale_factor
                } else {
                    card.bottom() - 4.0 * scale_factor
                };
                let body_top = card.y + 22.0 * scale_factor;
                let body_h = (body_bottom - body_top).max(12.0);

                if slot.view_mode == SlotViewMode::Browser {
                    // ブラウザモードはミニブラウザウィンドウアイコン（上部バー付き枠）で一目で判別可能にする
                    let icon_w = (16.0 * scale_factor).min((card.w - 6.0 * scale_factor).max(8.0));
                    let icon_h = (14.0 * scale_factor).min((body_h - 4.0 * scale_factor).max(8.0));
                    let icon_x = card.x + (card.w - icon_w) * 0.5;
                    let icon_y = body_top + ((body_h - icon_h) * 0.5).max(0.0);
                    let icon_rect = Rect::new(icon_x, icon_y, icon_w, icon_h);
                    let accent = if has_notif || is_waiting || is_outputting {
                        led_color
                    } else if is_active {
                        [0.34, 0.84, 0.92, 1.0]
                    } else {
                        [0.26, 0.62, 0.72, 1.0]
                    };
                    self.push_rect(rects, icon_rect, [0.10, 0.14, 0.19, 1.0]);
                    // ブラウザ上部タイトルバー帯
                    self.push_rect(
                        rects,
                        Rect::new(icon_x, icon_y, icon_w, (3.5 * scale_factor).max(2.0)),
                        accent,
                    );
                    self.push_border(rects, icon_rect, (1.2 * scale_factor).max(1.0), accent);
                } else {
                    let led_w = (8.0 * scale_factor).min((card.w - 6.0 * scale_factor).max(3.0));
                    let led_h = (28.0 * scale_factor).min((body_h - 4.0 * scale_factor).max(6.0));
                    let led_x = card.x + (card.w - led_w) * 0.5;
                    let led_y = body_top + ((body_h - led_h) * 0.5).max(0.0);
                    self.push_rect(rects, Rect::new(led_x, led_y, led_w, led_h), led_color);
                }

                // 折りたたみ時も音声再生中ならカード下部中央にスピーカーマークを描画
                if is_playing_audio {
                    let icon_scale = (scale_factor * 0.85).max(0.75);
                    let icon_w = 13.0 * icon_scale.max(1.0);
                    let icon_x = card.x + (card.w - icon_w) * 0.5;
                    let icon_cy = card.bottom() - 11.0 * scale_factor;
                    self.push_speaker_icon_at(rects, icon_x, icon_cy, icon_scale);
                }
            } else {
                let header_bg = if is_dormant {
                    [0.09, 0.10, 0.13, 1.0]
                } else if is_waiting {
                    [0.24, 0.20, 0.10, 1.0]
                } else if is_active {
                    [0.16, 0.22, 0.32, 1.0]
                } else {
                    [0.12, 0.14, 0.18, 1.0]
                };
                self.push_rect(rects, header, header_bg);
                self.push_border(rects, card, border_w, border_color);

                // 音声再生中のときヘッダ右端にスピーカーマークを描画
                if is_playing_audio {
                    self.push_speaker_icon(rects, header, scale_factor);
                }

                if slot.view_mode == SlotViewMode::Browser {
                    // スロットのブラウザモード（単体または分割ブラウザ）のミニプレビュー
                    let mini_gap = 2.0 * scale_factor;
                    let mini_rects = slot.compute_pane_rects(preview, mini_gap);
                    self.push_rect(rects, preview, [0.18, 0.21, 0.27, 1.0]);
                    for (bid, mrect) in mini_rects {
                        if self.browser_textures.contains_key(&bid) {
                            image_draws.push((
                                bid,
                                ImageInstance {
                                    pos: [mrect.x, mrect.y],
                                    size: [mrect.w, mrect.h],
                                },
                            ));
                        } else {
                            self.push_rect(rects, mrect, [0.08, 0.11, 0.15, 1.0]);
                            let chrome_h = (8.0 * scale_factor).min(mrect.h * 0.25);
                            self.push_rect(
                                rects,
                                Rect::new(mrect.x, mrect.y, mrect.w, chrome_h),
                                [0.14, 0.22, 0.28, 1.0],
                            );
                        }
                        self.push_border(
                            rects,
                            mrect,
                            1.0 * scale_factor,
                            [0.26, 0.56, 0.66, 0.85],
                        );
                    }
                } else if let Some(root) = &slot.root {
                    let mini_gap = 2.0 * scale_factor;
                    let mini_rects = slot.compute_pane_rects(preview, mini_gap);
                    self.push_rect(rects, preview, [0.18, 0.21, 0.27, 1.0]);

                    for (pid, mrect) in mini_rects {
                        let Some(pane_item) = root.pane(pid) else {
                            continue;
                        };
                        let Pane::Terminal(pane) = pane_item else {
                            // 分割内のブラウザペインプレビュー
                            if self.browser_textures.contains_key(&pid) {
                                image_draws.push((
                                    pid,
                                    ImageInstance {
                                        pos: [mrect.x, mrect.y],
                                        size: [mrect.w, mrect.h],
                                    },
                                ));
                            } else {
                                self.push_rect(rects, mrect, [0.08, 0.12, 0.16, 1.0]);
                                let chrome_h = (6.0 * scale_factor).min(mrect.h * 0.25);
                                self.push_rect(
                                    rects,
                                    Rect::new(mrect.x, mrect.y, mrect.w, chrome_h),
                                    [0.16, 0.28, 0.36, 1.0],
                                );
                            }
                            self.push_border(
                                rects,
                                mrect,
                                1.0 * scale_factor,
                                [0.28, 0.62, 0.72, 0.85],
                            );
                            continue;
                        };
                        let mm = &pane.minimap;
                        self.push_rect(rects, mrect, mm.default_bg);

                        let cols_f = (mm.cols as f32).max(1.0);
                        let rows_f = (mm.rows as f32).max(1.0);
                        let cw = mrect.w / cols_f;
                        let ch = mrect.h / rows_f;

                        // 横結合済みの MinimapRun をまとめて矩形描画
                        for run in &mm.runs {
                            let cx = mrect.x + (run.col_start as f32) * cw;
                            let cy = mrect.y + (run.row as f32) * ch;
                            let rw = ((run.col_len as f32) * cw).max(1.0);
                            if run.is_bg {
                                self.push_rect(
                                    rects,
                                    Rect::new(cx, cy, rw, ch.max(1.0)),
                                    run.color,
                                );
                            } else {
                                let h = (ch * 0.68).max(1.0);
                                let y_off = (ch - h) * 0.5;
                                self.push_rect(rects, Rect::new(cx, cy + y_off, rw, h), run.color);
                            }
                        }

                        if let Some((cur_x, cur_y)) = mm.cursor {
                            let cx = mrect.x + (cur_x as f32) * cw;
                            let cy = mrect.y + (cur_y as f32) * ch;
                            self.push_rect(
                                rects,
                                Rect::new(cx, cy, cw.max(2.0), ch.max(2.0)),
                                [0.53, 0.75, 0.82, 0.95],
                            );
                        }
                    }
                } else {
                    self.push_rect(rects, preview, [0.065, 0.072, 0.09, 1.0]);
                }
            }

            let cache_key = CardHeaderCacheKey {
                left_str,
                right_str: time_str,
                is_active,
                is_outputting,
                is_waiting,
                is_dormant,
                is_collapsed,
                is_playing_audio,
                width_px: header.w.round() as u32,
            };

            if self.card_header_keys[i] != cache_key {
                let left_color = if is_dormant {
                    Color::rgb(0x4c, 0x54, 0x66)
                } else if is_waiting {
                    Color::rgb(0xf6, 0xd8, 0x60)
                } else if is_active {
                    Color::rgb(0xef, 0xf4, 0xff)
                } else {
                    Color::rgb(0xc8, 0xd0, 0xe0)
                };
                let right_color = if is_browser_slot && is_playing_audio {
                    Color::rgb(0x4e, 0xdc, 0xe6)
                } else if is_outputting {
                    Color::rgb(0x66, 0xdd, 0x99)
                } else if is_waiting {
                    Color::rgb(0xf6, 0xd8, 0x60)
                } else {
                    Color::rgb(0x88, 0x92, 0xa8)
                };
                let left_w = if is_collapsed {
                    header.w
                } else {
                    (header.w - time_w - 10.0 * scale_factor).max(10.0)
                };

                let lb = &mut self.card_left_buffers[i];
                lb.set_metrics(card_metrics);
                lb.set_size(Some(left_w), Some(header.h));
                let shaping = if cache_key.left_str.is_ascii() {
                    Shaping::Basic
                } else {
                    Shaping::Advanced
                };
                lb.set_text(
                    &cache_key.left_str,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(left_color)
                        .weight(if is_active {
                            Weight::BOLD
                        } else {
                            Weight::NORMAL
                        }),
                    shaping,
                    None,
                );
                lb.shape_until_scroll(&mut self.font_system, false);

                let rb = &mut self.card_right_buffers[i];
                rb.set_metrics(card_metrics);
                rb.set_size(Some(time_w), Some(header.h));
                let right_shaping = if cache_key.right_str.is_ascii() {
                    Shaping::Basic
                } else {
                    Shaping::Advanced
                };
                rb.set_text(
                    &cache_key.right_str,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(right_color),
                    right_shaping,
                    None,
                );
                rb.shape_until_scroll(&mut self.font_system, false);

                self.card_header_keys[i] = cache_key;
            }
        }
        time_w
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_sidebar_text_areas<'a>(
    col_toggle_buffers: &'a [Buffer; 2],
    card_left_buffers: &'a [Buffer],
    card_right_buffers: &'a [Buffer],
    card_header_keys: &[CardHeaderCacheKey],
    layout: &AppLayout,
    scale_factor: f32,
    time_w: f32,
    text_areas: &mut Vec<TextArea<'a>>,
) {
    let pad_x = 5.0 * scale_factor;
    let pad_y = 3.0 * scale_factor;

    for (col, toggle_buf) in col_toggle_buffers.iter().enumerate() {
        let btn = layout.col_toggle_rects[col];
        let collapsed = layout.sidebar_state.is_col_collapsed(col);
        let left = if collapsed {
            btn.x + (btn.w - 8.0 * scale_factor) * 0.5
        } else {
            btn.x + pad_x
        };
        text_areas.push(TextArea {
            buffer: toggle_buf,
            left,
            top: btn.y + 2.0 * scale_factor,
            scale: 1.0,
            bounds: safe_bounds(btn.x, btn.y, btn.right(), btn.bottom()),
            default_color: Color::rgb(0x9a, 0xa6, 0xbd),
            custom_glyphs: &[],
        });
    }

    for i in 0..NUM_SLOTS {
        let col = i / 5;
        let is_collapsed = layout.sidebar_state.is_col_collapsed(col);
        let header = layout.slot_header_rects[i];
        let left = if is_collapsed {
            header.x + (header.w - 7.0 * scale_factor) * 0.5
        } else {
            header.x + pad_x
        };
        let right_bound = if is_collapsed {
            header.right()
        } else {
            header.right() - time_w - pad_x
        };
        text_areas.push(TextArea {
            buffer: &card_left_buffers[i],
            left,
            top: header.y + pad_y,
            scale: 1.0,
            bounds: safe_bounds(header.x, header.y, right_bound, header.bottom()),
            default_color: Color::rgb(0xc8, 0xd0, 0xe0),
            custom_glyphs: &[],
        });
        if !is_collapsed && !card_header_keys[i].right_str.is_empty() {
            text_areas.push(TextArea {
                buffer: &card_right_buffers[i],
                left: header.right() - time_w - pad_x * 0.5,
                top: header.y + pad_y,
                scale: 1.0,
                bounds: safe_bounds(
                    header.right() - time_w - pad_x,
                    header.y,
                    header.right(),
                    header.bottom(),
                ),
                default_color: Color::rgb(0x88, 0x92, 0xa8),
                custom_glyphs: &[],
            });
        }
    }
}
