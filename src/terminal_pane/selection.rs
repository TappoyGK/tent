use ghostty_vt::Terminal;
use ghostty_vt::screen::CellWide;
use ghostty_vt::terminal::{Point, PointCoordinate, PointSpace};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlMatch {
    pub url: String,
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Other,
}

fn classify_cell_char(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

/// 現在のビューポート先頭行が `Point::Screen` 絶対座標で何行目かを返す
pub(super) fn viewport_top_screen_row(terminal: &Terminal) -> u32 {
    terminal
        .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
        .ok()
        .and_then(|gr| terminal.point_from_grid_ref(&gr, PointSpace::Screen).ok())
        .flatten()
        .map(|coord| coord.y)
        .unwrap_or(0)
}

/// `Point::Screen` 絶対座標の範囲 `[a, b]` を現在のビューポート座標へ投影する。
/// ビューポートと交差しない場合は `None` を返す。
pub(super) fn screen_selection_to_viewport(
    a: (u16, u32),
    b: (u16, u32),
    vp_top: u32,
    cols: u16,
    rows: u16,
) -> Option<((u16, u16), (u16, u16))> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let ((sc, sr), (ec, er)) = if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    };
    let vp_bottom = vp_top.saturating_add(u32::from(rows.saturating_sub(1)));
    if er < vp_top || sr > vp_bottom {
        return None;
    }
    let max_col = cols.saturating_sub(1);
    let vis_sr = sr.max(vp_top) - vp_top;
    let vis_sc = if sr < vp_top { 0 } else { sc.min(max_col) };
    let vis_er = er.min(vp_bottom) - vp_top;
    let vis_ec = if er > vp_bottom {
        max_col
    } else {
        ec.min(max_col)
    };
    Some(((vis_sc, vis_sr as u16), (vis_ec, vis_er as u16)))
}

/// 指定した `Point::Screen` 行・列のセルが属する単語（同一文字種連続区間）の列範囲 `(start_col, end_col)` を返す
pub(super) fn word_bounds_at_screen(
    terminal: &Terminal,
    cols: u16,
    col: u16,
    screen_row: u32,
) -> (u16, u16) {
    if cols == 0 {
        return (0, 0);
    }
    let max_col = cols.saturating_sub(1);
    let mut target = col.min(max_col);

    // セルごとの (文字種, CellWide) を取得
    let mut classes = Vec::with_capacity(usize::from(cols));
    let mut wides = Vec::with_capacity(usize::from(cols));
    let mut grapheme_buf = ['\0'; 16];
    for c in 0..cols {
        let Ok(gr) = terminal.grid_ref(Point::Screen(PointCoordinate {
            x: c,
            y: screen_row,
        })) else {
            classes.push(CharClass::Whitespace);
            wides.push(CellWide::Narrow);
            continue;
        };
        let wide = gr
            .cell()
            .ok()
            .and_then(|cell| cell.wide().ok())
            .unwrap_or(CellWide::Narrow);
        let len = gr.graphemes(&mut grapheme_buf).unwrap_or(0);
        let class = if len == 0 {
            CharClass::Whitespace
        } else {
            classify_cell_char(grapheme_buf[0])
        };
        classes.push(class);
        wides.push(wide);
    }

    // 全角文字の右側（SpacerTail）をクリックした場合は本体セル（1つ左）へ合わせる
    if matches!(wides[usize::from(target)], CellWide::SpacerTail) && target > 0 {
        target -= 1;
    }
    let target_class = classes[usize::from(target)];

    let mut start = target;
    while start > 0 {
        let prev = start - 1;
        let prev_idx = usize::from(prev);
        if matches!(wides[prev_idx], CellWide::SpacerTail) {
            if prev > 0 && classes[prev_idx - 1] == target_class {
                start = prev - 1;
                continue;
            }
            break;
        }
        if classes[prev_idx] == target_class {
            start = prev;
        } else {
            break;
        }
    }

    let mut end = target;
    if matches!(wides[usize::from(end)], CellWide::Wide) && end < max_col {
        end += 1;
    }
    while end < max_col {
        let next = end + 1;
        let next_idx = usize::from(next);
        if matches!(wides[next_idx], CellWide::SpacerHead) {
            break;
        }
        if classes[next_idx] == target_class {
            end = next;
            if matches!(wides[next_idx], CellWide::Wide) && end < max_col {
                end += 1;
            }
        } else {
            break;
        }
    }

    (start, end)
}

/// 指定した `Point::Screen` 行を含む論理行（ソフトラップ連結行）全体の範囲を返す
pub(super) fn line_bounds_at_screen(
    terminal: &Terminal,
    cols: u16,
    screen_row: u32,
) -> ((u16, u32), (u16, u32)) {
    let max_col = cols.saturating_sub(1);
    let total_rows = terminal.total_rows().unwrap_or(1).max(1) as u32;
    let clamped = screen_row.min(total_rows.saturating_sub(1));

    let is_row_wrapped = |y: u32| -> bool {
        terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y }))
            .ok()
            .and_then(|gr| gr.row().ok())
            .and_then(|r| r.is_wrapped().ok())
            .unwrap_or(false)
    };

    let mut start_row = clamped;
    while start_row > 0 && is_row_wrapped(start_row - 1) {
        start_row -= 1;
    }

    let mut end_row = clamped;
    while end_row + 1 < total_rows && is_row_wrapped(end_row) {
        end_row += 1;
    }

    ((0, start_row), (max_col, end_row))
}

/// `Point::Screen` 絶対座標の範囲 `[a, b]` から選択テキストを抽出する（スクロールバック跨ぎ対応）
pub(super) fn extract_selection_from_screen(
    terminal: &Terminal,
    cols: u16,
    a: (u16, u32),
    b: (u16, u32),
) -> String {
    let total_rows = terminal.total_rows().unwrap_or(0) as u32;
    if cols == 0 || total_rows == 0 {
        return String::new();
    }
    let ((sc, sr), (ec, er)) = if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    };
    let max_row = total_rows.saturating_sub(1);
    let max_col = cols.saturating_sub(1);
    let sr = sr.min(max_row);
    let er = er.min(max_row);

    let mut out = String::new();
    let mut first_row = true;
    let mut prev_wrapped = false;
    let mut grapheme_buf = ['\0'; 16];

    for row_idx in sr..=er {
        let is_wrapped = terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y: row_idx }))
            .ok()
            .and_then(|gr| gr.row().ok())
            .and_then(|r| r.is_wrapped().ok())
            .unwrap_or(false);

        let c_start = if row_idx == sr { sc.min(max_col) } else { 0 };
        let c_end = if row_idx == er {
            ec.min(max_col)
        } else {
            max_col
        };

        let mut line = String::new();
        for col_idx in c_start..=c_end {
            let Ok(gr) = terminal.grid_ref(Point::Screen(PointCoordinate {
                x: col_idx,
                y: row_idx,
            })) else {
                continue;
            };
            let wide = gr
                .cell()
                .ok()
                .and_then(|c| c.wide().ok())
                .unwrap_or(CellWide::Narrow);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            let len = gr.graphemes(&mut grapheme_buf).unwrap_or(0);
            if len == 0 {
                line.push(' ');
            } else {
                for &ch in &grapheme_buf[..len] {
                    line.push(ch);
                }
            }
        }

        if !first_row && !prev_wrapped {
            out.push('\n');
        }
        if is_wrapped && row_idx < er {
            out.push_str(&line);
        } else {
            out.push_str(line.trim_end());
        }
        first_row = false;
        prev_wrapped = is_wrapped;
    }

    out
}

pub(super) fn extract_selection_from_terminal(
    terminal: &Terminal,
    cols: u16,
    rows: u16,
    a: (u16, u16),
    b: (u16, u16),
) -> String {
    if cols == 0 || rows == 0 {
        return String::new();
    }
    let ((sc, sr), (ec, er)) = if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    };
    let max_row = rows.saturating_sub(1);
    let max_col = cols.saturating_sub(1);
    let sr = sr.min(max_row);
    let er = er.min(max_row);

    let mut out = String::new();
    let mut first_row = true;
    let mut prev_wrapped = false;
    let mut grapheme_buf = ['\0'; 16];

    for row_idx in sr..=er {
        let is_wrapped = terminal
            .grid_ref(Point::Viewport(PointCoordinate {
                x: 0,
                y: u32::from(row_idx),
            }))
            .ok()
            .and_then(|gr| gr.row().ok())
            .and_then(|r| r.is_wrapped().ok())
            .unwrap_or(false);

        let c_start = if row_idx == sr { sc.min(max_col) } else { 0 };
        let c_end = if row_idx == er {
            ec.min(max_col)
        } else {
            max_col
        };

        let mut line = String::new();
        for col_idx in c_start..=c_end {
            let Ok(gr) = terminal.grid_ref(Point::Viewport(PointCoordinate {
                x: col_idx,
                y: u32::from(row_idx),
            })) else {
                continue;
            };
            let wide = gr
                .cell()
                .ok()
                .and_then(|c| c.wide().ok())
                .unwrap_or(CellWide::Narrow);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            let len = gr.graphemes(&mut grapheme_buf).unwrap_or(0);
            if len == 0 {
                line.push(' ');
            } else {
                for &ch in &grapheme_buf[..len] {
                    line.push(ch);
                }
            }
        }

        if !first_row && !prev_wrapped {
            out.push('\n');
        }
        if is_wrapped && row_idx < er {
            out.push_str(&line);
        } else {
            out.push_str(line.trim_end());
        }
        first_row = false;
        prev_wrapped = is_wrapped;
    }

    out
}

pub(super) fn find_url_at_cell(
    terminal: &Terminal,
    cols: u16,
    col: u16,
    row: u16,
) -> Option<UrlMatch> {
    if let Ok(grid_ref) = terminal.grid_ref(Point::Viewport(PointCoordinate {
        x: col,
        y: u32::from(row),
    })) {
        let mut uri_buf = [0u8; 2048];
        if let Ok(len) = grid_ref.hyperlink_uri(&mut uri_buf)
            && len > 0
            && let Ok(uri) = std::str::from_utf8(&uri_buf[..len])
            && (uri.starts_with("http://") || uri.starts_with("https://"))
        {
            let mut start_col = col;
            let mut end_col = col;
            let mut probe = [0u8; 2048];
            while start_col > 0 {
                let next = start_col - 1;
                let same = terminal
                    .grid_ref(Point::Viewport(PointCoordinate {
                        x: next,
                        y: u32::from(row),
                    }))
                    .ok()
                    .and_then(|gr| gr.hyperlink_uri(&mut probe).ok())
                    .is_some_and(|n| n == len && probe[..n] == uri_buf[..len]);
                if same {
                    start_col = next;
                } else {
                    break;
                }
            }
            loop {
                let next = end_col.saturating_add(1);
                if next == end_col || next >= cols {
                    break;
                }
                let same = terminal
                    .grid_ref(Point::Viewport(PointCoordinate {
                        x: next,
                        y: u32::from(row),
                    }))
                    .ok()
                    .and_then(|gr| gr.hyperlink_uri(&mut probe).ok())
                    .is_some_and(|n| n == len && probe[..n] == uri_buf[..len]);
                if same {
                    end_col = next;
                } else {
                    break;
                }
            }
            return Some(UrlMatch {
                url: uri.to_string(),
                row,
                start_col,
                end_col,
            });
        }
    }

    let mut cells: Vec<(u16, String)> = Vec::new();
    let mut grapheme_buf = ['\0'; 16];
    for c_idx in 0..cols {
        let Ok(gr) = terminal.grid_ref(Point::Viewport(PointCoordinate {
            x: c_idx,
            y: u32::from(row),
        })) else {
            continue;
        };
        let wide = gr
            .cell()
            .ok()
            .and_then(|c| c.wide().ok())
            .unwrap_or(CellWide::Narrow);
        if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
            continue;
        }
        let len = gr.graphemes(&mut grapheme_buf).unwrap_or(0);
        let text = if len == 0 {
            " ".to_string()
        } else {
            grapheme_buf[..len].iter().collect()
        };
        cells.push((c_idx, text));
    }

    find_plain_url_in_row(&cells, row, col)
}

fn find_plain_url_in_row(cells: &[(u16, String)], row: u16, target_col: u16) -> Option<UrlMatch> {
    let mut line = String::new();
    let mut byte_to_col: Vec<u16> = Vec::new();
    for (col, text) in cells {
        for _ in 0..text.len() {
            byte_to_col.push(*col);
        }
        line.push_str(text);
    }

    let mut search_from = 0;
    while search_from < line.len() {
        let rest = &line[search_from..];
        let next_https = rest.find("https://");
        let next_http = rest.find("http://");
        let rel_start = match (next_https, next_http) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let abs_start = search_from + rel_start;
        let tail = &line[abs_start..];
        let rel_end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`') || c.is_control()
            })
            .unwrap_or(tail.len());
        let raw_url = &tail[..rel_end];
        let trimmed = trim_url_trailing_punctuation(raw_url);
        if !trimmed.is_empty() {
            let abs_end = abs_start + trimmed.len();
            let start_col = byte_to_col[abs_start];
            let end_col = byte_to_col[abs_end.saturating_sub(1)];
            if target_col >= start_col && target_col <= end_col {
                return Some(UrlMatch {
                    url: trimmed.to_string(),
                    row,
                    start_col,
                    end_col,
                });
            }
        }
        search_from = abs_start + rel_end.max(1);
    }
    None
}

fn trim_url_trailing_punctuation(mut s: &str) -> &str {
    while let Some(last) = s.chars().last() {
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            ']' => s.matches('[').count() < s.matches(']').count(),
            '}' => s.matches('{').count() < s.matches('}').count(),
            _ => false,
        };
        if strip {
            s = &s[..s.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    s
}
