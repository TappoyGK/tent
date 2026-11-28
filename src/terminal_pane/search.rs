use ghostty_vt::Terminal;
use ghostty_vt::screen::CellWide;
use ghostty_vt::terminal::{Point, PointCoordinate};

/// 検索結果 1 件の `Point::Screen` 絶対座標範囲（同一行内の開始列〜終了列）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub screen_row: u32,
    pub start_col: u16,
    pub end_col: u16,
}

/// 1 回の検索で保持する最大マッチ数（巨大スクロールバックでのメモリ保護）
const MAX_SEARCH_MATCHES: usize = 10_000;

/// 端末のスクロールバック＋アクティブ画面全体（`0..total_rows`）から
/// 大文字・小文字を区別せずに `query` を検索し、マッチ一覧を上から順に返す。
pub(super) fn search_terminal_screen(
    terminal: &Terminal,
    cols: u16,
    query: &str,
) -> Vec<SearchMatch> {
    if query.is_empty() || cols == 0 {
        return Vec::new();
    }
    let total_rows = terminal.total_rows().unwrap_or(0) as u32;
    if total_rows == 0 {
        return Vec::new();
    }

    let needle_lower = query.to_lowercase();
    if needle_lower.is_empty() {
        return Vec::new();
    }
    let needle_len = needle_lower.len();

    let mut matches = Vec::new();
    let mut grapheme_buf = ['\0'; 16];
    let mut lower_line = String::with_capacity(usize::from(cols) * 2);
    // lower_line の各バイトオフセットに対応するセル列範囲 (start_col, end_col)
    let mut byte_to_col: Vec<(u16, u16)> = Vec::with_capacity(usize::from(cols) * 2);

    for row_idx in 0..total_rows {
        lower_line.clear();
        byte_to_col.clear();

        let mut has_non_space = false;
        let mut pending_spaces: Vec<u16> = Vec::new();

        for col_idx in 0..cols {
            let Ok(gr) = terminal.grid_ref(Point::Screen(PointCoordinate {
                x: col_idx,
                y: row_idx,
            })) else {
                pending_spaces.push(col_idx);
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
            if len == 0 || (len == 1 && grapheme_buf[0] == ' ') {
                pending_spaces.push(col_idx);
                continue;
            }

            // 溜めていた行中スペースを確定
            for sp_col in pending_spaces.drain(..) {
                lower_line.push(' ');
                byte_to_col.push((sp_col, sp_col));
            }
            has_non_space = true;

            let end_col = if matches!(wide, CellWide::Wide) {
                col_idx.saturating_add(1).min(cols.saturating_sub(1))
            } else {
                col_idx
            };

            for &ch in &grapheme_buf[..len] {
                for lc in ch.to_lowercase() {
                    let before = lower_line.len();
                    lower_line.push(lc);
                    let added = lower_line.len() - before;
                    for _ in 0..added {
                        byte_to_col.push((col_idx, end_col));
                    }
                }
            }
        }

        if !has_non_space || lower_line.len() < needle_len {
            continue;
        }

        let mut search_from = 0usize;
        while search_from + needle_len <= lower_line.len() {
            let Some(rel) = lower_line[search_from..].find(&needle_lower) else {
                break;
            };
            let abs_start = search_from + rel;
            let abs_end = abs_start + needle_len;
            if let (Some(&(sc, _)), Some(&(_, ec))) =
                (byte_to_col.get(abs_start), byte_to_col.get(abs_end - 1))
            {
                matches.push(SearchMatch {
                    screen_row: row_idx,
                    start_col: sc,
                    end_col: ec,
                });
                if matches.len() >= MAX_SEARCH_MATCHES {
                    return matches;
                }
            }
            // 重複マッチも拾いつつ確実に次の文字境界へ進める
            let step = lower_line[abs_start..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
            search_from = abs_start + step;
        }
    }

    matches
}
