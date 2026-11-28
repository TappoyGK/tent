use ghostty_vt::render::Dirty;
use ghostty_vt::{RenderState, Terminal, TerminalOptions};

use super::super::selection::{extract_selection_from_terminal, find_url_at_cell};

#[test]
fn test_extract_selection_text_ascii_and_japanese() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 40,
        rows: 5,
        ..Default::default()
    })
    .unwrap();
    term.vt_write(b"hello world\r\n");
    term.vt_write("日本語テスト\r\n".as_bytes());
    term.vt_write(b"line3");

    let sel1 = extract_selection_from_terminal(&term, 40, 5, (0, 0), (4, 0));
    assert_eq!(sel1, "hello");

    let sel2 = extract_selection_from_terminal(&term, 40, 5, (6, 0), (5, 1));
    assert_eq!(sel2, "world\n日本語");
}

#[test]
fn test_find_url_at_cell_plain_and_osc8() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 5,
        ..Default::default()
    })
    .unwrap();
    term.vt_write(b"see (https://example.com/foo_(bar)). done\r\n");
    term.vt_write(b"\x1b]8;;https://github.com/ghostty-org/ghostty\x1b\\Ghostty\x1b]8;;\x1b\\");

    let m1 = find_url_at_cell(&term, 80, 10, 0).unwrap();
    assert_eq!(m1.url, "https://example.com/foo_(bar)");
    assert_eq!(m1.start_col, 5);
    assert_eq!(m1.end_col, 33);

    assert!(find_url_at_cell(&term, 80, 0, 0).is_none());

    let m2 = find_url_at_cell(&term, 80, 3, 1).unwrap();
    assert_eq!(m2.url, "https://github.com/ghostty-org/ghostty");
    assert_eq!(m2.start_col, 0);
    assert_eq!(m2.end_col, 6);
}

#[test]
fn test_extract_selection_unwraps_soft_wrapped_lines() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 5,
        rows: 4,
        ..Default::default()
    })
    .unwrap();
    // 幅5セルに10文字出力 → 2行に自動折り返し、その後明示改行で3行目
    term.vt_write(b"abcdefghij\r\nxy");

    let sel = extract_selection_from_terminal(&term, 5, 4, (0, 0), (4, 2));
    assert_eq!(sel, "abcdefghij\nxy");
}

#[test]
fn test_extract_selection_does_not_clear_main_render_dirty() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 20,
        rows: 4,
        ..Default::default()
    })
    .unwrap();
    term.vt_write(b"old");

    let mut main_rs = RenderState::new().unwrap();

    // 初回描画済みとして clean にする
    {
        let snap = main_rs.update(&term).unwrap();
        snap.clean().unwrap();
    }

    // 未描画の新しい出力を流す
    term.vt_write(b"\rnew");

    // コピー抽出は GridRef 経由で RenderState に触れないため、main_rs の dirty フラグを消さない
    let copied = extract_selection_from_terminal(&term, 20, 4, (0, 0), (2, 0));
    assert_eq!(copied, "new");

    let snap = main_rs.update(&term).unwrap();
    assert_ne!(snap.dirty().unwrap(), Dirty::Clean);
}

#[test]
fn test_apply_resync_snapshot_preserves_latest_resize() {
    use std::path::PathBuf;

    use super::super::TerminalPane;
    use crate::daemon::{DaemonClient, ReplayedPane, encode_terminal_snapshot};

    // 80x24 の旧スナップショットを作成
    let mut old_term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        ..Default::default()
    })
    .unwrap();
    old_term.vt_write(b"RESYNC_CONTENT");
    let snap_bytes = encode_terminal_snapshot(&old_term);

    // GUI 側ペインを 80x24 で復元したあと 120x40 へリサイズ
    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 80,
            rows: 24,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: Vec::new(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    pane.resize(120, 40, 10, 20);
    assert_eq!((pane.cols, pane.rows), (120, 40));

    // 遅れて届いた 80x24 の再同期スナップショットを適用しても 120x40 が維持されること
    pane.apply_resync_snapshot(ReplayedPane {
        pane_id: 1,
        cols: 80,
        rows: 24,
        cwd: PathBuf::from("/tmp"),
        replay_bytes: snap_bytes,
    });

    assert_eq!((pane.cols, pane.rows), (120, 40));
    assert_eq!(pane.terminal.cols().unwrap(), 120);
    assert_eq!(pane.terminal.rows().unwrap(), 40);
    assert!(
        extract_selection_from_terminal(&pane.terminal, 120, 40, (0, 0), (13, 0))
            .contains("RESYNC_CONTENT")
    );
}

#[test]
fn test_faint_style_dims_foreground_color() {
    use std::path::PathBuf;

    use super::super::TerminalPane;
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 4,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"normal\x1b[2mfaint\x1b[0m".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();

    let (snap, _) = pane.update_main_snapshot(None, true).unwrap();
    let row0 = &snap.rows[0].spans;
    assert_eq!(row0.len(), 2);
    assert_eq!(row0[0].text, "normal");
    assert_eq!(row0[1].text, "faint");
    // SGR 2 (faint/dim) は通常文字より明確に暗いグレーになること
    assert!(row0[1].fg[0] < row0[0].fg[0] - 40);
    assert!(row0[1].fg[1] < row0[0].fg[1] - 40);
    assert!(row0[1].fg[2] < row0[0].fg[2] - 40);
}

#[test]
fn test_sync_output_mode_2026_holds_render_until_reset() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::super::TerminalPane;
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 4,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: Vec::new(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();

    let t0 = Instant::now();
    assert!(!pane.is_render_held(t0));

    // Mode 2026 開始中は描画ホールド（チラつき防止）
    pane.terminal.vt_write(b"\x1b[?2026h\x1b[2Jpartial");
    assert!(pane.is_render_held(t0));
    // 最大ホールド時間超過で自動解除（ハング防止）
    assert!(!pane.is_render_held(t0 + Duration::from_secs(2)));

    // Mode 2026 解除で即座に描画再開
    pane.terminal.vt_write(b"\x1b[?2026l");
    assert!(!pane.is_render_held(t0));
}

#[test]
fn test_clear_screen_and_scrollback_clears_viewport_and_history_across_resync() {
    use std::path::PathBuf;

    use ghostty_vt::terminal::ScrollViewport;

    use super::super::TerminalPane;
    use crate::daemon::{DaemonClient, ReplayedPane, encode_terminal_snapshot};

    // デーモン側と GUI 側の双方にスクロールバック付きの内容を流す
    let initial =
        b"PRE_CLEAR_MARKER\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\nline7\r\nline8";
    let mut daemon_vt = Terminal::new(TerminalOptions {
        cols: 40,
        rows: 4,
        ..Default::default()
    })
    .unwrap();
    daemon_vt.vt_write(initial);

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 4,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: encode_terminal_snapshot(&daemon_vt),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();

    // クリア前はスクロールアップで PRE_CLEAR_MARKER が見えること
    pane.terminal.scroll_viewport(ScrollViewport::Top);
    let before = pane.extract_selection_text((0, 0), (39, 3));
    assert!(before.contains("PRE_CLEAR_MARKER"));
    pane.terminal.scroll_viewport(ScrollViewport::Bottom);

    // ClientMsg::Clear 相当の消去をデーモン側・GUI 側へ適用
    const CLEAR_SEQ: &[u8] = b"\x1b[3J\x1b[2J\x1b[H";
    daemon_vt.vt_write(CLEAR_SEQ);
    pane.clear_screen_and_scrollback();

    // GUI の可視領域・スクロールバックともに消去されていること
    pane.terminal.scroll_viewport(ScrollViewport::Top);
    let after_clear = pane.extract_selection_text((0, 0), (39, 3));
    assert!(!after_clear.contains("PRE_CLEAR_MARKER"));
    assert!(after_clear.trim().is_empty());

    // デーモンから再同期スナップショットが適用されても PRE_CLEAR_MARKER が復活しないこと
    pane.apply_resync_snapshot(ReplayedPane {
        pane_id: 1,
        cols: 40,
        rows: 4,
        cwd: PathBuf::from("/tmp"),
        replay_bytes: encode_terminal_snapshot(&daemon_vt),
    });
    pane.terminal.scroll_viewport(ScrollViewport::Top);
    let after_resync = pane.extract_selection_text((0, 0), (39, 3));
    assert!(!after_resync.contains("PRE_CLEAR_MARKER"));
    assert!(after_resync.trim().is_empty());
}

#[test]
fn test_word_bounds_at_screen_ascii_punctuation_and_wide_chars() {
    use super::super::selection::{extract_selection_from_screen, word_bounds_at_screen};

    let mut term = Terminal::new(TerminalOptions {
        cols: 40,
        rows: 4,
        ..Default::default()
    })
    .unwrap();
    term.vt_write(b"foo_bar(baz) \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e test");

    // 英数字 + '_' の単語
    let (s1, e1) = word_bounds_at_screen(&term, 40, 2, 0);
    assert_eq!(
        extract_selection_from_screen(&term, 40, (s1, 0), (e1, 0)),
        "foo_bar"
    );

    // 記号 '('
    let (s2, e2) = word_bounds_at_screen(&term, 40, 7, 0);
    assert_eq!(
        extract_selection_from_screen(&term, 40, (s2, 0), (e2, 0)),
        "("
    );

    // 全角文字「日本語」の右側 SpacerTail セルをクリックしても単語全体が選択されること
    // "foo_bar(baz) " は 13 セル → "日" は col 13..14, "本" は 15..16, "語" は 17..18
    let (s3, e3) = word_bounds_at_screen(&term, 40, 14, 0);
    assert_eq!((s3, e3), (13, 18));
    assert_eq!(
        extract_selection_from_screen(&term, 40, (s3, 0), (e3, 0)),
        "日本語"
    );
}

#[test]
fn test_line_bounds_at_screen_includes_soft_wrapped_lines() {
    use super::super::selection::{extract_selection_from_screen, line_bounds_at_screen};

    let mut term = Terminal::new(TerminalOptions {
        cols: 5,
        rows: 5,
        ..Default::default()
    })
    .unwrap();
    // 1行目: "first" (改行あり), 2〜4行目: "abcdefghijkl" (5文字幅で3行にソフトラップ)
    term.vt_write(b"first\r\nabcdefghijkl\r\nlast");

    // ソフトラップ中段（row=2）をクリックすると論理行全体（row=1..=3）が選択されること
    let (start, end) = line_bounds_at_screen(&term, 5, 2);
    assert_eq!(start, (0, 1));
    assert_eq!(end, (4, 3));
    assert_eq!(
        extract_selection_from_screen(&term, 5, start, end),
        "abcdefghijkl"
    );

    // 通常行（row=0）は 1 行のみ
    let (s0, e0) = line_bounds_at_screen(&term, 5, 0);
    assert_eq!(s0, (0, 0));
    assert_eq!(e0, (4, 0));
    assert_eq!(extract_selection_from_screen(&term, 5, s0, e0), "first");
}

#[test]
fn test_screen_selection_across_scrollback_and_viewport_projection() {
    use ghostty_vt::terminal::ScrollViewport;

    use super::super::selection::{
        extract_selection_from_screen, screen_selection_to_viewport, viewport_top_screen_row,
    };

    let mut term = Terminal::new(TerminalOptions {
        cols: 20,
        rows: 4,
        ..Default::default()
    })
    .unwrap();
    for i in 1..=12 {
        term.vt_write(format!("line{i:02}\r\n").as_bytes());
    }

    // 最下部（active）でのビューポート先頭 screen_row を確認
    let bottom_top = viewport_top_screen_row(&term);
    assert!(bottom_top > 0);

    // 上へ 5 行スクロールバックしても Screen 絶対座標の選択がズレずに抽出・投影できること
    term.scroll_viewport(ScrollViewport::Delta(-5));
    let scrolled_top = viewport_top_screen_row(&term);
    assert!(scrolled_top < bottom_top);

    let sel_start = (0u16, scrolled_top);
    let sel_end = (5u16, bottom_top + 1);
    let extracted = extract_selection_from_screen(&term, 20, sel_start, sel_end);
    assert!(extracted.lines().count() >= 6);

    // 現在のビューポートへ投影すると上端 (0, 0) から下端 (19, 3) にクリップされること
    let proj = screen_selection_to_viewport(sel_start, sel_end, scrolled_top, 20, 4);
    assert_eq!(proj, Some(((0, 0), (19, 3))));

    // 完全に画面外の範囲は None を返すこと
    let offscreen = screen_selection_to_viewport((0, 0), (5, 0), bottom_top, 20, 4);
    assert_eq!(offscreen, None);
}

#[test]
fn test_tracked_selection_survives_scrollback_pruning_and_clears_on_csi_3j() {
    use std::path::PathBuf;

    use super::super::TerminalPane;
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 12,
            rows: 3,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: Vec::new(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    // スクロールバック上限を最小にしてページ単位の先頭パージを確実に発生させる
    pane.terminal.set_scrollback_max_bytes(Some(1)).unwrap();

    // まず十分な行数を流してパージ閾値付近まで溜める
    let mut line_no: u32 = 0;
    let mut batch = String::new();
    for _ in 0..10_000 {
        line_no += 1;
        batch.push_str(&format!("L{line_no:06}\r\n"));
    }
    pane.terminal.vt_write(batch.as_bytes());

    // 1. 末尾付近の行（パージ後も残る位置）と、先頭 (0, 0)（パージで消える位置）にピンを立てる
    let vp_top = pane.viewport_top_screen_row();
    let tail_row = vp_top.saturating_sub(2);
    let tail_a = pane.track_screen_point((0, tail_row)).unwrap();
    let tail_b = pane.track_screen_point((6, tail_row)).unwrap();
    let head_pin = pane.track_screen_point((0, 0)).unwrap();

    let original_tail = pane.extract_tracked_selection_text(&tail_a, &tail_b);
    assert!(original_tail.starts_with('L'));
    let before_tail_y = pane.resolve_tracked_point(&tail_a).unwrap().1;

    // 実際にスクロールバック先頭がパージされる（head_pin が None になり tail_a の y が減少する）まで流す
    let mut pruned = false;
    for _ in 0..30 {
        batch.clear();
        for _ in 0..1_000 {
            line_no += 1;
            batch.push_str(&format!("L{line_no:06}\r\n"));
        }
        pane.terminal.vt_write(batch.as_bytes());
        if pane.resolve_tracked_point(&head_pin).is_none() {
            pruned = true;
            break;
        }
    }
    assert!(pruned, "scrollback head pruning must have occurred");

    // 2. パージで座標が前へ詰まっても、残存行のピンは元の内容を指し続けること
    let after_tail_y = pane.resolve_tracked_point(&tail_a).unwrap().1;
    assert!(after_tail_y < before_tail_y);
    assert_eq!(
        pane.extract_tracked_selection_text(&tail_a, &tail_b),
        original_tail
    );

    // 3. 片方の端（head_pin）だけがパージされた場合、先頭 (0, 0) へ寄せて残存部分の選択が保たれること
    let clamped_range = pane.resolve_tracked_selection_range(&head_pin, &tail_b);
    assert_eq!(clamped_range, Some(((0, 0), (6, after_tail_y))));
    let partial_text = pane.extract_tracked_selection_text(&head_pin, &tail_b);
    assert!(partial_text.ends_with(&original_tail));

    // 4. CSI 3 J（スクロールバック消去）で両方消えた場合は別内容を指さず無効になること
    pane.terminal.vt_write(b"\x1b[3J");
    assert_eq!(pane.resolve_tracked_selection_range(&tail_a, &tail_b), None);
    assert_eq!(pane.tracked_selection_to_viewport(&tail_a, &tail_b), None);
    assert_eq!(pane.extract_tracked_selection_text(&tail_a, &tail_b), "");
}
