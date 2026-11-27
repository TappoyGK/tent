mod client;
mod protocol;
mod server;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use ghostty_vt::{Terminal, snapshot};

use crate::terminal_pane::PaneId;

pub use client::{DaemonClient, stop_running_daemon};
pub use server::run_daemon_server;

/// デーモン通信プロトコルバージョン（スナップショット形式やメッセージ仕様変更時にインクリメント）
pub const DAEMON_PROTOCOL_VERSION: u32 = 4;

/// デフォルトのデーモン Unix ドメインソケットパス
pub fn default_socket_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("tent")
            .join("daemon.sock")
    } else {
        PathBuf::from("/tmp/tent-daemon.sock")
    }
}

/// 再接続時にデーモンから受け取る稼働中ペイン情報＋出力リプレイ
#[derive(Clone, Debug)]
pub struct ReplayedPane {
    pub pane_id: PaneId,
    pub cols: u16,
    pub rows: u16,
    pub cwd: PathBuf,
    pub replay_bytes: Vec<u8>,
}

/// デーモン → クライアント メッセージ
#[derive(Debug)]
pub enum DaemonEvent {
    /// 初回ハンドシェイク専用リプレイ（必ず HelloDone より前に届く）
    Replay(ReplayedPane),
    HelloDone {
        next_pane_id: PaneId,
        protocol_version: u32,
    },
    Output {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    Exited {
        pane_id: PaneId,
    },
    CwdList(Vec<(PaneId, PathBuf)>),
    /// 接続後の過負荷時に最新スナップショットへ追いつくための再同期専用イベント
    Resync(ReplayedPane),
}

/// 端末の完全なスナップショット（通常画面・代替画面・保存カーソル・スクロールバック・
/// 各種モード・未完了の VT パーサー入力を含む）をバイナリ列へエンコードする
pub fn encode_terminal_snapshot(terminal: &Terminal) -> Vec<u8> {
    match snapshot::encode(terminal) {
        Ok(bytes) => AsRef::<[u8]>::as_ref(&*bytes).to_vec(),
        Err(_) => Vec::new(),
    }
}

/// `encode_terminal_snapshot` で生成したバイナリスナップショットから `Terminal` を復元する
pub fn decode_terminal_snapshot(bytes: &[u8]) -> Option<Terminal> {
    if bytes.is_empty() {
        return None;
    }
    snapshot::Decoder::from_slice(bytes).decode().ok()
}
