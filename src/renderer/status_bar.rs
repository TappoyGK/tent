use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, TextArea, Weight};

use crate::layout::{AppLayout, Rect, StatusBarControls};
use crate::slot::{Slot, SlotViewMode, Workspace};

use super::primitives::{RectInstance, safe_bounds, shorten_home_path};
use super::{RenderOverlays, Renderer};

impl Renderer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_status_bar(
        &mut self,
        workspace: &Workspace,
        active_idx: usize,
        layout: &AppLayout,
        scale_factor: f32,
        overlays: &RenderOverlays<'_>,
        rects: &mut Vec<RectInstance>,
    ) -> (StatusBarControls, bool) {
        // メインステータスバーのテキスト・ボタン・URLバー更新
        let active_slot_ref = &workspace.slots[active_idx];
        let active_num = Slot::number_label(active_idx);
        let active_browser = active_slot_ref.focused_browser();
        let is_browser_bar = active_browser.is_some();
        let is_slot_browser_mode = active_slot_ref.view_mode == SlotViewMode::Browser;
        let sb_ctrl = layout.status_bar_controls(
            scale_factor,
            is_browser_bar,
            overlays.daemon_version_mismatch,
        );

        let btn_font_size = (10.5 * scale_factor).round().max(9.0);
        let btn_metrics = Metrics::new(btn_font_size, (btn_font_size * 1.3).ceil());

        // 右端のバッテリー残量＋現在日時（YYYY-MM-DD HH:MM:SS）を更新
        let date_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let (batt_part, batt_color) = match overlays.battery {
            Some(b) => {
                let txt = if b.charging {
                    format!("+{}%  ", b.percent)
                } else {
                    format!("{}%  ", b.percent)
                };
                let col = if b.charging {
                    Color::rgb(0x5c, 0xd6, 0x8d)
                } else if b.percent <= 20 {
                    Color::rgb(0xf0, 0x60, 0x50)
                } else {
                    Color::rgb(0x9e, 0xaa, 0xc2)
                };
                (txt, col)
            }
            None => (String::new(), Color::rgb(0x9e, 0xaa, 0xc2)),
        };
        let clock_key = format!("{batt_part}{date_str}");
        if self.clock_text != clock_key {
            let clock_font_size = (11.0 * scale_factor).round().max(9.0);
            let clock_metrics = Metrics::new(clock_font_size, (clock_font_size * 1.35).ceil());
            self.clock_buffer.set_metrics(clock_metrics);
            self.clock_buffer
                .set_size(Some(sb_ctrl.clock_rect.w), Some(sb_ctrl.clock_rect.h));
            let base_attrs = Attrs::new()
                .family(Family::Name("Menlo"))
                .color(Color::rgb(0x9e, 0xaa, 0xc2));
            let spans = [
                (
                    batt_part.as_str(),
                    Attrs::new().family(Family::Name("Menlo")).color(batt_color),
                ),
                (
                    date_str.as_str(),
                    Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(Color::rgb(0x9e, 0xaa, 0xc2)),
                ),
            ];
            self.clock_buffer
                .set_rich_text(spans, &base_attrs, Shaping::Basic, None);
            self.clock_buffer
                .shape_until_scroll(&mut self.font_system, false);
            self.clock_text = clock_key;
        }

        // アプリバージョン表記（リリース版は v0.2.0、開発版は v0.2.0-dev@2adc49c 等）を更新
        let ver_key =
            ((scale_factor * 100.0).round() as u32) * 2 + u32::from(overlays.version_copied);
        if self.version_cache_key != ver_key {
            let ver_font_size = (10.5 * scale_factor).round().max(9.0);
            let ver_metrics = Metrics::new(ver_font_size, (ver_font_size * 1.35).ceil());
            self.version_buffer.set_metrics(ver_metrics);
            self.version_buffer
                .set_size(Some(sb_ctrl.version_rect.w), Some(sb_ctrl.version_rect.h));
            let (ver_text, ver_color) = if overlays.version_copied {
                ("Copied!", Color::rgb(0x5c, 0xd6, 0x8d))
            } else {
                (env!("TENT_DISPLAY_VERSION"), Color::rgb(0x6e, 0x7a, 0x92))
            };
            self.version_buffer.set_text(
                ver_text,
                &Attrs::new().family(Family::Name("Menlo")).color(ver_color),
                Shaping::Basic,
                None,
            );
            self.version_buffer
                .shape_until_scroll(&mut self.font_system, false);
            self.version_cache_key = ver_key;
        }

        // Wi-Fi 電波バーをバッテリー＋日時のすぐ左隣に描画
        if let Some(wifi) = overlays.wifi {
            let clock_char_w = (11.0 * scale_factor).round().max(9.0) * 0.61;
            let clock_text_w = (self.clock_text.len() as f32) * clock_char_w;
            let text_left = (sb_ctrl.clock_rect.right() - clock_text_w - 2.0 * scale_factor)
                .max(sb_ctrl.clock_rect.x + 20.0 * scale_factor);
            self.push_wifi_icon(
                rects,
                text_left - 20.0 * scale_factor,
                sb_ctrl.clock_rect.y,
                sb_ctrl.clock_rect.h,
                scale_factor,
                wifi,
            );
        }

        // 横長タイマー描画（右端=0分, 左端=60分。5/15/25/45分にスナップ目盛り）
        if sb_ctrl.timer_icon_rect.w > 1.0 {
            self.push_hourglass_icon(
                rects,
                sb_ctrl.timer_icon_rect,
                scale_factor,
                overlays.timer.progress,
                overlays.timer.is_running,
                overlays.timer.is_finished,
                overlays.timer.anim_tick,
            );
        }
        if sb_ctrl.timer_rect.w > 1.0 {
            let tr = sb_ctrl.timer_rect;
            let track_bg = if overlays.timer.is_finished {
                [0.36, 0.13, 0.12, 1.0]
            } else {
                [0.08, 0.10, 0.14, 1.0]
            };
            let track_border = if overlays.timer.is_finished {
                [0.95, 0.38, 0.28, 1.0]
            } else if overlays.timer.is_dragging {
                [0.40, 0.78, 0.96, 1.0]
            } else {
                [0.22, 0.26, 0.34, 1.0]
            };
            self.push_rect(rects, tr, track_bg);

            // 5 / 15 / 25 / 45 分の目盛り線（右端が 0 分、左端が 60 分）
            let tick_w = (1.0 * scale_factor).max(1.0);
            for snap_min in [5.0f32, 15.0, 25.0, 45.0] {
                let ratio = snap_min / 60.0;
                let tx = tr.right() - tr.w * ratio;
                self.push_rect(
                    rects,
                    Rect::new(
                        tx - tick_w * 0.5,
                        tr.y + 3.0 * scale_factor,
                        tick_w,
                        tr.h - 6.0 * scale_factor,
                    ),
                    [0.25, 0.30, 0.40, 0.85],
                );
            }

            // 右端から左へ伸びる残り時間バー
            let frac = overlays.timer.fraction.clamp(0.0, 1.0);
            if frac > 0.0 {
                let fill_w = (tr.w * frac).min(tr.w);
                let fill_x = tr.right() - fill_w;
                let fill_color = if overlays.timer.is_dragging {
                    [0.26, 0.62, 0.88, 0.85]
                } else if frac <= 5.0 / 60.0 {
                    [0.86, 0.52, 0.20, 0.85]
                } else {
                    [0.20, 0.58, 0.54, 0.85]
                };
                self.push_rect(rects, Rect::new(fill_x, tr.y, fill_w, tr.h), fill_color);
                // 左端のつまみライン
                let knob_w = (2.5 * scale_factor).max(2.0);
                self.push_rect(
                    rects,
                    Rect::new(fill_x, tr.y, knob_w, tr.h),
                    [0.75, 0.92, 1.0, 0.95],
                );
            } else {
                // 停止中は右端に引っ張りハンドルを表示
                let knob_w = (3.0 * scale_factor).max(2.0);
                self.push_rect(
                    rects,
                    Rect::new(tr.right() - knob_w, tr.y, knob_w, tr.h),
                    [0.36, 0.46, 0.62, 0.95],
                );
            }

            self.push_border(rects, tr, 1.0 * scale_factor, track_border);

            let tkey = format!(
                "{}|{}|{}",
                overlays.timer.label,
                overlays.timer.is_finished,
                (scale_factor * 100.0).round() as u32
            );
            if self.timer_cache_key != tkey {
                let tfs = (10.5 * scale_factor).round().max(9.0);
                let tmetrics = Metrics::new(tfs, (tfs * 1.3).ceil());
                self.timer_buffer.set_metrics(tmetrics);
                self.timer_buffer.set_size(Some(tr.w), Some(tr.h));
                let tcolor = if overlays.timer.is_finished {
                    Color::rgb(0xff, 0xd4, 0xc4)
                } else {
                    Color::rgb(0xf0, 0xf6, 0xff)
                };
                self.timer_buffer.set_text(
                    &overlays.timer.label,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(tcolor)
                        .weight(Weight::BOLD),
                    Shaping::Basic,
                    None,
                );
                self.timer_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                self.timer_cache_key = tkey;
            }
        }

        // メモリ使用量モニターバー描画（80% = 1.0GiB の目安ライン、超過時は赤で警告）
        if sb_ctrl.mem_bar_rect.w > 1.0 && !overlays.memory.label.is_empty() {
            let mr = sb_ctrl.mem_bar_rect;
            let over = overlays.memory.is_over_budget;
            let track_bg = if over {
                [0.26, 0.09, 0.09, 1.0]
            } else {
                [0.08, 0.10, 0.14, 1.0]
            };
            let track_border = if over {
                [0.96, 0.36, 0.32, 1.0]
            } else {
                [0.22, 0.26, 0.34, 1.0]
            };
            self.push_rect(rects, mr, track_bg);

            let frac = overlays.memory.fraction.clamp(0.0, 1.0);
            if frac > 0.0 {
                let fill_w = (mr.w * frac).min(mr.w);
                let fill_color = if over {
                    [0.90, 0.26, 0.24, 0.92]
                } else if frac >= 0.60 {
                    [0.84, 0.60, 0.20, 0.85]
                } else {
                    [0.22, 0.64, 0.52, 0.80]
                };
                self.push_rect(rects, Rect::new(mr.x, mr.y, fill_w, mr.h), fill_color);
            }

            // 80%（1.0 GiB）しきい値ラインの縦目盛り
            let tick_w = (1.5 * scale_factor).max(1.0);
            let tick_x = mr.x + mr.w * 0.80;
            let tick_color = if over {
                [1.0, 0.82, 0.78, 0.95]
            } else {
                [0.52, 0.58, 0.70, 0.90]
            };
            self.push_rect(
                rects,
                Rect::new(
                    tick_x - tick_w * 0.5,
                    mr.y + 1.5 * scale_factor,
                    tick_w,
                    (mr.h - 3.0 * scale_factor).max(2.0),
                ),
                tick_color,
            );
            self.push_border(rects, mr, 1.0 * scale_factor, track_border);

            let scale_key = (scale_factor * 100.0).round() as u32;
            if self.mem_cache_key.0 != overlays.memory.label
                || self.mem_cache_key.1 != over
                || self.mem_cache_key.2 != scale_key
            {
                let mfs = (10.0 * scale_factor).round().max(9.0);
                let mmetrics = Metrics::new(mfs, (mfs * 1.3).ceil());
                self.mem_buffer.set_metrics(mmetrics);
                self.mem_buffer.set_size(Some(mr.w), Some(mr.h));
                let mcolor = if over {
                    Color::rgb(0xff, 0xe2, 0xdc)
                } else {
                    Color::rgb(0xe4, 0xec, 0xf8)
                };
                self.mem_buffer.set_text(
                    &overlays.memory.label,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(mcolor)
                        .weight(Weight::BOLD),
                    Shaping::Basic,
                    None,
                );
                self.mem_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                self.mem_cache_key = (overlays.memory.label.clone(), over, scale_key);
            }
        }

        // 右側トグルボタン（[Upgrade], [Web]/[Term], [+Web], [?]）とブラウザ操作ボタン描画
        let toggle_label = if is_slot_browser_mode { "Term" } else { "Web" };
        let upgrade_label = if overlays.daemon_upgrade_confirm {
            "Confirm?"
        } else {
            "Upgrade"
        };
        let btn_specs: [(usize, Rect, &str, bool); 8] = [
            (0, sb_ctrl.back_rect, "<", is_browser_bar),
            (1, sb_ctrl.forward_rect, ">", is_browser_bar),
            (2, sb_ctrl.reload_rect, "R", is_browser_bar),
            (3, sb_ctrl.ext_rect, "Ext", is_browser_bar),
            (4, sb_ctrl.toggle_mode_rect, toggle_label, true),
            (5, sb_ctrl.split_web_rect, "+Web", true),
            (6, sb_ctrl.help_rect, "?", true),
            (
                7,
                sb_ctrl.upgrade_daemon_rect,
                upgrade_label,
                overlays.daemon_version_mismatch,
            ),
        ];
        for (idx, rect, label, visible) in btn_specs {
            if !visible || rect.w <= 1.0 {
                continue;
            }
            let (bg, border, text_color) = if idx == 7 {
                if overlays.daemon_upgrade_confirm {
                    (
                        [0.44, 0.13, 0.15, 1.0],
                        [0.95, 0.36, 0.36, 1.0],
                        Color::rgb(0xff, 0xf2, 0xf2),
                    )
                } else {
                    (
                        [0.32, 0.21, 0.08, 1.0],
                        [0.92, 0.60, 0.24, 1.0],
                        Color::rgb(0xff, 0xd4, 0x94),
                    )
                }
            } else if idx == 4 && is_slot_browser_mode {
                (
                    [0.16, 0.30, 0.38, 1.0],
                    [0.34, 0.72, 0.86, 1.0],
                    Color::rgb(0xc8, 0xd4, 0xe8),
                )
            } else {
                (
                    [0.14, 0.17, 0.22, 1.0],
                    [0.24, 0.28, 0.36, 1.0],
                    Color::rgb(0xc8, 0xd4, 0xe8),
                )
            };
            self.push_rect(rects, rect, bg);
            self.push_border(rects, rect, 1.0 * scale_factor, border);

            let key = format!(
                "{label}|{}|{}",
                rect.w.round() as u32,
                idx == 7 && overlays.daemon_upgrade_confirm
            );
            if self.status_btn_labels[idx] != key {
                let buf = &mut self.status_btn_buffers[idx];
                buf.set_metrics(btn_metrics);
                buf.set_size(Some(rect.w), Some(rect.h));
                let mut attrs = Attrs::new().family(Family::Name("Menlo")).color(text_color);
                if idx == 7 {
                    attrs = attrs.weight(Weight::BOLD);
                }
                buf.set_text(label, &attrs, Shaping::Basic, None);
                buf.shape_until_scroll(&mut self.font_system, false);
                self.status_btn_labels[idx] = key;
            }
        }

        if is_browser_bar {
            // 左側の [番号] バッジのみ main_status_buffer で描画
            let status_key = (
                format!("B|{active_num}"),
                layout.main_status_rect.w.round() as u32,
            );
            if self.main_status_cache_key != status_key {
                let status_font_size = (12.0 * scale_factor).round().max(10.0);
                let status_metrics =
                    Metrics::new(status_font_size, (status_font_size * 1.35).ceil());
                self.main_status_buffer.set_metrics(status_metrics);
                self.main_status_buffer
                    .set_size(Some(40.0 * scale_factor), Some(layout.main_status_rect.h));
                let badge = format!("[{active_num}]");
                self.main_status_buffer.set_text(
                    &badge,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(Color::rgb(0x46, 0xc8, 0xd8))
                        .weight(Weight::BOLD),
                    Shaping::Basic,
                    None,
                );
                self.main_status_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                self.main_status_cache_key = status_key;
            }

            // URL バー背景・枠線・全選択ハイライト・テキスト
            let is_editing = overlays.url_bar_editing.is_some();
            let is_select_all = overlays.url_bar_editing.is_some_and(|(_, sa)| sa);
            let url_bg = if is_editing {
                [0.07, 0.09, 0.13, 1.0]
            } else {
                [0.12, 0.14, 0.19, 1.0]
            };
            let url_border = if is_editing {
                [0.36, 0.72, 0.98, 1.0]
            } else {
                [0.24, 0.28, 0.36, 1.0]
            };
            self.push_rect(rects, sb_ctrl.url_bar_rect, url_bg);
            self.push_border(
                rects,
                sb_ctrl.url_bar_rect,
                if is_editing {
                    1.5 * scale_factor
                } else {
                    1.0 * scale_factor
                },
                url_border,
            );

            let display_url = if let Some((edit_text, select_all)) = overlays.url_bar_editing {
                if select_all {
                    edit_text.to_string()
                } else {
                    format!("{edit_text}|")
                }
            } else {
                active_browser.map(|b| b.url.clone()).unwrap_or_default()
            };
            if is_select_all && !display_url.is_empty() {
                let char_w = (11.5 * scale_factor).round().max(10.0) * 0.61;
                let text_w = (display_url.chars().count() as f32 * char_w)
                    .min((sb_ctrl.url_bar_rect.w - 10.0 * scale_factor).max(0.0));
                let sel_rect = Rect::new(
                    sb_ctrl.url_bar_rect.x + 5.0 * scale_factor,
                    sb_ctrl.url_bar_rect.y + 2.5 * scale_factor,
                    text_w,
                    (sb_ctrl.url_bar_rect.h - 5.0 * scale_factor).max(2.0),
                );
                self.push_rect(rects, sel_rect, [0.22, 0.48, 0.82, 0.75]);
            }
            let url_key = (
                display_url.clone(),
                is_editing,
                sb_ctrl.url_bar_rect.w.round() as u32,
            );
            if self.url_bar_cache_key != url_key {
                let url_font_size = (11.5 * scale_factor).round().max(10.0);
                let url_metrics = Metrics::new(url_font_size, (url_font_size * 1.3).ceil());
                self.url_bar_buffer.set_metrics(url_metrics);
                self.url_bar_buffer.set_size(
                    Some((sb_ctrl.url_bar_rect.w - 10.0 * scale_factor).max(10.0)),
                    Some(sb_ctrl.url_bar_rect.h),
                );
                let shaping = if display_url.is_ascii() {
                    Shaping::Basic
                } else {
                    Shaping::Advanced
                };
                self.url_bar_buffer.set_text(
                    &display_url,
                    &Attrs::new()
                        .family(Family::Name("Menlo"))
                        .color(if is_editing {
                            Color::rgb(0xff, 0xff, 0xff)
                        } else {
                            Color::rgb(0xd4, 0xde, 0xee)
                        }),
                    shaping,
                    None,
                );
                self.url_bar_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                self.url_bar_cache_key = url_key;
            }
        } else {
            let active_path_display = shorten_home_path(active_slot_ref.primary_terminal_cwd());
            let avail_w =
                (sb_ctrl.text_right_limit - layout.main_status_rect.x - 12.0 * scale_factor)
                    .max(20.0);
            let status_key = (
                format!("T|{active_num}|{active_path_display}"),
                avail_w.round() as u32,
            );
            if self.main_status_cache_key != status_key {
                let status_font_size = (12.0 * scale_factor).round().max(10.0);
                let status_metrics =
                    Metrics::new(status_font_size, (status_font_size * 1.35).ceil());
                self.main_status_buffer.set_metrics(status_metrics);
                self.main_status_buffer
                    .set_size(Some(avail_w), Some(layout.main_status_rect.h));
                let badge = format!("[{active_num}] ");
                // パスを「親ディレクトリ/」と「末尾フォルダ名」に分けて末尾だけを強調表示
                let trimmed = active_path_display.trim_end_matches('/');
                let (prefix_part, leaf_part) = match trimmed.rfind('/') {
                    Some(pos) if !trimmed[pos + 1..].is_empty() => {
                        (&trimmed[..=pos], &trimmed[pos + 1..])
                    }
                    _ => ("", active_path_display.as_str()),
                };
                let base_attrs = Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(Color::rgb(0xe8, 0xee, 0xf8));
                let spans = [
                    (
                        badge.as_str(),
                        Attrs::new()
                            .family(Family::Name("Menlo"))
                            .color(Color::rgb(0x5c, 0xad, 0xf5))
                            .weight(Weight::BOLD),
                    ),
                    (
                        prefix_part,
                        Attrs::new()
                            .family(Family::Name("Menlo"))
                            .color(Color::rgb(0x7c, 0x86, 0x9b)),
                    ),
                    (
                        leaf_part,
                        Attrs::new()
                            .family(Family::Name("Menlo"))
                            .color(Color::rgb(0xf4, 0xf8, 0xff))
                            .weight(Weight::BOLD),
                    ),
                ];
                let shaping = if active_path_display.is_ascii() {
                    Shaping::Basic
                } else {
                    Shaping::Advanced
                };
                self.main_status_buffer
                    .set_rich_text(spans, &base_attrs, shaping, None);
                self.main_status_buffer
                    .shape_until_scroll(&mut self.font_system, false);
                self.main_status_cache_key = status_key;
            }
        }
        (sb_ctrl, is_browser_bar)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_status_bar_text_areas<'a>(
    main_status_buffer: &'a Buffer,
    clock_text: &str,
    clock_buffer: &'a Buffer,
    version_buffer: &'a Buffer,
    timer_buffer: &'a Buffer,
    timer_label: &str,
    mem_buffer: &'a Buffer,
    mem_label: &str,
    url_bar_buffer: &'a Buffer,
    status_btn_buffers: &'a [Buffer; 8],
    layout: &AppLayout,
    scale_factor: f32,
    sb_ctrl: &StatusBarControls,
    is_browser_bar: bool,
    show_daemon_upgrade: bool,
    text_areas: &mut Vec<TextArea<'a>>,
) {
    // メインパネル上部ステータスバー
    text_areas.push(TextArea {
        buffer: main_status_buffer,
        left: layout.main_status_rect.x + 10.0 * scale_factor,
        top: layout.main_status_rect.y + 5.0 * scale_factor,
        scale: 1.0,
        bounds: safe_bounds(
            layout.main_status_rect.x + 8.0 * scale_factor,
            layout.main_status_rect.y,
            sb_ctrl.text_right_limit,
            layout.main_status_rect.bottom(),
        ),
        default_color: Color::rgb(0xef, 0xf4, 0xff),
        custom_glyphs: &[],
    });

    let clock_char_w = (11.0 * scale_factor).round().max(9.0) * 0.61;
    let clock_text_w = (clock_text.len() as f32) * clock_char_w;
    let clock_left =
        (sb_ctrl.clock_rect.right() - clock_text_w - 2.0 * scale_factor).max(sb_ctrl.clock_rect.x);
    text_areas.push(TextArea {
        buffer: clock_buffer,
        left: clock_left,
        top: sb_ctrl.clock_rect.y + 3.0 * scale_factor,
        scale: 1.0,
        bounds: safe_bounds(
            sb_ctrl.clock_rect.x,
            sb_ctrl.clock_rect.y,
            sb_ctrl.clock_rect.right(),
            sb_ctrl.clock_rect.bottom(),
        ),
        default_color: Color::rgb(0x9e, 0xaa, 0xc2),
        custom_glyphs: &[],
    });

    text_areas.push(TextArea {
        buffer: version_buffer,
        left: sb_ctrl.version_rect.x + 2.0 * scale_factor,
        top: sb_ctrl.version_rect.y + 3.5 * scale_factor,
        scale: 1.0,
        bounds: safe_bounds(
            sb_ctrl.version_rect.x,
            sb_ctrl.version_rect.y,
            sb_ctrl.version_rect.right(),
            sb_ctrl.version_rect.bottom(),
        ),
        default_color: Color::rgb(0x6e, 0x7a, 0x92),
        custom_glyphs: &[],
    });

    if sb_ctrl.timer_rect.w > 1.0 && !timer_label.is_empty() {
        let tr = sb_ctrl.timer_rect;
        let char_w = (10.5 * scale_factor).round().max(9.0) * 0.62;
        let label_w = (timer_label.len() as f32) * char_w;
        let left = (tr.x + (tr.w - label_w) * 0.5).max(tr.x + 4.0 * scale_factor);
        text_areas.push(TextArea {
            buffer: timer_buffer,
            left,
            top: tr.y + 3.0 * scale_factor,
            scale: 1.0,
            bounds: safe_bounds(tr.x, tr.y, tr.right(), tr.bottom()),
            default_color: Color::rgb(0xf0, 0xf6, 0xff),
            custom_glyphs: &[],
        });
    }

    if sb_ctrl.mem_bar_rect.w > 1.0 && !mem_label.is_empty() {
        let mr = sb_ctrl.mem_bar_rect;
        let char_w = (10.0 * scale_factor).round().max(9.0) * 0.62;
        let label_w = (mem_label.len() as f32) * char_w;
        let left = (mr.x + (mr.w - label_w) * 0.5).max(mr.x + 3.0 * scale_factor);
        text_areas.push(TextArea {
            buffer: mem_buffer,
            left,
            top: mr.y + 3.0 * scale_factor,
            scale: 1.0,
            bounds: safe_bounds(mr.x, mr.y, mr.right(), mr.bottom()),
            default_color: Color::rgb(0xe4, 0xec, 0xf8),
            custom_glyphs: &[],
        });
    }

    if is_browser_bar {
        text_areas.push(TextArea {
            buffer: url_bar_buffer,
            left: sb_ctrl.url_bar_rect.x + 6.0 * scale_factor,
            top: sb_ctrl.url_bar_rect.y + 3.0 * scale_factor,
            scale: 1.0,
            bounds: safe_bounds(
                sb_ctrl.url_bar_rect.x + 4.0 * scale_factor,
                sb_ctrl.url_bar_rect.y,
                sb_ctrl.url_bar_rect.right() - 4.0 * scale_factor,
                sb_ctrl.url_bar_rect.bottom(),
            ),
            default_color: Color::rgb(0xd4, 0xde, 0xee),
            custom_glyphs: &[],
        });
    }

    let btn_text_specs: [(usize, Rect, bool); 8] = [
        (0, sb_ctrl.back_rect, is_browser_bar),
        (1, sb_ctrl.forward_rect, is_browser_bar),
        (2, sb_ctrl.reload_rect, is_browser_bar),
        (3, sb_ctrl.ext_rect, is_browser_bar),
        (4, sb_ctrl.toggle_mode_rect, true),
        (5, sb_ctrl.split_web_rect, true),
        (6, sb_ctrl.help_rect, true),
        (7, sb_ctrl.upgrade_daemon_rect, show_daemon_upgrade),
    ];
    for (idx, rect, visible) in btn_text_specs {
        if !visible || rect.w <= 1.0 {
            continue;
        }
        text_areas.push(TextArea {
            buffer: &status_btn_buffers[idx],
            left: rect.x + 6.0 * scale_factor,
            top: rect.y + 3.0 * scale_factor,
            scale: 1.0,
            bounds: safe_bounds(rect.x, rect.y, rect.right(), rect.bottom()),
            default_color: Color::rgb(0xc8, 0xd4, 0xe8),
            custom_glyphs: &[],
        });
    }
}
