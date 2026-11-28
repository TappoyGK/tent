use ghostty_vt::Terminal;
use ghostty_vt::screen::CellWide;
use ghostty_vt::terminal::{Point, PointCoordinate};

/// 検索結果 1 件の `Point::Screen` 絶対座標範囲（ソフトラップ跨ぎの複数行に対応）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start_row: u32,
    pub start_col: u16,
    pub end_row: u32,
    pub end_col: u16,
}

/// 1 回の検索で保持する最大マッチ数（巨大スクロールバックでのメモリ保護）
const MAX_SEARCH_MATCHES: usize = 10_000;

/// 端末のスクロールバック＋アクティブ画面全体（`0..total_rows`）から
/// 大文字・小文字を区別せずに `query` を検索し、マッチ一覧を上から順に返す。
/// ソフトラップで折り返された物理行は 1 つの論理行として連結して検索する。
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

    // 画面側と同じ `char::to_lowercase` を使って文字単位で小文字化する
    // （ギリシャ語語末シグマなど `str::to_lowercase` との差異による不一致を防ぐ）
    let needle_lower: String = query.chars().flat_map(char::to_lowercase).collect();
    if needle_lower.is_empty() {
        return Vec::new();
    }
    let needle_len = needle_lower.len();

    let mut matches = Vec::new();
    let mut grapheme_buf = ['\0'; 16];
    let mut lower_line = String::with_capacity(usize::from(cols) * 2);
    // lower_line の各バイトオフセットに対応する `(screen_row, start_col, end_col)`
    let mut byte_to_pos: Vec<(u32, u16, u16)> = Vec::with_capacity(usize::from(cols) * 2);
    let mut pending_spaces: Vec<(u32, u16)> = Vec::new();
    let mut has_non_space = false;

    for row_idx in 0..total_rows {
        let is_wrapped = terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y: row_idx }))
            .ok()
            .and_then(|gr| gr.row().ok())
            .and_then(|r| r.is_wrapped().ok())
            .unwrap_or(false);

        for col_idx in 0..cols {
            let Ok(gr) = terminal.grid_ref(Point::Screen(PointCoordinate {
                x: col_idx,
                y: row_idx,
            })) else {
                pending_spaces.push((row_idx, col_idx));
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
                pending_spaces.push((row_idx, col_idx));
                continue;
            }

            // 溜めていた行中スペースを確定
            for (sp_row, sp_col) in pending_spaces.drain(..) {
                lower_line.push(' ');
                byte_to_pos.push((sp_row, sp_col, sp_col));
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
                        byte_to_pos.push((row_idx, col_idx, end_col));
                    }
                }
            }
        }

        // ソフトラップで次の物理行へ続く場合は論理行を連結したまま継続する
        if is_wrapped && row_idx + 1 < total_rows {
            continue;
        }

        // 論理行の終端：末尾の余白スペースは捨ててから検索を実行
        pending_spaces.clear();

        if has_non_space && lower_line.len() >= needle_len {
            let mut search_from = 0usize;
            while search_from + needle_len <= lower_line.len() {
                let Some(rel) = lower_line[search_from..].find(&needle_lower) else {
                    break;
                };
                let abs_start = search_from + rel;
                let abs_end = abs_start + needle_len;
                if let (Some(&(sr, sc, _)), Some(&(er, _, ec))) =
                    (byte_to_pos.get(abs_start), byte_to_pos.get(abs_end - 1))
                {
                    matches.push(SearchMatch {
                        start_row: sr,
                        start_col: sc,
                        end_row: er,
                        end_col: ec,
                    });
                    if matches.len() >= MAX_SEARCH_MATCHES {
                        return matches;
                    }
                }
                // 非重複マッチとしてマッチ末尾から次を検索する（ブラウザの window.find と整合）
                search_from = abs_end;
            }
        }

        lower_line.clear();
        byte_to_pos.clear();
        has_non_space = false;
    }

    matches
}

/// 再検索後のマッチ一覧に対して、追跡ピンから解決した前回座標 `prev_pos` に最も対応するインデックスを返す。
pub fn resolve_tracked_search_match_index(
    matches: &[SearchMatch],
    prev_pos: Option<(u16, u32)>,
    had_current: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if let Some((pcol, prow)) = prev_pos {
        if let Some(exact) = matches
            .iter()
            .position(|m| m.start_row == prow && m.start_col == pcol)
        {
            return Some(exact);
        }
        let idx = matches.partition_point(|m| (m.start_row, m.start_col) < (prow, pcol));
        return Some(idx.min(matches.len() - 1));
    }
    if had_current {
        Some(0)
    } else {
        Some(matches.len() - 1)
    }
}
