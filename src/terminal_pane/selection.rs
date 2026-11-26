use ghostty_vt::Terminal;
use ghostty_vt::screen::CellWide;
use ghostty_vt::terminal::{Point, PointCoordinate};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlMatch {
    pub url: String,
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
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
