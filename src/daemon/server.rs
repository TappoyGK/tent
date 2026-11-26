use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use ghostty_vt::{Event as VtEvent, Terminal, TerminalOptions};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::macos;
use crate::terminal_pane::PaneId;

use super::protocol::{ClientMsg, read_client_msg, write_daemon_event};
use super::{DAEMON_PROTOCOL_VERSION, DaemonEvent, ReplayedPane, encode_terminal_snapshot};

// =========================================================================
// デーモンサーバー実装 (`tent --daemon`)
// =========================================================================

/// `ghostty_vt::Terminal` は内部で `NonNull` を持つため `!Send` だが、
/// `DaemonState` の `Mutex` 下で排他的にのみ操作するため安全にスレッド間移動できる。
struct SendTerminal(Terminal);

// SAFETY: `SendTerminal` は常に `Mutex<DaemonState>` により排他制御されており、
// 複数スレッドから同時にアクセスされることはない。
unsafe impl Send for SendTerminal {}

impl std::ops::Deref for SendTerminal {
    type Target = Terminal;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SendTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

struct DaemonPane {
    pty_master: Box<dyn MasterPty + Send>,
    pty_write_tx: Sender<Vec<u8>>,
    child_pid: Option<i32>,
    cols: u16,
    rows: u16,
    cwd: PathBuf,
    terminal: SendTerminal,
}

impl DaemonPane {
    fn feed_output(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
        // デーモン側 VT の内部イベントを消化（PWD 変更があれば最新化）
        for ev in self.terminal.events() {
            if let VtEvent::PwdChanged(uri) = ev
                && let Some(path) = parse_file_uri(&uri)
            {
                self.cwd = path;
            }
        }
    }

    fn snapshot_vt_bytes(&self) -> Vec<u8> {
        encode_terminal_snapshot(&self.terminal)
    }

    fn current_cwd(&mut self) -> PathBuf {
        let pid = self.pty_master.process_group_leader().or(self.child_pid);
        if let Some(pid) = pid
            && let Some(path) = macos::get_pid_cwd(pid)
        {
            self.cwd = path;
        }
        self.cwd.clone()
    }
}

/// デーモン側端末で未完了の制御シーケンス／UTF-8 バイトを追跡するための上限（64KB）
const DAEMON_CONTINUATION_MAX_BYTES: usize = 64 * 1024;

/// デーモン側で端末状態を追跡する `Terminal` を生成する（未完了シーケンス追跡を有効化）
pub(super) fn new_daemon_terminal(cols: u16, rows: u16) -> Result<Terminal, ghostty_vt::Error> {
    let mut terminal = Terminal::new(TerminalOptions {
        cols: cols.max(10),
        rows: rows.max(4),
        max_scrollback: 1_000_000,
    })?;
    terminal.set_continuation_max_bytes(DAEMON_CONTINUATION_MAX_BYTES)?;
    Ok(terminal)
}

fn parse_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let path_part = &rest[slash..];
    let bytes = path_part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    Some(PathBuf::from(decoded))
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 1 クライアント向け送信キューの出力バイト上限（16MB）
/// 上限を超過する持続的な過負荷時は古い Output を捨てて最新スナップショットで再同期する
const DEFAULT_MAX_OUTBOX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// 連続する同一ペインの Output チャンクを結合する際の上限サイズ（256KB）
const COALESCE_CHUNK_LIMIT: usize = 256 * 1024;

struct OutboxInner {
    queue: VecDeque<DaemonEvent>,
    queued_output_bytes: usize,
    closed: bool,
}

/// ロック保持中にブロックせず、かつ持続的な大量出力でもメモリ上限を超えないクライアント送信キュー
#[derive(Clone)]
pub(super) struct ClientOutbox {
    inner: Arc<(Mutex<OutboxInner>, Condvar)>,
    max_output_bytes: usize,
}

impl ClientOutbox {
    pub(super) fn new(max_output_bytes: usize) -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(OutboxInner {
                    queue: VecDeque::new(),
                    queued_output_bytes: 0,
                    closed: false,
                }),
                Condvar::new(),
            )),
            max_output_bytes,
        }
    }

    /// 制御・終了・スナップショット系イベントを欠落なく積む
    pub(super) fn push_event(&self, ev: DaemonEvent) -> bool {
        let (lock, cvar) = &*self.inner;
        let Ok(mut inner) = lock.lock() else {
            return false;
        };
        if inner.closed {
            return false;
        }
        if let DaemonEvent::Output { ref data, .. } = ev {
            inner.queued_output_bytes += data.len();
        }
        inner.queue.push_back(ev);
        cvar.notify_one();
        true
    }

    /// 出力チャンクを積む（連続チャンクは結合し、上限超過時は古い出力を捨ててスナップショット再同期へ切替）
    pub(super) fn push_output_or_resync(
        &self,
        pane_id: PaneId,
        data: Vec<u8>,
        make_snapshot: impl FnOnce() -> Option<ReplayedPane>,
    ) -> bool {
        let (lock, cvar) = &*self.inner;
        let Ok(mut inner) = lock.lock() else {
            return false;
        };
        if inner.closed {
            return false;
        }

        if inner.queued_output_bytes + data.len() > self.max_output_bytes {
            // 当該ペインの溜まった古い Output / Resync のみを除去（初回ハンドシェイク用 Replay は絶対に触らない）
            let mut removed_bytes = 0usize;
            inner.queue.retain(|ev| match ev {
                DaemonEvent::Output {
                    pane_id: pid,
                    data: d,
                } if *pid == pane_id => {
                    removed_bytes += d.len();
                    false
                }
                DaemonEvent::Resync(rp) if rp.pane_id == pane_id => false,
                _ => true,
            });
            inner.queued_output_bytes = inner.queued_output_bytes.saturating_sub(removed_bytes);
            if let Some(rp) = make_snapshot() {
                inner.queue.push_back(DaemonEvent::Resync(rp));
            }
            cvar.notify_one();
            return true;
        }

        inner.queued_output_bytes += data.len();
        if let Some(DaemonEvent::Output {
            pane_id: last_pid,
            data: last_data,
        }) = inner.queue.back_mut()
            && *last_pid == pane_id
            && last_data.len() + data.len() <= COALESCE_CHUNK_LIMIT
        {
            last_data.extend_from_slice(&data);
        } else {
            inner.queue.push_back(DaemonEvent::Output { pane_id, data });
        }
        cvar.notify_one();
        true
    }

    pub(super) fn recv(&self) -> Option<DaemonEvent> {
        let (lock, cvar) = &*self.inner;
        let mut inner = lock.lock().ok()?;
        loop {
            if let Some(ev) = inner.queue.pop_front() {
                if let DaemonEvent::Output { ref data, .. } = ev {
                    inner.queued_output_bytes =
                        inner.queued_output_bytes.saturating_sub(data.len());
                }
                return Some(ev);
            }
            if inner.closed {
                return None;
            }
            inner = cvar.wait(inner).ok()?;
        }
    }

    pub(super) fn close(&self) {
        let (lock, cvar) = &*self.inner;
        if let Ok(mut inner) = lock.lock() {
            inner.closed = true;
            cvar.notify_all();
        }
    }

    #[cfg(test)]
    pub(super) fn drain_all(&self) -> Vec<DaemonEvent> {
        let (lock, _) = &*self.inner;
        let mut inner = lock.lock().unwrap();
        inner.queued_output_bytes = 0;
        inner.queue.drain(..).collect()
    }
}

pub(super) struct DaemonState {
    panes: HashMap<PaneId, DaemonPane>,
    next_pane_id: PaneId,
    pub(super) client_outbox: Option<ClientOutbox>,
}

impl DaemonState {
    pub(super) fn new() -> Self {
        Self {
            panes: HashMap::new(),
            next_pane_id: 1,
            client_outbox: None,
        }
    }

    pub(super) fn emit(&mut self, ev: DaemonEvent) {
        if let Some(outbox) = &self.client_outbox
            && !outbox.push_event(ev)
        {
            self.client_outbox = None;
        }
    }

    pub(super) fn emit_output(&mut self, pane_id: PaneId, data: Vec<u8>) {
        let Some(outbox) = self.client_outbox.clone() else {
            return;
        };
        let ok = outbox.push_output_or_resync(pane_id, data, || {
            self.panes.get_mut(&pane_id).map(|dp| ReplayedPane {
                pane_id,
                cols: dp.cols,
                rows: dp.rows,
                cwd: dp.current_cwd(),
                replay_bytes: dp.snapshot_vt_bytes(),
            })
        });
        if !ok {
            self.client_outbox = None;
        }
    }
}

/// デーモンサーバーを起動し、Unix ソケットでクライアント接続を待ち受ける
pub fn run_daemon_server(sock_path: &Path) -> io::Result<()> {
    if let Some(parent) = sock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    // すでに生きたデーモンがいれば二重起動せず終了
    if sock_path.exists() {
        if UnixStream::connect(sock_path).is_ok() {
            return Ok(());
        }
        let _ = fs::remove_file(sock_path);
    }

    let listener = UnixListener::bind(sock_path)?;
    // 他ユーザーから PTY ソケットへ接続できないよう所有者限定（0600）に設定
    let _ = fs::set_permissions(sock_path, fs::Permissions::from_mode(0o600));
    let state = Arc::new(Mutex::new(DaemonState::new()));

    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let state_clone = Arc::clone(&state);
        thread::spawn(move || {
            handle_client_connection(stream, state_clone);
        });
    }
    Ok(())
}

fn handle_client_connection(mut reader_stream: UnixStream, state: Arc<Mutex<DaemonState>>) {
    let Ok(mut writer_stream) = reader_stream.try_clone() else {
        return;
    };

    // ロック保持中にブロックせず、Output / Exited を欠落させない有界・コアレッシング対応キュー
    let outbox = ClientOutbox::new(DEFAULT_MAX_OUTBOX_OUTPUT_BYTES);
    let outbox_writer = outbox.clone();

    // デーモン → クライアント書き込みスレッド
    let writer_handle = thread::spawn(move || {
        while let Some(ev) = outbox_writer.recv() {
            if write_daemon_event(&mut writer_stream, &ev).is_err() {
                outbox_writer.close();
                break;
            }
        }
    });

    // クライアント → デーモン読み込みループ
    while let Ok(msg) = read_client_msg(&mut reader_stream) {
        match msg {
            ClientMsg::Hello => {
                // ロック保持中にスナップショット作成＋Replay/HelloDone 投入＋client_outbox 登録を
                // アトミックに行うことで、ハンドシェイク中の Output/Exited の欠落・二重適用を完全に防ぐ
                if let Ok(mut st) = state.lock() {
                    let mut replays = Vec::with_capacity(st.panes.len());
                    for (&pid, dp) in st.panes.iter_mut() {
                        let cwd = dp.current_cwd();
                        let bytes = dp.snapshot_vt_bytes();
                        replays.push(ReplayedPane {
                            pane_id: pid,
                            cols: dp.cols,
                            rows: dp.rows,
                            cwd,
                            replay_bytes: bytes,
                        });
                    }
                    let next_id = st.next_pane_id;
                    for rp in replays {
                        outbox.push_event(DaemonEvent::Replay(rp));
                    }
                    outbox.push_event(DaemonEvent::HelloDone {
                        next_pane_id: next_id,
                        protocol_version: DAEMON_PROTOCOL_VERSION,
                    });
                    st.client_outbox = Some(outbox.clone());
                }
            }
            ClientMsg::Spawn {
                pane_id,
                cols,
                rows,
                cwd,
            } => {
                spawn_daemon_pane(Arc::clone(&state), pane_id, cols, rows, PathBuf::from(cwd));
            }
            ClientMsg::Input { pane_id, data } => {
                if let Ok(st) = state.lock()
                    && let Some(dp) = st.panes.get(&pane_id)
                {
                    let _ = dp.pty_write_tx.send(data);
                }
            }
            ClientMsg::Resize {
                pane_id,
                cols,
                rows,
            } => {
                if let Ok(mut st) = state.lock()
                    && let Some(dp) = st.panes.get_mut(&pane_id)
                {
                    dp.cols = cols;
                    dp.rows = rows;
                    let _ = dp.terminal.resize(cols, rows, 0, 0);
                    let _ = dp.pty_master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
            ClientMsg::Kill { pane_id } => {
                let removed = {
                    let mut st = state.lock().unwrap();
                    st.panes.remove(&pane_id)
                };
                if let Some(dp) = removed {
                    // フォアグラウンドの子プロセス（sleep や TUI 等）も含めプロセスグループ全体へ SIGHUP を送る
                    if let Some(pid) = dp.child_pid {
                        unsafe {
                            let _ = libc::kill(-pid, libc::SIGHUP);
                            let _ = libc::kill(pid, libc::SIGHUP);
                        }
                    }
                    drop(dp);
                }
            }
            ClientMsg::QueryCwds => {
                let cwds = {
                    let mut st = state.lock().unwrap();
                    st.panes
                        .iter_mut()
                        .map(|(&pid, dp)| (pid, dp.current_cwd()))
                        .collect::<Vec<_>>()
                };
                outbox.push_event(DaemonEvent::CwdList(cwds));
            }
            ClientMsg::Clear { pane_id } => {
                const CLEAR_SEQ: &[u8] = b"\x1b[3J\x1b[2J\x1b[H";
                if let Ok(mut st) = state.lock() {
                    let cleared = if let Some(dp) = st.panes.get_mut(&pane_id) {
                        dp.terminal.vt_write(CLEAR_SEQ);
                        true
                    } else {
                        false
                    };
                    if cleared {
                        st.emit_output(pane_id, CLEAR_SEQ.to_vec());
                    }
                }
            }
        }
    }

    outbox.close();
    let _ = writer_handle.join();
}

fn spawn_daemon_pane(
    state: Arc<Mutex<DaemonState>>,
    pane_id: PaneId,
    cols: u16,
    rows: u16,
    cwd: PathBuf,
) {
    let pty_system = native_pty_system();
    let Ok(pair) = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        return;
    };

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");
    if cwd.is_dir() {
        cmd.cwd(&cwd);
    } else if let Some(home) = std::env::var_os("HOME") {
        cmd.cwd(home);
    }
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");
    cmd.env_remove("TERM_PROGRAM");
    cmd.env_remove("TERM_PROGRAM_VERSION");
    if std::env::var_os("LANG").is_none() {
        cmd.env("LANG", "ja_JP.UTF-8");
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if let Ok(terminfo_dir) = ghostty_vt::terminfo::dir() {
        cmd.env("TERMINFO", terminfo_dir);
        cmd.env("TERM", "xterm-ghostty");
    }
    if let Ok(res_dir) = ghostty_vt::shell_integration::resources_dir() {
        cmd.env("GHOSTTY_RESOURCES_DIR", res_dir);
    }

    let Ok(mut child) = pair.slave.spawn_command(cmd) else {
        return;
    };
    let child_pid = child.process_id().map(|p| p as i32);
    let Ok(mut reader) = pair.master.try_clone_reader() else {
        return;
    };
    let Ok(mut writer) = pair.master.take_writer() else {
        return;
    };

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
    let _ = thread::Builder::new()
        .name(format!("daemon-pty-w-{pane_id}"))
        .spawn(move || {
            while let Ok(bytes) = write_rx.recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });

    let Ok(terminal) = new_daemon_terminal(cols, rows) else {
        return;
    };

    {
        let mut st = state.lock().unwrap();
        st.next_pane_id = st.next_pane_id.max(pane_id + 1);
        st.panes.insert(
            pane_id,
            DaemonPane {
                pty_master: pair.master,
                pty_write_tx: write_tx,
                child_pid,
                cols,
                rows,
                cwd,
                terminal: SendTerminal(terminal),
            },
        );
    }

    // 子プロセス wait 専用スレッド（ゾンビ防止＆終了通知）
    let wait_state = Arc::clone(&state);
    let _ = thread::Builder::new()
        .name(format!("daemon-pty-wait-{pane_id}"))
        .spawn(move || {
            let _ = child.wait();
            if let Ok(mut st) = wait_state.lock()
                && st.panes.remove(&pane_id).is_some()
            {
                st.emit(DaemonEvent::Exited { pane_id });
            }
        });

    // PTY 読み込みスレッド
    let read_state = Arc::clone(&state);
    let _ = thread::Builder::new()
        .name(format!("daemon-pty-r-{pane_id}"))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        if let Ok(mut st) = read_state.lock()
                            && st.panes.remove(&pane_id).is_some()
                        {
                            st.emit(DaemonEvent::Exited { pane_id });
                        }
                        break;
                    }
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        if let Ok(mut st) = read_state.lock() {
                            if let Some(dp) = st.panes.get_mut(&pane_id) {
                                dp.feed_output(&data);
                            }
                            st.emit_output(pane_id, data);
                        }
                    }
                }
            }
        });
}
