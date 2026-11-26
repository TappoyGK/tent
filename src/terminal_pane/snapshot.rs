use ghostty_vt::render::{CursorVisualStyle, Dirty};
use ghostty_vt::screen::CellWide;
use ghostty_vt::style::RgbColor;

use super::TerminalPane;
/// 縮小プレビュー用の横結合済みラン（同色の連続セルを1矩形に統合）
#[derive(Clone, Copy, Debug)]
pub struct MinimapRun {
    pub row: u16,
    pub col_start: u16,
    pub col_len: u16,
    pub color: [f32; 4],
    pub is_bg: bool,
}

/// 縮小プレビューのキャッシュ
#[derive(Clone, Debug)]
pub struct MinimapSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub default_bg: [f32; 4],
    pub runs: Vec<MinimapRun>,
    pub cursor: Option<(u16, u16)>,
}

impl Default for MinimapSnapshot {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            default_bg: [0.09, 0.10, 0.13, 1.0],
            runs: Vec::new(),
            cursor: None,
        }
    }
}

/// メイン画面描画用の1スパン（セル列位置付き）
#[derive(Clone, Debug)]
pub struct StyledSpan {
    pub start_col: u16,
    pub text: String,
    pub fg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub is_wide: bool,
}

/// メイン画面描画用のセル背景
#[derive(Clone, Copy, Debug)]
pub struct CellBg {
    pub col: u16,
    pub row: u16,
    pub color: [f32; 4],
}

/// セル枠いっぱいに隙間なく描画するブロック要素・罫線文字
#[derive(Clone, Copy, Debug)]
pub struct BoxGlyph {
    pub col: u16,
    pub row: u16,
    pub ch: char,
    pub color: [f32; 4],
}

/// カーソル情報（位置と表示可否を分離し、非表示TUIでもIME位置を追従可能にする）
#[derive(Clone, Copy, Debug)]
pub struct CursorInfo {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub style: CursorVisualStyle,
    pub color: [f32; 4],
}

/// 1行分のスナップショット（行単位の差分更新用）
#[derive(Clone, Debug, Default)]
pub struct RowSnapshot {
    pub spans: Vec<StyledSpan>,
    pub cell_bgs: Vec<CellBg>,
    pub box_glyphs: Vec<BoxGlyph>,
}

/// メイン画面の1フレーム分のスナップショット
#[derive(Clone, Debug)]
pub struct MainScreenSnapshot {
    pub default_bg: [f32; 4],
    pub rows: Vec<RowSnapshot>,
    pub cursor: Option<CursorInfo>,
}

/// クリック／ホバー対象として検出された URL と画面上のセル範囲
impl TerminalPane {
    pub fn ensure_minimap_updated(&mut self) {
        if !self.minimap_dirty {
            return;
        }
        self.minimap_dirty = false;

        let Ok(snapshot) = self.render_state.update(&self.terminal) else {
            return;
        };
        let Ok(colors) = snapshot.colors() else {
            return;
        };
        let cols = snapshot.cols().unwrap_or(self.cols);
        let rows = snapshot.rows().unwrap_or(self.rows);
        let default_bg = rgb_to_f32(colors.background);

        let mut runs = Vec::new();
        if let Ok(mut row_it) = self.row_iter.update(&snapshot) {
            let mut row_idx: u16 = 0;
            while let Some(row) = row_it.next() {
                if let Ok(mut cell_it) = self.cell_iter.update(row) {
                    let mut col_idx: u16 = 0;
                    while let Some(cell) = cell_it.next() {
                        let style = cell.style().unwrap_or_default();
                        let mut fg_rgb =
                            cell.fg_color().ok().flatten().unwrap_or(colors.foreground);
                        let mut bg_opt = cell.bg_color().ok().flatten();
                        if style.inverse {
                            let actual_bg = bg_opt.unwrap_or(colors.background);
                            bg_opt = Some(fg_rgb);
                            fg_rgb = actual_bg;
                        }

                        if let Some(bg_rgb) = bg_opt
                            && bg_rgb != colors.background
                        {
                            push_minimap_run(&mut runs, row_idx, col_idx, rgb_to_f32(bg_rgb), true);
                        }

                        if cell.graphemes_len().unwrap_or(0) > 0 {
                            let c = rgb_to_f32(fg_rgb);
                            push_minimap_run(
                                &mut runs,
                                row_idx,
                                col_idx,
                                [c[0], c[1], c[2], 0.85],
                                false,
                            );
                        }

                        col_idx = col_idx.saturating_add(1);
                    }
                }
                row_idx = row_idx.saturating_add(1);
            }
        }

        let cursor = snapshot.cursor().ok().and_then(|c| {
            if c.visible {
                c.viewport.map(|v| (v.x, v.y))
            } else {
                None
            }
        });

        let _ = snapshot.clean();

        self.minimap = MinimapSnapshot {
            cols,
            rows,
            default_bg,
            runs,
            cursor,
        };
    }

    /// メイン画面スナップショットを ghostty-vt の行単位 dirty 追跡で差分更新する。
    pub fn update_main_snapshot(
        &mut self,
        existing: Option<MainScreenSnapshot>,
        force_full: bool,
    ) -> Option<(MainScreenSnapshot, Vec<usize>)> {
        let snapshot = self.render_state.update(&self.terminal).ok()?;
        let colors = snapshot.colors().ok()?;
        let cols = snapshot.cols().unwrap_or(self.cols);
        let rows_count = snapshot.rows().unwrap_or(self.rows) as usize;
        let default_bg = rgb_to_f32(colors.background);

        let dirty_state = snapshot.dirty().unwrap_or(Dirty::Full);
        let full_rebuild = force_full
            || existing.is_none()
            || dirty_state == Dirty::Full
            || existing
                .as_ref()
                .is_some_and(|e| e.rows.len() != rows_count);

        let mut rows = match existing {
            Some(e) if !full_rebuild => e.rows,
            _ => vec![RowSnapshot::default(); rows_count],
        };
        let mut dirty_rows = Vec::new();

        if let Ok(mut row_it) = self.row_iter.update(&snapshot) {
            let mut row_idx: usize = 0;
            let mut grapheme_buf = String::with_capacity(16);

            while let Some(row) = row_it.next() {
                if row_idx >= rows_count {
                    break;
                }
                let row_dirty = full_rebuild || row.dirty().unwrap_or(true);
                if row_dirty {
                    let mut spans: Vec<StyledSpan> = Vec::new();
                    let mut cell_bgs: Vec<CellBg> = Vec::new();
                    let mut box_glyphs: Vec<BoxGlyph> = Vec::new();

                    if let Ok(mut cell_it) = self.cell_iter.update(row) {
                        let mut col_idx: u16 = 0;
                        while let Some(cell) = cell_it.next() {
                            let style = cell.style().unwrap_or_default();
                            let mut fg_rgb =
                                cell.fg_color().ok().flatten().unwrap_or(colors.foreground);
                            let mut bg_opt = cell.bg_color().ok().flatten();

                            if style.inverse {
                                let actual_bg = bg_opt.unwrap_or(colors.background);
                                bg_opt = Some(fg_rgb);
                                fg_rgb = actual_bg;
                            }
                            if style.faint {
                                let actual_bg = bg_opt.unwrap_or(colors.background);
                                fg_rgb = blend_rgb(fg_rgb, actual_bg, 0.50);
                            }
                            if style.invisible {
                                fg_rgb = bg_opt.unwrap_or(colors.background);
                            }

                            if let Some(bg_rgb) = bg_opt
                                && bg_rgb != colors.background
                            {
                                cell_bgs.push(CellBg {
                                    col: col_idx,
                                    row: row_idx as u16,
                                    color: rgb_to_f32(bg_rgb),
                                });
                            }

                            let wide_prop = cell
                                .raw_cell()
                                .ok()
                                .and_then(|c| c.wide().ok())
                                .unwrap_or(CellWide::Narrow);

                            if wide_prop == CellWide::SpacerTail {
                                col_idx = col_idx.saturating_add(1);
                                continue;
                            }
                            let is_wide = wide_prop == CellWide::Wide;

                            grapheme_buf.clear();
                            if cell.graphemes_len().unwrap_or(0) > 0 {
                                let _ = cell.graphemes_utf8(&mut grapheme_buf);
                            }

                            if !grapheme_buf.is_empty() {
                                let mut chars_it = grapheme_buf.chars();
                                if let (Some(ch), None) = (chars_it.next(), chars_it.next())
                                    && is_custom_box_or_block(ch)
                                {
                                    box_glyphs.push(BoxGlyph {
                                        col: col_idx,
                                        row: row_idx as u16,
                                        ch,
                                        color: rgb_to_f32(fg_rgb),
                                    });
                                    col_idx = col_idx.saturating_add(1);
                                    continue;
                                }

                                let fg = [fg_rgb.r, fg_rgb.g, fg_rgb.b];
                                let can_merge = !is_wide
                                    && spans.last().is_some_and(|last| {
                                        !last.is_wide
                                            && last.fg == fg
                                            && last.bold == style.bold
                                            && last.italic == style.italic
                                            && last.start_col as usize + last.text.chars().count()
                                                == col_idx as usize
                                            && last.text.is_ascii()
                                            && grapheme_buf.is_ascii()
                                    });

                                if can_merge {
                                    if let Some(last) = spans.last_mut() {
                                        last.text.push_str(&grapheme_buf);
                                    }
                                } else {
                                    spans.push(StyledSpan {
                                        start_col: col_idx,
                                        text: grapheme_buf.clone(),
                                        fg,
                                        bold: style.bold,
                                        italic: style.italic,
                                        is_wide,
                                    });
                                }
                            }

                            col_idx = col_idx.saturating_add(1);
                        }
                    }

                    rows[row_idx] = RowSnapshot {
                        spans,
                        cell_bgs,
                        box_glyphs,
                    };
                    dirty_rows.push(row_idx);
                }
                row_idx += 1;
            }
        }

        let cursor_color = rgb_to_f32(colors.cursor.unwrap_or(RgbColor {
            r: 0x88,
            g: 0xc0,
            b: 0xd0,
        }));
        let cursor = snapshot.cursor().ok().and_then(|c| {
            c.viewport.map(|v| CursorInfo {
                x: v.x,
                y: v.y,
                visible: c.visible,
                style: c.visual_style,
                color: cursor_color,
            })
        });

        let _ = snapshot.clean();

        if !dirty_rows.is_empty() || self.minimap_dirty {
            let mut runs = Vec::new();
            for (r_idx, row_snap) in rows.iter().enumerate() {
                for bg in &row_snap.cell_bgs {
                    push_minimap_run(&mut runs, r_idx as u16, bg.col, bg.color, true);
                }
                for b in &row_snap.box_glyphs {
                    push_minimap_run(
                        &mut runs,
                        r_idx as u16,
                        b.col,
                        [b.color[0], b.color[1], b.color[2], 0.85],
                        false,
                    );
                }
                for s in &row_snap.spans {
                    let c = [
                        s.fg[0] as f32 / 255.0,
                        s.fg[1] as f32 / 255.0,
                        s.fg[2] as f32 / 255.0,
                        0.85,
                    ];
                    let len = if s.is_wide {
                        2
                    } else {
                        s.text.chars().count().max(1) as u16
                    };
                    runs.push(MinimapRun {
                        row: r_idx as u16,
                        col_start: s.start_col,
                        col_len: len,
                        color: c,
                        is_bg: false,
                    });
                }
            }
            self.minimap = MinimapSnapshot {
                cols,
                rows: rows_count as u16,
                default_bg,
                runs,
                cursor: cursor.filter(|c| c.visible).map(|c| (c.x, c.y)),
            };
            self.minimap_dirty = false;
        }

        Some((
            MainScreenSnapshot {
                default_bg,
                rows,
                cursor,
            },
            dirty_rows,
        ))
    }
}

fn push_minimap_run(runs: &mut Vec<MinimapRun>, row: u16, col: u16, color: [f32; 4], is_bg: bool) {
    if let Some(last) = runs.last_mut()
        && last.row == row
        && last.is_bg == is_bg
        && last.col_start + last.col_len == col
        && last.color == color
    {
        last.col_len = last.col_len.saturating_add(1);
        return;
    }
    runs.push(MinimapRun {
        row,
        col_start: col,
        col_len: 1,
        color,
        is_bg,
    });
}

pub fn is_custom_box_or_block(ch: char) -> bool {
    matches!(
        ch,
        '\u{2580}'..='\u{2595}'
            | '\u{2500}'..='\u{2503}'
            | '\u{250C}'
            | '\u{2510}'
            | '\u{2514}'
            | '\u{2518}'
            | '\u{251C}'
            | '\u{2524}'
            | '\u{252C}'
            | '\u{2534}'
            | '\u{253C}'
            | '\u{256D}'..='\u{2570}'
    )
}

fn blend_rgb(fg: RgbColor, bg: RgbColor, fg_weight: f32) -> RgbColor {
    let bg_weight = 1.0 - fg_weight;
    RgbColor {
        r: (f32::from(fg.r) * fg_weight + f32::from(bg.r) * bg_weight).round() as u8,
        g: (f32::from(fg.g) * fg_weight + f32::from(bg.g) * bg_weight).round() as u8,
        b: (f32::from(fg.b) * fg_weight + f32::from(bg.b) * bg_weight).round() as u8,
    }
}

fn rgb_to_f32(c: RgbColor) -> [f32; 4] {
    [
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        1.0,
    ]
}
