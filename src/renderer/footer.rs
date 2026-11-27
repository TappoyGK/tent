use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, TextArea, Weight, Wrap};

use crate::layout::{AppLayout, Rect};
use crate::slot::{Slot, SlotViewMode};
use crate::terminal_pane::PaneId;

use super::Renderer;
use super::primitives::{RectInstance, safe_bounds};

/// フッター内の 1 ペイン分の時刻エントリ
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FooterEntry {
    pub label: String,
    pub is_latest: bool,
}

/// フッターの 1 列分のレイアウトと表示エントリ群
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FooterColumn {
    pub x: f32,
    pub w: f32,
    pub entries: Vec<FooterEntry>,
}

impl FooterColumn {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn has_latest(&self) -> bool {
        self.entries.iter().any(|e| e.is_latest)
    }
}

/// メイン領域の各ペイン矩形と最終更新時刻 `(timestamp_micros, "HH:MM")` から、
/// 下端フッターの列ごとの表示エントリを算出する。
/// ターミナルペインが 2 つ以上あり、最新の更新時刻が一意に定まる場合はそのペインを `is_latest = true` にする。
pub(super) fn compute_footer_columns(panes: &[(Rect, Option<(i64, String)>)]) -> Vec<FooterColumn> {
    if panes.is_empty() {
        return Vec::new();
    }
    let max_bottom = panes
        .iter()
        .map(|(r, _)| r.bottom())
        .fold(f32::NEG_INFINITY, f32::max);

    // ターミナルペインが 2 つ以上あり、最大タイムスタンプが一意なときだけ最新強調を有効化
    let mut valid_count = 0usize;
    let mut max_ts: Option<i64> = None;
    let mut max_ts_count = 0usize;
    for (_, ts_opt) in panes {
        if let Some((micros, label)) = ts_opt
            && !label.is_empty()
        {
            valid_count += 1;
            match max_ts {
                None => {
                    max_ts = Some(*micros);
                    max_ts_count = 1;
                }
                Some(cur) if *micros > cur => {
                    max_ts = Some(*micros);
                    max_ts_count = 1;
                }
                Some(cur) if *micros == cur => {
                    max_ts_count += 1;
                }
                _ => {}
            }
        }
    }
    let highlight_ts = if valid_count >= 2 && max_ts_count == 1 {
        max_ts
    } else {
        None
    };

    let mut bottom_cols: Vec<Rect> = panes
        .iter()
        .filter(|(r, _)| (max_bottom - r.bottom()).abs() <= 2.0)
        .map(|(r, _)| *r)
        .collect();
    bottom_cols.sort_by(|a, b| a.x.total_cmp(&b.x));

    bottom_cols
        .into_iter()
        .map(|col| {
            let mut stacked: Vec<(f32, f32, i64, &str)> = panes
                .iter()
                .filter_map(|(r, ts)| {
                    let (micros, label) = ts.as_ref()?;
                    if label.is_empty() {
                        return None;
                    }
                    let overlap = r.right().min(col.right()) - r.x.max(col.x);
                    (overlap > 1.0).then_some((r.y, r.x, *micros, label.as_str()))
                })
                .collect();
            stacked.sort_by(|a, b| {
                if (a.0 - b.0).abs() > 1.0 {
                    a.0.total_cmp(&b.0)
                } else {
                    a.1.total_cmp(&b.1)
                }
            });
            let entries = stacked
                .into_iter()
                .map(|(_, _, micros, label)| FooterEntry {
                    label: label.to_string(),
                    is_latest: highlight_ts == Some(micros),
                })
                .collect();
            FooterColumn {
                x: col.x,
                w: col.w,
                entries,
            }
        })
        .collect()
}

impl Renderer {
    pub(super) fn build_footer(
        &mut self,
        slot: &Slot,
        active_pane_rects: &[(PaneId, Rect)],
        layout: &AppLayout,
        scale_factor: f32,
        pane_gap: f32,
        rects: &mut Vec<RectInstance>,
    ) -> Vec<FooterColumn> {
        let footer = layout.main_footer_rect;
        if footer.h <= 1.0 {
            return Vec::new();
        }

        // フッター背景＋上端境界線
        self.push_rect(rects, footer, [0.10, 0.12, 0.16, 1.0]);
        self.push_rect(
            rects,
            Rect::new(footer.x, footer.y, footer.w, 1.0 * scale_factor),
            [0.20, 0.23, 0.29, 1.0],
        );

        let pane_timestamps: Vec<(Rect, Option<(i64, String)>)> = match slot.view_mode {
            SlotViewMode::Panes => {
                if let Some(root) = &slot.root {
                    active_pane_rects
                        .iter()
                        .map(|(pid, rect)| {
                            let ts = root.terminal_pane(*pid).map(|p| {
                                (
                                    p.last_output_at.timestamp_micros(),
                                    p.last_output_at.format("%H:%M").to_string(),
                                )
                            });
                            (*rect, ts)
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            SlotViewMode::Browser => Vec::new(),
        };

        let columns = compute_footer_columns(&pane_timestamps);

        // 最新更新ペインを含む列は背景と上端アクセントラインを強調表示する
        for col in &columns {
            if col.has_latest() {
                let x0 = col.x.max(footer.x);
                let x1 = (col.x + col.w).min(footer.right());
                if x1 > x0 {
                    let w = x1 - x0;
                    self.push_rect(
                        rects,
                        Rect::new(x0, footer.y, w, footer.h),
                        [0.16, 0.14, 0.10, 1.0],
                    );
                    self.push_rect(
                        rects,
                        Rect::new(x0, footer.y, w, (1.5 * scale_factor).max(1.5)),
                        [0.94, 0.77, 0.22, 0.90],
                    );
                }
            }
        }

        // 2 列以上ある場合は列境界にさりげない縦セパレータを描画
        for col in columns.iter().skip(1) {
            let sep_x = (col.x - pane_gap * 0.5).max(footer.x);
            self.push_rect(
                rects,
                Rect::new(
                    sep_x,
                    footer.y + 3.0 * scale_factor,
                    1.0 * scale_factor,
                    (footer.h - 6.0 * scale_factor).max(1.0),
                ),
                [0.20, 0.23, 0.29, 0.8],
            );
        }

        // テキストバッファを必要数確保し、差分更新
        let font_size = (10.5 * scale_factor).round().max(9.0);
        let metrics = Metrics::new(font_size, (font_size * 1.3).ceil());
        let scale_key = (scale_factor * 100.0).round() as u32;

        while self.footer_buffers.len() < columns.len() {
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_wrap(Wrap::None);
            self.footer_buffers.push(buf);
            self.footer_cache_keys.push((Vec::new(), 0, 0));
        }
        self.footer_buffers.truncate(columns.len());
        self.footer_cache_keys.truncate(columns.len());

        let normal_attrs = Attrs::new()
            .family(Family::Name("Menlo"))
            .color(Color::rgb(0x8a, 0x96, 0xae));
        let latest_attrs = Attrs::new()
            .family(Family::Name("Menlo"))
            .weight(Weight::BOLD)
            .color(Color::rgb(0xf8, 0xcc, 0x56));
        let sep_attrs = Attrs::new()
            .family(Family::Name("Menlo"))
            .color(Color::rgb(0x5c, 0x66, 0x7a));

        for (i, col) in columns.iter().enumerate() {
            let avail_w = (col.w - 8.0 * scale_factor).max(1.0);
            let entry_key: Vec<(String, bool)> = col
                .entries
                .iter()
                .map(|e| (e.label.clone(), e.is_latest))
                .collect();
            let key = (entry_key, avail_w.round() as u32, scale_key);
            if self.footer_cache_keys[i] != key {
                let buf = &mut self.footer_buffers[i];
                buf.set_metrics(metrics);
                buf.set_size(Some(avail_w), Some(footer.h));
                let mut spans: Vec<(&str, Attrs<'_>)> = Vec::with_capacity(col.entries.len() * 2);
                for (j, entry) in col.entries.iter().enumerate() {
                    if j > 0 {
                        spans.push((" / ", sep_attrs.clone()));
                    }
                    let attrs = if entry.is_latest {
                        latest_attrs.clone()
                    } else {
                        normal_attrs.clone()
                    };
                    spans.push((entry.label.as_str(), attrs));
                }
                buf.set_rich_text(spans, &normal_attrs, Shaping::Basic, None);
                buf.shape_until_scroll(&mut self.font_system, false);
                self.footer_cache_keys[i] = key;
            }
        }

        columns
    }
}

pub(super) fn push_footer_text_areas<'a>(
    footer_buffers: &'a [Buffer],
    columns: &[FooterColumn],
    layout: &AppLayout,
    scale_factor: f32,
    text_areas: &mut Vec<TextArea<'a>>,
) {
    let footer = layout.main_footer_rect;
    if footer.h <= 1.0 {
        return;
    }
    let font_size = (10.5 * scale_factor).round().max(9.0);
    let line_h = (font_size * 1.3).ceil();
    let top = footer.y + ((footer.h - line_h) * 0.5).max(1.0);
    let pad_x = 4.0 * scale_factor;

    for (buf, col) in footer_buffers.iter().zip(columns.iter()) {
        if col.is_empty() {
            continue;
        }
        let left = (col.x + pad_x).max(footer.x + pad_x);
        let right = col.x + col.w;
        text_areas.push(TextArea {
            buffer: buf,
            left,
            top,
            scale: 1.0,
            bounds: safe_bounds(left, footer.y, right.min(footer.right()), footer.bottom()),
            default_color: Color::rgb(0x8a, 0x96, 0xae),
            custom_glyphs: &[],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(micros: i64, label: &str) -> Option<(i64, String)> {
        Some((micros, label.to_string()))
    }

    fn entry(label: &str, is_latest: bool) -> FooterEntry {
        FooterEntry {
            label: label.to_string(),
            is_latest,
        }
    }

    #[test]
    fn test_compute_footer_columns_vertical_horizontal_and_nested() {
        // 1. 単純な上下（水平）分割: 1 列に上から 15:00 / 15:03（最新の 15:03 が強調）
        let stacked = vec![
            (Rect::new(0.0, 0.0, 400.0, 200.0), ts(100, "15:00")),
            (Rect::new(0.0, 204.0, 400.0, 200.0), ts(200, "15:03")),
        ];
        assert_eq!(
            compute_footer_columns(&stacked),
            vec![FooterColumn {
                x: 0.0,
                w: 400.0,
                entries: vec![entry("15:00", false), entry("15:03", true)],
            }]
        );

        // 2. 左右分割 + 左だけ上下分割: 左列 15:00 / 15:03, 右列 15:10（最新の右列のみ強調）
        let left_split = vec![
            (Rect::new(0.0, 0.0, 200.0, 200.0), ts(100, "15:00")),
            (Rect::new(0.0, 204.0, 200.0, 200.0), ts(200, "15:03")),
            (Rect::new(204.0, 0.0, 200.0, 404.0), ts(300, "15:10")),
        ];
        assert_eq!(
            compute_footer_columns(&left_split),
            vec![
                FooterColumn {
                    x: 0.0,
                    w: 200.0,
                    entries: vec![entry("15:00", false), entry("15:03", false)],
                },
                FooterColumn {
                    x: 204.0,
                    w: 200.0,
                    entries: vec![entry("15:10", true)],
                },
            ]
        );

        // 3. 同じ分表示（15:10）でもマイクロ秒差で後から更新されたペインが強調されること
        let same_minute = vec![
            (Rect::new(0.0, 0.0, 200.0, 404.0), ts(1_000_000, "15:10")),
            (Rect::new(204.0, 0.0, 200.0, 404.0), ts(1_000_005, "15:10")),
        ];
        assert_eq!(
            compute_footer_columns(&same_minute),
            vec![
                FooterColumn {
                    x: 0.0,
                    w: 200.0,
                    entries: vec![entry("15:10", false)],
                },
                FooterColumn {
                    x: 204.0,
                    w: 200.0,
                    entries: vec![entry("15:10", true)],
                },
            ]
        );

        // 4. ブラウザペイン（None）が混在しターミナルが 1 個だけのときは強調しない
        let mixed = vec![
            (Rect::new(0.0, 0.0, 200.0, 200.0), ts(100, "15:00")),
            (Rect::new(0.0, 204.0, 200.0, 200.0), None),
            (Rect::new(204.0, 0.0, 200.0, 404.0), None),
        ];
        assert_eq!(
            compute_footer_columns(&mixed),
            vec![
                FooterColumn {
                    x: 0.0,
                    w: 200.0,
                    entries: vec![entry("15:00", false)],
                },
                FooterColumn {
                    x: 204.0,
                    w: 200.0,
                    entries: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn test_layout_main_footer_does_not_overlap_inner_area() {
        use crate::layout::SidebarState;

        let layout = AppLayout::compute(1400, 900, 2.0, SidebarState::default());
        assert!(layout.main_footer_rect.h > 1.0);
        assert!(layout.main_inner_rect.bottom() <= layout.main_footer_rect.y);
        assert_eq!(layout.main_footer_rect.bottom(), layout.window_h);

        // 極小ウィンドウでも負サイズやパニックを起こさないこと
        let tiny = AppLayout::compute(200, 20, 2.0, SidebarState::default());
        assert!(tiny.main_footer_rect.h >= 0.0);
        assert!(tiny.main_inner_rect.h >= 1.0);
    }
}
