use std::path::PathBuf;

use super::super::{SearchMatch, TerminalPane, resolve_tracked_search_match_index};
use crate::daemon::{DaemonClient, ReplayedPane};

#[test]
fn test_search_screen_across_scrollback_case_insensitive_wide_chars_and_scroll_jump() {
    let mut content = String::new();
    content.push_str("Error: first failure\r\n");
    for i in 1..=25 {
        content.push_str(&format!("log line {i:02}\r\n"));
    }
    content.push_str("日本語error確認\r\n");
    content.push_str("final ERROR end");

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: content.into_bytes(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();

    // 1. 大文字・小文字区別なしでスクロールバック先頭〜現在画面まで全件ヒットすること
    let hits = pane.search_screen("error");
    assert_eq!(hits.len(), 3);
    assert_eq!(
        hits[0],
        SearchMatch {
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 4,
        }
    );
    // "日本語" は 3 文字 × 2 セル = 列 0..=5 なので "error" は列 6..=10
    assert_eq!(hits[1].start_col, 6);
    assert_eq!(hits[1].end_col, 10);
    // 最終行の "final ERROR end" は列 6..=10
    assert_eq!(hits[2].start_col, 6);
    assert_eq!(hits[2].end_col, 10);

    // 2. 全角日本語の検索でもワイド文字の終端セル（右側セル）まで含めて一致範囲を返すこと
    let jp_hits = pane.search_screen("日本語");
    assert_eq!(jp_hits.len(), 1);
    assert_eq!(jp_hits[0].start_col, 0);
    assert_eq!(jp_hits[0].end_col, 5);

    // 3. スクロールバック先頭のヒット行へスクロールジャンプできること
    assert!(pane.viewport_top_screen_row() > 0);
    pane.scroll_to_screen_row(hits[0].start_row);
    let vp_top = pane.viewport_top_screen_row();
    assert!(hits[0].start_row >= vp_top && hits[0].start_row < vp_top + u32::from(pane.rows));

    // 4. 空クエリは 0 件
    assert!(pane.search_screen("").is_empty());
}

#[test]
fn test_search_screen_soft_wrap_non_overlapping_and_greek_sigma() {
    // cols=10 で "abcdefghijklmno" が 2 行にソフトラップする
    let pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 10,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"abcdefghijklmno\r\naaaa\r\n\xce\x9f\xce\x94\xce\x9f\xce\xa3".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();

    // ソフトラップ境界をまたぐ "jk" が 2 行跨ぎでヒットすること
    let wrapped = pane.search_screen("jk");
    assert_eq!(
        wrapped,
        vec![SearchMatch {
            start_row: 0,
            start_col: 9,
            end_row: 1,
            end_col: 0,
        }]
    );

    // 重複しないマッチ（"aaaa" の "aa" は 2 件）
    let non_overlap = pane.search_screen("aa");
    assert_eq!(non_overlap.len(), 2);
    assert_eq!(non_overlap[0].start_col, 0);
    assert_eq!(non_overlap[0].end_col, 1);
    assert_eq!(non_overlap[1].start_col, 2);
    assert_eq!(non_overlap[1].end_col, 3);

    // ギリシャ語語末シグマ（ΟΔΟΣ）も char::to_lowercase 統一でヒットすること
    let greek = pane.search_screen("ΟΔΟΣ");
    assert_eq!(greek.len(), 1);
}

#[test]
fn test_search_tracking_follows_reflow_and_scrollback_pruning() {
    // 1. リサイズ（リフロー）で行番号が変わっても追跡ピンと content_generation で追従できること
    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 20,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"0123456789abcde\r\ntarget_token\r\nend".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();

    let before_gen = pane.content_generation;
    let hits_before = pane.search_screen("target_token");
    assert_eq!(hits_before.len(), 1);
    assert_eq!(hits_before[0].start_row, 1);
    let pin = pane
        .track_screen_point((hits_before[0].start_col, hits_before[0].start_row))
        .unwrap();

    // 幅を 20 → 10 に狭めると 1 行目が 2 行に折り返され、target_token は row 2 へ移る
    pane.resize(10, 6, 8, 16);
    assert_ne!(pane.content_generation, before_gen);
    let moved_pos = pane.resolve_tracked_point(&pin);
    let hits_after = pane.search_screen("target_token");
    assert_eq!(hits_after.len(), 1);
    assert!(hits_after[0].start_row > hits_before[0].start_row);
    let resolved_idx = resolve_tracked_search_match_index(&hits_after, moved_pos, true);
    assert_eq!(resolved_idx, Some(0));
    assert_eq!(
        moved_pos,
        Some((hits_after[0].start_col, hits_after[0].start_row))
    );

    // 2. スクロールバック先頭パージでピンが失効した場合は、残存する最も古いマッチ（idx 0）へ寄せること
    let multi = vec![
        SearchMatch {
            start_row: 2,
            start_col: 0,
            end_row: 2,
            end_col: 3,
        },
        SearchMatch {
            start_row: 10,
            start_col: 0,
            end_row: 10,
            end_col: 3,
        },
        SearchMatch {
            start_row: 25,
            start_col: 0,
            end_row: 25,
            end_col: 3,
        },
    ];
    assert_eq!(
        resolve_tracked_search_match_index(&multi, None, true),
        Some(0)
    );
    assert_eq!(
        resolve_tracked_search_match_index(&multi, None, false),
        Some(2)
    );
}
