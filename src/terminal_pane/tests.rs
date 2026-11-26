use ghostty_vt::render::Dirty;
use ghostty_vt::{RenderState, Terminal, TerminalOptions};

use super::selection::{extract_selection_from_terminal, find_url_at_cell};

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

    use super::TerminalPane;
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

    use super::TerminalPane;
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

    use super::TerminalPane;
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
