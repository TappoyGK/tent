use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ghostty_vt::Terminal;

use super::client::normalize_post_handshake_event;
use super::protocol::{read_daemon_event, write_daemon_event};
use super::server::{ClientOutbox, DaemonState, new_daemon_terminal};
use super::{
    DAEMON_PROTOCOL_VERSION, DaemonClient, DaemonEvent, ReplayedPane, decode_terminal_snapshot,
    encode_terminal_snapshot, run_daemon_server,
};

#[test]
fn test_daemon_spawn_detach_and_reconnect_replay() {
    let dir = std::env::temp_dir().join(format!("tent-daemon-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("test.sock");
    let sock_server = sock.clone();

    thread::spawn(move || {
        let _ = run_daemon_server(&sock_server);
    });

    // サーバー起動待ち
    let mut attached = None;
    for _ in 0..30 {
        thread::sleep(Duration::from_millis(30));
        let (tx, _rx) = mpsc::channel();
        if let Ok(res) = DaemonClient::connect_or_spawn(&sock, false, move |ev| {
            let _ = tx.send(ev);
        }) {
            attached = Some(res);
            break;
        }
    }
    let first = attached.expect("should connect to test daemon");
    assert!(first.replayed_panes.is_empty());
    assert!(!first.daemon_version_mismatch);
    let sock_mode = fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(sock_mode, 0o600, "daemon socket should be owner-only 0600");

    // ペイン起動＆コマンド実行
    first.client.spawn_pane(10, 80, 24, &dir);
    thread::sleep(Duration::from_millis(200));
    first
        .client
        .send_input(10, b"echo TENT_RECONNECT_OK\r".to_vec());
    thread::sleep(Duration::from_millis(300));

    // 最初のクライアントを切断（デーモンとシェルは生き残る）
    drop(first.client);
    thread::sleep(Duration::from_millis(100));

    // 2つ目のクライアントで再接続 → リプレイで出力が復元されることを確認
    let (tx2, _rx2) = mpsc::channel();
    let second = DaemonClient::connect_or_spawn(&sock, false, move |ev| {
        let _ = tx2.send(ev);
    })
    .expect("should reconnect to test daemon");

    assert_eq!(second.replayed_panes.len(), 1);
    assert_eq!(second.replayed_panes[0].pane_id, 10);
    let restored = decode_terminal_snapshot(&second.replayed_panes[0].replay_bytes)
        .expect("replay bytes should decode as a valid terminal snapshot");
    let text = plain_text(&restored);
    assert!(
        text.contains("TENT_RECONNECT_OK"),
        "replay did not contain marker: {text:?}"
    );

    // 後始末
    second.client.kill_pane(10);
    thread::sleep(Duration::from_millis(100));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_daemon_emit_does_not_drop_output_or_exited_under_burst() {
    let mut st = DaemonState::new();
    let outbox = ClientOutbox::new(16 * 1024 * 1024);
    st.client_outbox = Some(outbox.clone());

    for i in 0..1000u64 {
        st.emit(DaemonEvent::Output {
            pane_id: 1,
            data: format!("chunk-{i}").into_bytes(),
        });
    }
    st.emit(DaemonEvent::Exited { pane_id: 1 });

    let received = outbox.drain_all();
    assert_eq!(received.len(), 1001);
    assert!(matches!(
        received.last(),
        Some(DaemonEvent::Exited { pane_id: 1 })
    ));
}

#[test]
fn test_outbox_coalesces_and_resyncs_when_over_limit() {
    let outbox = ClientOutbox::new(64);

    // 上限内なら連続する同一ペインの出力は 1 フレームに結合される
    assert!(outbox.push_output_or_resync(1, b"hello ".to_vec(), || None));
    assert!(outbox.push_output_or_resync(1, b"world".to_vec(), || None));
    let evs = outbox.drain_all();
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        DaemonEvent::Output { pane_id, data } => {
            assert_eq!(*pane_id, 1);
            assert_eq!(data, b"hello world");
        }
        other => panic!("expected coalesced Output, got {other:?}"),
    }

    // 上限（64B）を超過したら古い Output を捨てて最新スナップショット Replay へ切り替わる
    assert!(outbox.push_output_or_resync(1, vec![b'A'; 40], || None));
    assert!(outbox.push_output_or_resync(1, vec![b'B'; 40], || {
        Some(ReplayedPane {
            pane_id: 1,
            cols: 80,
            rows: 24,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"SNAP".to_vec(),
        })
    }));
    // Exited イベントは過負荷時でも絶対に捨てられない
    assert!(outbox.push_event(DaemonEvent::Exited { pane_id: 1 }));

    let evs = outbox.drain_all();
    assert_eq!(evs.len(), 2);
    assert!(matches!(
        &evs[0],
        DaemonEvent::Resync(rp) if rp.pane_id == 1 && rp.replay_bytes == b"SNAP"
    ));
    assert!(matches!(&evs[1], DaemonEvent::Exited { pane_id: 1 }));
}

#[test]
fn test_outbox_overload_never_moves_initial_replay_past_hello_done() {
    let outbox = ClientOutbox::new(64);
    let initial = ReplayedPane {
        pane_id: 1,
        cols: 80,
        rows: 24,
        cwd: PathBuf::from("/tmp"),
        replay_bytes: b"INIT".to_vec(),
    };
    assert!(outbox.push_event(DaemonEvent::Replay(initial)));
    assert!(outbox.push_event(DaemonEvent::HelloDone {
        next_pane_id: 2,
        protocol_version: DAEMON_PROTOCOL_VERSION,
    }));
    // HelloDone の後ろに大量出力が積まれて過負荷再同期が発動しても、
    // 初回 Replay は HelloDone より前にそのまま残り、再同期は Resync として HelloDone の後ろに積まれること
    assert!(outbox.push_output_or_resync(1, vec![b'A'; 40], || None));
    assert!(outbox.push_output_or_resync(1, vec![b'B'; 40], || {
        Some(ReplayedPane {
            pane_id: 1,
            cols: 80,
            rows: 24,
            cwd: PathBuf::from("/tmp"),
            replay_bytes: b"LATEST".to_vec(),
        })
    }));

    let evs = outbox.drain_all();
    assert_eq!(evs.len(), 3);
    assert!(matches!(
        &evs[0],
        DaemonEvent::Replay(rp) if rp.pane_id == 1 && rp.replay_bytes == b"INIT"
    ));
    assert!(matches!(
        &evs[1],
        DaemonEvent::HelloDone {
            next_pane_id: 2,
            ..
        }
    ));
    assert!(matches!(
        &evs[2],
        DaemonEvent::Resync(rp) if rp.pane_id == 1 && rp.replay_bytes == b"LATEST"
    ));
}

#[test]
fn test_protocol_hello_done_backward_compatible_version() {
    // 現行 v2 フレームの往復
    let mut buf = Vec::new();
    write_daemon_event(
        &mut buf,
        &DaemonEvent::HelloDone {
            next_pane_id: 42,
            protocol_version: DAEMON_PROTOCOL_VERSION,
        },
    )
    .unwrap();
    let decoded = read_daemon_event(&mut Cursor::new(&buf)).unwrap();
    assert!(matches!(
        decoded,
        DaemonEvent::HelloDone {
            next_pane_id: 42,
            protocol_version
        } if protocol_version == DAEMON_PROTOCOL_VERSION
    ));

    // 旧デーモン（8バイトのみ）は protocol_version = 1 として安全に読み取れること
    let mut legacy = Vec::new();
    legacy.push(0x82u8); // TAG_HELLO_DONE
    legacy.extend_from_slice(&8u32.to_le_bytes());
    legacy.extend_from_slice(&7u64.to_le_bytes());
    let decoded_legacy = read_daemon_event(&mut Cursor::new(&legacy)).unwrap();
    assert!(matches!(
        decoded_legacy,
        DaemonEvent::HelloDone {
            next_pane_id: 7,
            protocol_version: 1
        }
    ));
}

fn plain_text(terminal: &Terminal) -> String {
    use ghostty_vt::fmt::{Format, Formatter, FormatterOptions};
    let opts = FormatterOptions::new().with_format(Format::Plain);
    let bytes = Formatter::new(terminal, opts)
        .and_then(|mut f| f.format_alloc())
        .expect("plain format should succeed");
    String::from_utf8_lossy(AsRef::<[u8]>::as_ref(&*bytes)).into_owned()
}

#[test]
fn test_snapshot_vt_preserves_modes_after_256kb_output() {
    use ghostty_vt::terminal::Mode;

    let mut src = new_daemon_terminal(80, 24).unwrap();

    // 代替画面・ブラケットペースト・SGRマウスを有効化したあと 300KB 以上出力
    src.vt_write(b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
    let chunk = b"0123456789abcdef0123456789abcdef\r\n";
    for _ in 0..10_000 {
        src.vt_write(chunk);
    }
    src.vt_write(b"FINAL_MARKER");

    assert!(src.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false));
    assert!(src.mode(Mode::BRACKETED_PASTE).unwrap_or(false));
    assert!(src.mode(Mode::NORMAL_MOUSE).unwrap_or(false));
    assert!(src.mode(Mode::SGR_MOUSE).unwrap_or(false));

    let snap = encode_terminal_snapshot(&src);
    let dst = decode_terminal_snapshot(&snap).expect("snapshot should decode");

    assert!(dst.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false));
    assert!(dst.mode(Mode::BRACKETED_PASTE).unwrap_or(false));
    assert!(dst.mode(Mode::NORMAL_MOUSE).unwrap_or(false));
    assert!(dst.mode(Mode::SGR_MOUSE).unwrap_or(false));
    assert!(plain_text(&dst).contains("FINAL_MARKER"));
}

#[test]
fn test_snapshot_preserves_primary_screen_while_in_alt_screen() {
    let mut src = new_daemon_terminal(80, 24).unwrap();
    src.vt_write(b"PRIMARY_MARKER\r\n");
    src.vt_write(b"\x1b[?1049hALT_TUI_MARKER");

    let snap = encode_terminal_snapshot(&src);
    let mut dst = decode_terminal_snapshot(&snap).expect("snapshot should decode");

    // 復元直後は代替画面の内容が見える
    assert!(plain_text(&dst).contains("ALT_TUI_MARKER"));

    // TUI 終了（通常画面へ復帰）後、元の通常画面の内容が残っていること
    src.vt_write(b"\x1b[?1049l");
    dst.vt_write(b"\x1b[?1049l");
    assert!(plain_text(&src).contains("PRIMARY_MARKER"));
    assert!(
        plain_text(&dst).contains("PRIMARY_MARKER"),
        "primary screen marker lost after exiting alt screen: {:?}",
        plain_text(&dst)
    );
}

#[test]
fn test_snapshot_preserves_incomplete_escape_sequence() {
    use ghostty_vt::terminal::Mode;

    let mut src = new_daemon_terminal(80, 24).unwrap();
    // CSI シーケンスの途中でスナップショットを取得
    src.vt_write(b"\x1b[?2004");
    assert!(!src.mode(Mode::BRACKETED_PASTE).unwrap_or(false));

    let snap = encode_terminal_snapshot(&src);
    let mut dst = decode_terminal_snapshot(&snap).expect("snapshot should decode");

    // 残りの 1 バイトを双方へ投入すると、どちらもブラケットペーストが有効になること
    src.vt_write(b"h");
    dst.vt_write(b"h");
    assert!(src.mode(Mode::BRACKETED_PASTE).unwrap_or(false));
    assert!(dst.mode(Mode::BRACKETED_PASTE).unwrap_or(false));
    assert!(
        !plain_text(&dst).contains('h'),
        "trailing sequence byte leaked as text: {:?}",
        plain_text(&dst)
    );
}

#[test]
fn test_normalize_post_handshake_event_converts_legacy_replay_to_resync() {
    let rp = ReplayedPane {
        pane_id: 5,
        cols: 80,
        rows: 24,
        cwd: PathBuf::from("/tmp"),
        replay_bytes: b"SNAP".to_vec(),
    };
    // 旧 v2 デーモンからのハンドシェイク後 Replay は Resync に変換される
    let converted = normalize_post_handshake_event(DaemonEvent::Replay(rp.clone()), 2);
    assert!(matches!(
        converted,
        DaemonEvent::Resync(r) if r.pane_id == 5 && r.replay_bytes == b"SNAP"
    ));

    // 現行 v3 デーモンからの Replay はそのまま（Resync へ誤変換しない）
    let kept = normalize_post_handshake_event(DaemonEvent::Replay(rp), 3);
    assert!(matches!(kept, DaemonEvent::Replay(r) if r.pane_id == 5));
}

#[test]
fn test_build_shell_integration_launch_for_zsh_bash_fish() {
    use super::server::{build_shell_integration_launch, detect_bash_major_version};

    // macOS 標準の /bin/bash が存在する環境ではロケール非依存でバージョン取得できること
    if std::path::Path::new("/bin/bash").is_file() {
        assert!(detect_bash_major_version("/bin/bash").is_some());
    }

    let res_dir = ghostty_vt::shell_integration::resources_dir().unwrap();

    // zsh: `-l` のまま既存 ZDOTDIR を退避しつつ shell-integration/zsh を注入
    let zsh = build_shell_integration_launch(
        "/bin/zsh",
        &res_dir,
        Some("/custom/zdot"),
        None,
        None,
        None,
    );
    assert_eq!(zsh.args, vec!["-l"]);
    assert!(
        zsh.envs
            .iter()
            .any(|(k, v)| k == "GHOSTTY_ZSH_ZDOTDIR" && v == "/custom/zdot")
    );
    assert!(
        zsh.envs
            .iter()
            .any(|(k, v)| k == "ZDOTDIR" && v.ends_with("shell-integration/zsh"))
    );

    // bash 5.x: `--posix` が `-l` より前に配置され、GHOSTTY_BASH_INJECT=1 + 既存 ENV 退避 + ENV=ghostty.bash
    let bash5 = build_shell_integration_launch(
        "/opt/homebrew/bin/bash",
        &res_dir,
        None,
        Some("/my/env"),
        None,
        Some(5),
    );
    assert_eq!(bash5.args, vec!["--posix", "-l"]);
    assert!(
        bash5
            .envs
            .iter()
            .any(|(k, v)| k == "GHOSTTY_BASH_INJECT" && v == "1")
    );
    assert!(
        bash5
            .envs
            .iter()
            .any(|(k, v)| k == "GHOSTTY_BASH_ENV" && v == "/my/env")
    );
    assert!(
        bash5
            .envs
            .iter()
            .any(|(k, v)| k == "ENV" && v.ends_with("shell-integration/bash/ghostty.bash"))
    );

    // bash 3.2 (macOS 標準): 未対応のため注入せず通常の `-l` のみで起動すること
    let bash3 =
        build_shell_integration_launch("/bin/bash", &res_dir, None, Some("/my/env"), None, Some(3));
    assert_eq!(bash3.args, vec!["-l"]);
    assert!(!bash3.envs.iter().any(|(k, _)| k == "GHOSTTY_BASH_INJECT"));
    assert!(!bash3.envs.iter().any(|(k, _)| k == "ENV"));

    // fish: `-l` のまま XDG_DATA_DIRS の先頭に shell-integration/fish を連結
    let fish = build_shell_integration_launch(
        "/opt/homebrew/bin/fish",
        &res_dir,
        None,
        None,
        Some("/usr/share"),
        None,
    );
    assert_eq!(fish.args, vec!["-l"]);
    assert!(fish.envs.iter().any(|(k, v)| {
        k == "XDG_DATA_DIRS" && v.contains("shell-integration/fish:") && v.ends_with(":/usr/share")
    }));
}
