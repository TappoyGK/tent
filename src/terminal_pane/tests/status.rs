use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::super::{
    BACKGROUND_NOTIFY_MIN_DURATION, STARTUP_GRACE_PERIOD, TerminalPane, WAITING_IDLE_THRESHOLD,
};
use crate::daemon::{AiHookState, AiTool, DaemonClient, ReplayedPane};

#[test]
fn test_resize_alone_does_not_trigger_waiting_input() {
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
fn test_osc133_split_chunks_notify_long_job_and_ignore_short_job() {
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
    assert!(pane.update_waiting_state(t0 + WAITING_IDLE_THRESHOLD, false, Some("1")));
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

    // 60秒後に裏スロットで 1 行だけログが届き、4.1 秒アイドルになっても完了通知しないこと
    pane.feed_pty_bytes(b"[01:00] heartbeat\r\n", false, true, "1");
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(after_idle));
    assert!(pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_active_short_command_enter_does_not_trigger_later_background_notification() {
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
    assert!(pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_independent_periodic_short_logs_do_not_trigger_completion_notification() {
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

    // 2. t=4s: アイドル判定（通知対象外）
    let t4 = t0 + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(t4));
    assert!(pane.update_waiting_state(t4, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());

    // 3. t=60s: 1分後に独立した別の1行が届く
    pane.feed_pty_bytes(b"\r\n[01:00] heartbeat 2", false, true, "1");

    // 4. t=64s: 1行目と合算されず、短い独立出力として通知対象外であること
    let t64 = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(!pane.should_notify_background_completion(t64));
    assert!(pane.update_waiting_state(t64, false, Some("1")));
    assert!(pane.last_notification_sent.is_none());
}

#[test]
fn test_invisible_pty_redraw_does_not_update_timestamp_or_active_state() {
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
fn test_ai_managed_pane_ignores_pty_noise_and_tracks_hook_lifecycle() {
    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 60,
            rows: 12,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"Welcome to agy\r\n> ".to_vec(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    pane.created_at = Instant::now() - STARTUP_GRACE_PERIOD - Duration::from_secs(1);
    pane.mark_screen_seen();
    let old_ts = pane.last_output_at - chrono::TimeDelta::minutes(30);
    pane.last_output_at = old_ts;
    pane.last_output_instant = None;

    // 1. フック未受信の AI ペイン（フック未設定ユーザー）は一般ルールがそのまま動くこと
    assert!(pane.update_ai_tool_from_daemon(Some(AiTool::Agy)));
    assert!(!pane.is_ai_hook_managed());
    pane.feed_pty_bytes(b"\r\nunhooked output\r\n> ", false, true, "1");
    assert!(pane.last_output_at > old_ts);
    assert!(pane.had_background_output);

    // 2. SessionStart (Idle) 等のフックを受信した瞬間から AI フック管理モードに入り、
    //    以降 PTY にどんな TUI 再描画・フッター更新が来ても一般ルールが一切発火しないこと
    assert!(pane.apply_ai_hook_event(AiHookState::Idle, Some(AiTool::Agy), false, true, "1"));
    assert!(pane.is_ai_hook_managed());
    let ts_after_idle = pane.last_output_at;
    pane.feed_pty_bytes(
        b"\r\nstatus bar updated\r\nhint: press /help\r\nspinner ...",
        false,
        true,
        "1",
    );
    assert_eq!(pane.last_output_at, ts_after_idle);
    assert!(!pane.is_actively_outputting(Instant::now()));
    assert!(!pane.had_background_output);
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_secs(1);
    assert!(!pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(!pane.is_waiting_input);

    // 3. Working フックで緑LED（is_actively_outputting）が点灯し、2 秒以上無出力でも点灯を維持すること
    assert!(pane.apply_ai_hook_event(AiHookState::Working, Some(AiTool::Agy), false, true, "1"));
    assert!(pane.is_actively_outputting(Instant::now() + Duration::from_secs(30)));
    assert!(!pane.is_waiting_input);

    // 4. Waiting フックで緑が消え、裏スロットが黄LED（is_waiting_input）になること。
    //    あいだに Working / Idle / ユーザー入力 / スロット確認を挟まずに届いた重複 Waiting は時間経過にかかわらず抑止され、
    //    ユーザー入力（write_input）後の正当な Waiting は再び受け付けること
    assert!(pane.apply_ai_hook_event(AiHookState::Waiting, Some(AiTool::Agy), false, true, "1"));
    assert!(!pane.is_actively_outputting(Instant::now()));
    assert!(pane.is_waiting_input);
    let ts_first_waiting = pane.last_output_at;
    assert!(!pane.apply_ai_hook_event(AiHookState::Waiting, Some(AiTool::Agy), false, true, "1"));
    assert_eq!(pane.last_output_at, ts_first_waiting);

    // ユーザーが承認/拒否を入力すると Waiting が解除され、次の Waiting が正しく反応すること
    pane.write_input(b"y\r");
    assert!(!pane.is_waiting_input);
    assert!(pane.apply_ai_hook_event(AiHookState::Waiting, Some(AiTool::Agy), false, true, "1"));
    assert!(pane.is_waiting_input);

    // 5. Idle フック（ユーザーキャンセル等）で緑も黄もクリアされること
    assert!(pane.apply_ai_hook_event(AiHookState::Idle, Some(AiTool::Agy), false, true, "1"));
    assert!(!pane.is_actively_outputting(Instant::now()));
    assert!(!pane.is_waiting_input);

    // 6. AI 終了してシェルへ戻ると一般ルールへ復帰し、通常出力を検知できること
    assert!(pane.update_ai_tool_from_daemon(None));
    assert!(!pane.is_ai_hook_managed());
    pane.feed_pty_bytes(b"\r\nhello from shell\r\n$ ", false, true, "1");
    assert!(pane.is_actively_outputting(Instant::now()));
    assert!(pane.had_background_output);
    let after_shell_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(pane.update_waiting_state(after_shell_idle, false, Some("1")));
    assert!(pane.is_waiting_input);
}

#[test]
fn test_fast_path_does_not_extend_timestamp_on_repeated_invisible_redraws() {
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
    let old_ts = pane.last_output_at - chrono::TimeDelta::minutes(10);
    pane.last_output_at = old_ts;
    let initial_instant = Instant::now() - Duration::from_millis(10);
    pane.last_output_instant = Some(initial_instant);

    // 50ms 高速パス中に同一内容の不可視再描画が来ても時刻・last_output_instant を延長しないこと
    pane.feed_pty_bytes(b"\x1b[?25l\x1b[1;1Hdone 18:39\x1b[?25h", false, true, "1");
    assert_eq!(pane.last_output_at, old_ts);
    assert_eq!(pane.last_output_instant, Some(initial_instant));
    assert!(!pane.had_background_output);
}

#[test]
fn test_scrolled_up_viewport_still_updates_output_timestamp_and_waiting_state() {
    use ghostty_vt::terminal::ScrollViewport;

    let mut initial = String::new();
    for i in 1..=60 {
        initial.push_str(&format!("line{i:02}\r\n"));
    }
    let mut pane = TerminalPane::attach_from_replay(
        ReplayedPane {
            pane_id: 1,
            cols: 40,
            rows: 10,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: initial.into_bytes(),
        },
        DaemonClient::dummy_for_test(),
    )
    .unwrap();
    pane.created_at = Instant::now() - STARTUP_GRACE_PERIOD - Duration::from_secs(1);

    // 上へ 20 行スクロールバックして確認した状態にする
    pane.terminal.scroll_viewport(ScrollViewport::Delta(-20));
    pane.mark_screen_seen();
    pane.last_output_instant = None;

    // ビューポート外（アクティブ画面）に新規出力が届いても時刻・出力中・入力待ちが更新されること
    pane.feed_pty_bytes(b"new1\r\nnew2\r\n$ ", false, true, "1");
    assert!(pane.last_output_instant.is_some());
    assert!(pane.had_background_output);
    let after_idle = Instant::now() + WAITING_IDLE_THRESHOLD + Duration::from_millis(100);
    assert!(pane.update_waiting_state(after_idle, false, Some("1")));
    assert!(pane.is_waiting_input);
}
