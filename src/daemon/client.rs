use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use crate::terminal_pane::PaneId;

use super::protocol::{ClientMsg, read_daemon_event, write_client_msg};
use super::{AiHookState, AiTool, DAEMON_PROTOCOL_VERSION, DaemonEvent, ReplayedPane};

/// GUI 側からデーモンへ非同期コマンドを送るハンドル
#[derive(Clone)]
pub struct DaemonClient {
    cmd_tx: Sender<ClientMsg>,
    server_version: u32,
}

/// 初回接続ハンドシェイクの結果
pub struct AttachResult {
    pub client: DaemonClient,
    pub replayed_panes: Vec<ReplayedPane>,
    pub next_pane_id: PaneId,
    /// 常駐デーモンが現行バイナリと異なるプロトコルバージョンの場合 true
    pub daemon_version_mismatch: bool,
    /// 接続先常駐デーモンの PID（メモリ使用量モニタリング用）
    pub daemon_pid: Option<i32>,
}

impl DaemonClient {
    /// デーモンに接続（未起動なら自動で `tent --daemon` をデタッチ起動）し、
    /// 既存ペインのリプレイを受信してからイベント受信スレッドを開始する。
    pub fn connect_or_spawn(
        sock_path: &Path,
        auto_spawn_daemon: bool,
        on_event: impl Fn(DaemonEvent) + Send + 'static,
    ) -> io::Result<AttachResult> {
        let stream = match UnixStream::connect(sock_path) {
            Ok(s) => s,
            Err(_) if auto_spawn_daemon => {
                spawn_detached_daemon(sock_path)?;
                let mut connected = None;
                for _ in 0..40 {
                    thread::sleep(Duration::from_millis(50));
                    if let Ok(s) = UnixStream::connect(sock_path) {
                        connected = Some(s);
                        break;
                    }
                }
                connected.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "failed to connect to tent daemon")
                })?
            }
            Err(e) => return Err(e),
        };

        let daemon_pid = peer_pid_of_stream(&stream);
        let mut reader = stream.try_clone()?;
        let mut writer = stream;

        // 同期ハンドシェイク: Hello -> Replay* -> HelloDone
        // 万一 HelloDone より前に通常イベントが届いても捨てずに保留し、ハンドシェイク完了後に順序通り適用する
        write_client_msg(&mut writer, &ClientMsg::Hello)?;
        let mut replayed_panes = Vec::new();
        let mut buffered_events = Vec::new();
        let next_pane_id;
        let server_version;
        loop {
            match read_daemon_event(&mut reader)? {
                DaemonEvent::Replay(rp) => replayed_panes.push(rp),
                DaemonEvent::HelloDone {
                    next_pane_id: nid,
                    protocol_version,
                } => {
                    next_pane_id = nid;
                    server_version = protocol_version;
                    break;
                }
                other => buffered_events.push(other),
            }
        }
        let daemon_version_mismatch = server_version != DAEMON_PROTOCOL_VERSION;
        if daemon_version_mismatch {
            eprintln!(
                "TenT: daemon protocol version mismatch (daemon=v{server_version}, client=v{DAEMON_PROTOCOL_VERSION}); restart daemon to apply daemon-side updates"
            );
        }

        // クライアント → デーモン非同期書き込みスレッド（UIスレッド非ブロック）
        let (cmd_tx, cmd_rx) = mpsc::channel::<ClientMsg>();
        thread::Builder::new()
            .name("daemon-client-w".to_string())
            .spawn(move || {
                while let Ok(msg) = cmd_rx.recv() {
                    if write_client_msg(&mut writer, &msg).is_err() {
                        break;
                    }
                }
            })?;

        // デーモン → クライアント非同期読み込みスレッド
        thread::Builder::new()
            .name("daemon-client-r".to_string())
            .spawn(move || {
                for ev in buffered_events {
                    on_event(normalize_post_handshake_event(ev, server_version));
                }
                while let Ok(ev) = read_daemon_event(&mut reader) {
                    on_event(normalize_post_handshake_event(ev, server_version));
                }
            })?;

        Ok(AttachResult {
            client: DaemonClient {
                cmd_tx,
                server_version,
            },
            replayed_panes,
            next_pane_id,
            daemon_version_mismatch,
            daemon_pid,
        })
    }

    pub fn spawn_pane(&self, pane_id: PaneId, cols: u16, rows: u16, cwd: &Path) {
        let _ = self.cmd_tx.send(ClientMsg::Spawn {
            pane_id,
            cols,
            rows,
            cwd: cwd.to_string_lossy().into_owned(),
        });
    }

    pub fn send_input(&self, pane_id: PaneId, data: Vec<u8>) {
        if !data.is_empty() {
            let _ = self.cmd_tx.send(ClientMsg::Input { pane_id, data });
        }
    }

    pub fn resize_pane(&self, pane_id: PaneId, cols: u16, rows: u16) {
        let _ = self.cmd_tx.send(ClientMsg::Resize {
            pane_id,
            cols,
            rows,
        });
    }

    pub fn kill_pane(&self, pane_id: PaneId) {
        let _ = self.cmd_tx.send(ClientMsg::Kill { pane_id });
    }

    pub fn query_cwds(&self) {
        let _ = self.cmd_tx.send(ClientMsg::QueryCwds);
    }

    pub fn supports_clear(&self) -> bool {
        self.server_version >= 4
    }

    pub fn clear_pane(&self, pane_id: PaneId) {
        if self.supports_clear() {
            let _ = self.cmd_tx.send(ClientMsg::Clear { pane_id });
        }
    }

    #[cfg(test)]
    pub fn dummy_for_test() -> Self {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        Self {
            cmd_tx,
            server_version: 0,
        }
    }
}

/// ハンドシェイク完了後に受信したイベントを正規化する。
/// 旧 v2 以前のデーモンは過負荷再同期にも `Replay` を送るため、`Resync` へ変換して互換性を維持する。
pub(super) fn normalize_post_handshake_event(ev: DaemonEvent, server_version: u32) -> DaemonEvent {
    if server_version < 3
        && let DaemonEvent::Replay(rp) = ev
    {
        return DaemonEvent::Resync(rp);
    }
    ev
}

fn peer_pid_of_stream(stream: &UnixStream) -> Option<i32> {
    let mut peer_pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            std::ptr::from_mut(&mut peer_pid).cast(),
            &raw mut len,
        )
    };
    (rc == 0 && peer_pid > 0).then_some(peer_pid)
}

/// 単発 CLI（`tent hook`）から常駐デーモンへ AI フック状態イベントを送信する。
/// デーモン未起動やソケット切断時は呼び出し元の AI フックを妨げないよう短時間でエラー返却する。
pub fn send_hook_event(
    sock_path: &Path,
    pane_id: PaneId,
    state: AiHookState,
    tool: Option<AiTool>,
) -> io::Result<()> {
    let mut stream = UnixStream::connect(sock_path)?;
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    write_client_msg(
        &mut stream,
        &ClientMsg::HookEvent {
            pane_id,
            state,
            tool,
        },
    )
}

/// 指定ソケットで待ち受けている常駐デーモンをピンポイントで停止する（明示的なアップグレード用）。
/// macOS の `LOCAL_PEERPID` で接続先デーモンの PID のみを取得し、`SIGTERM` 送信後にソケットを掃除する。
pub fn stop_running_daemon(sock_path: &Path) -> io::Result<()> {
    let stream = match UnixStream::connect(sock_path) {
        Ok(s) => s,
        Err(_) => {
            let _ = fs::remove_file(sock_path);
            return Ok(());
        }
    };

    let mut peer_pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            std::ptr::from_mut(&mut peer_pid).cast(),
            &raw mut len,
        )
    };
    drop(stream);

    if rc == 0 && peer_pid > 0 && peer_pid != std::process::id() as libc::pid_t {
        unsafe {
            let _ = libc::kill(peer_pid, libc::SIGTERM);
        }
        // デーモン終了を最大 500ms 待機
        for _ in 0..25 {
            thread::sleep(Duration::from_millis(20));
            let alive = unsafe { libc::kill(peer_pid, 0) } == 0;
            if !alive {
                break;
            }
        }
    }
    let _ = fs::remove_file(sock_path);
    Ok(())
}

fn spawn_detached_daemon(sock_path: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--daemon")
        .arg(sock_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // 親プロセス終了（Cmd+Q）でもデーモンと子シェルが生き残るよう新セッション化
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}
