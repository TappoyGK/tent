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

#[test]
fn test_resize_alone_does_not_trigger_waiting_input() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 80,
            rows: 20,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"1234567890123456789012345678901234567890\r\nready> ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let t0 = Instant::now();
    pane.mark_screen_seen();
    pane.last_output_instant = Some(t0);
    pane.had_background_output = true;

    // 裏スロットのまま幅が変わっても、折り返し変化だけで黄色（入力待ち）にならないこと
    pane.resize(20, 20, 10, 20);
    assert!(!pane.update_waiting_state(
        t0 + WAITING_IDLE_THRESHOLD + Duration::from_secs(1),
        false,
        None,
    ));
    assert!(!pane.is_waiting_input);
}

#[test]
fn test_work_indicator_in_upper_viewport_suppresses_waiting_input() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 24,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"prompt> ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    pane.mark_screen_seen();

    // ビューポート上部（下部8行より上）に esc to interrupt が表示されていても処理中として扱うこと
    pane.terminal
        .vt_write(b"\x1b[2;1HWorking... (esc to interrupt)");
    let t0 = Instant::now();
    pane.last_output_instant = Some(t0);
    pane.had_background_output = true;

    assert!(!pane.update_waiting_state(
        t0 + WAITING_IDLE_THRESHOLD + Duration::from_secs(1),
        false,
        None,
    ));
    assert!(!pane.is_waiting_input);
}

#[test]
fn test_notification_clears_when_background_output_resumes() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 10,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: Vec::new(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    // 起動猶予を超えた状態にする
    pane.created_at = Instant::now() - STARTUP_GRACE_PERIOD - Duration::from_secs(1);

    // 1. 裏で BEL を受けるとオレンジ（has_notification）になる
    pane.feed_pty_bytes(b"done\x07", false, true, "1");
    assert!(pane.has_notification);

    // 2. その後、同じペインで通常出力が再開したらオレンジが消えること
    pane.feed_pty_bytes(b"next step running...\r\n", false, true, "1");
    assert!(!pane.has_notification);
}

#[test]
fn test_clear_screen_and_scrollback_clears_viewport_and_history_across_resync() {
    use std::path::PathBuf;

    use ghostty_vt::terminal::ScrollViewport;

    use super::TerminalPane;
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
fn test_osc133_split_chunks_notify_long_job_and_ignore_short_job() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{
        BACKGROUND_NOTIFY_MIN_DURATION, STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD,
    };
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"ready> ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let now = Instant::now();
    pane.created_at = now - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // 1. チャンク境界を跨ぐ OSC 133;C でジョブ開始が検出されること
    pane.feed_pty_bytes(b"out\x1b", false, true, "1");
    assert_eq!(pane.osc133_job_started_at, None);
    pane.feed_pty_bytes(b"]133;C\x07building...\r\n", false, true, "1");
    assert!(pane.osc133_job_started_at.is_some());

    // 2. 途中の無出力区間（4秒アイドル）や入れ子プロンプトの 133;A があっても
    //    OSC 133 のジョブ開始時刻は保持され、誤通知も出ないこと
    let t0 = pane.osc133_job_started_at.unwrap();
    pane.last_output_instant = Some(t0);
    let _ = pane.update_waiting_state(t0 + WAITING_IDLE_THRESHOLD, false, Some("1"));
    assert_eq!(pane.osc133_job_started_at, Some(t0));
    assert!(pane.last_notification_sent.is_none());
    pane.feed_pty_bytes(b"\x1b]133;A\x07remote$ ", false, true, "1");
    assert_eq!(pane.osc133_job_started_at, Some(t0));

    // 3. 10秒経過した状態にして、チャンク境界を跨ぐ OSC 133;D で完了通知が発火すること
    pane.osc133_job_started_at =
        Some(Instant::now() - BACKGROUND_NOTIFY_MIN_DURATION - Duration::from_secs(5));
    pane.feed_pty_bytes(b"done\x1b]133", false, true, "1");
    assert!(pane.last_notification_sent.is_none());
    pane.feed_pty_bytes(b";D\x07$ ", false, true, "1");
    assert!(pane.last_notification_sent.is_some());
    assert_eq!(pane.osc133_job_started_at, None);
    assert!(pane.is_waiting_input);

    // 4. 短いコマンド（< 5s）では OSC 133;D で通知されず開始時刻がクリアされること
    pane.last_notification_sent = None;
    pane.feed_pty_bytes(
        b"\x1b]133;C\x07ls\r\nfoo\r\n\x1b]133;D\x07$ ",
        false,
        true,
        "1",
    );
    assert!(pane.last_notification_sent.is_none());
    assert_eq!(pane.osc133_job_started_at, None);
}

#[test]
fn test_active_work_indicator_notifies_on_completion_but_stale_text_does_not() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{
        BACKGROUND_NOTIFY_MIN_DURATION, STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD,
    };
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 50,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"ready> ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let now = Instant::now();
    pane.created_at = now - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // ケース A: 実行中インジケータが稼働し続け（5秒以上経過）、消えて完了したら通知されること
    pane.feed_pty_bytes(b"\r\nWorking... (esc to interrupt)", false, true, "1");
    assert!(pane.work_indicator_started_at.is_some());
    pane.work_indicator_started_at =
        Some(Instant::now() - BACKGROUND_NOTIFY_MIN_DURATION - Duration::from_secs(2));
    pane.feed_pty_bytes(b"\x1b[2J\x1b[HAll tasks done!\r\n> ", false, true, "1");
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(pane.should_notify_background_completion(after_idle));
    assert!(pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(pane.last_notification_sent.is_some());
    assert_eq!(pane.work_indicator_started_at, None);

    // ケース B: 過去ログに (esc to interrupt) が残ったまま 4 秒以上出力が止まった場合、
    //           開始時刻を引き継がず、後からスクロールアウトしても誤通知しないこと
    pane.last_notification_sent = None;
    pane.is_waiting_input = false;
    pane.mark_screen_seen();
    pane.feed_pty_bytes(b"old: (esc to interrupt)\r\n> ", false, true, "1");
    pane.work_indicator_started_at = Some(Instant::now() - Duration::from_secs(600));
    let idle1 = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.update_waiting_state(idle1, false, Some("1")));
    assert_eq!(pane.work_indicator_started_at, None);
    assert!(pane.last_notification_sent.is_none());

    pane.feed_pty_bytes(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n> ", false, true, "1");
    let idle2 = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(idle2));
    assert!(pane.update_waiting_state(idle2, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_continuous_output_pause_without_completion_boundary_does_not_notify() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"$ ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let now = Instant::now();
    pane.created_at = now - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // 出力が続いたあとに 4.1 秒出力が止まっても、完了境界がなければ OS 通知しないこと
    pane.feed_pty_bytes(
        b"Compiling a\r\nCompiling b\r\nCompiling c\r\n",
        false,
        true,
        "1",
    );
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(after_idle));
    assert!(pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_enter_started_tail_f_does_not_trigger_false_completion_notification() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"$ ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let now = Instant::now();
    pane.created_at = now - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // アクティブで Enter により tail -f を起動
    pane.write_input(b"tail -f app.log\r");
    pane.feed_pty_bytes(b"tail -f app.log\r\n[00:00] start\r\n", true, true, "1");
    pane.mark_screen_seen();

    // 60秒後に裏スロットで 1 行だけログが届き、4.1 秒アイドルになっても完了通知・黄色点灯しないこと
    pane.feed_pty_bytes(b"[01:00] heartbeat\r\n", false, true, "1");
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(after_idle));
    assert!(!pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_active_short_command_enter_does_not_trigger_later_background_notification() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"$ ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let now = Instant::now();
    pane.created_at = now - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // アクティブで短いコマンドを実行して完了・アイドルに戻る
    pane.write_input(b"ls\r");
    pane.feed_pty_bytes(b"ls\r\nfoo bar\r\n$ ", true, true, "1");
    pane.update_waiting_state(Instant::now() + WAITING_IDLE_THRESHOLD, true, Some("1"));
    pane.mark_screen_seen();

    // 後から裏スロットに回した状態で 1 行だけ無関係な出力が届いても完了通知しないこと
    pane.feed_pty_bytes(b"\r\n[1]+ Done  sleep 1\r\n$ ", false, true, "1");
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(after_idle));
    let _ = pane.update_waiting_state(after_idle, false, Some("1"));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_independent_periodic_short_logs_do_not_trigger_completion_notification() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 6,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"tail -f app.log".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    let t0 = Instant::now();
    pane.created_at = t0 - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // 1. t=0: 1行目の短いログ
    pane.feed_pty_bytes(b"\r\n[00:00] heartbeat 1", false, true, "1");

    // 2. t=4s: アイドル判定（1行の短いログは黄色にも通知対象にもならない）
    let t4 = t0 + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(t4));
    assert!(!pane.update_waiting_state(t4, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());

    // 3. t=60s: 1分後に独立した別の1行が届く
    pane.feed_pty_bytes(b"\r\n[01:00] heartbeat 2", false, true, "1");

    // 4. t=64s: 1行目と合算されず、短い独立出力として通知対象外であること
    let t64 = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(t64));
    assert!(!pane.update_waiting_state(t64, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_word_bounds_at_screen_ascii_punctuation_and_wide_chars() {
    use super::selection::{extract_selection_from_screen, word_bounds_at_screen};

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
    use super::selection::{extract_selection_from_screen, line_bounds_at_screen};

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

    use super::selection::{
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

    use super::TerminalPane;
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

#[test]
fn test_invisible_pty_redraw_does_not_update_timestamp_or_active_state() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 10,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"done 18:39\r\n> ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    pane.created_at = Instant::now() - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();
    let old_ts = pane.last_output_at - chrono::TimeDelta::minutes(90);
    pane.last_output_at = old_ts;
    pane.last_output_instant = None;

    // 1. タイトル更新・カーソル表示切替・同一内容の再描画では時刻も出力中フラグも更新されないこと
    pane.feed_pty_bytes(
        b"\x1b]0;Claude Code\x07\x1b[?25l\x1b[?25h",
        false,
        true,
        "1",
    );
    pane.feed_pty_bytes(
        b"\x1b[?2026h\x1b[1;1Hdone 18:39\x1b[?2026l",
        false,
        true,
        "1",
    );
    assert_eq!(pane.last_output_at, old_ts);
    assert_eq!(pane.last_output_instant, None);
    assert!(!pane.had_background_output);

    // 2. 実際に可視テキストが変わる出力が来たら時刻と出力中フラグが更新されること
    pane.feed_pty_bytes(b"\r\nnew response line", false, true, "1");
    assert!(pane.last_output_at > old_ts);
    assert!(pane.last_output_instant.is_some());
    assert!(pane.had_background_output);
}

#[test]
fn test_minor_footer_hint_change_does_not_trigger_waiting_input_but_real_reply_does() {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD};
    use crate::daemon::{DaemonClient, ReplayedPane};

    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 12,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"Main reply body\r\n\x1b[11;1H> \x1b[12;1Hauto mode on".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    pane.created_at = Instant::now() - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();

    // 1. 最下段のステータス行・ヒント 1〜2 行だけの書き換えでは黄色（入力待ち）にならないこと
    pane.feed_pty_bytes(
        b"\x1b[10;1Hnew task? /clear\x1b[12;1Hauto mode on - 1 agent",
        false,
        true,
        "1",
    );
    let t1 = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.update_waiting_state(t1, false, Some("1")));
    assert!(!pane.is_waiting_input);

    // 2. 3 行以上の実出力（AI の返答等）があれば黄色（入力待ち）になること
    pane.feed_pty_bytes(
        b"\x1b[2;1HReply line 1\x1b[3;1HReply line 2\x1b[4;1HReply line 3",
        false,
        true,
        "1",
    );
    let t2 = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(pane.update_waiting_state(t2, false, Some("1")));
    assert!(pane.is_waiting_input);
}
