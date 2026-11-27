mod client;
mod protocol;
mod server;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use ghostty_vt::{Terminal, snapshot};

use crate::terminal_pane::PaneId;

pub use client::{DaemonClient, send_hook_event, stop_running_daemon};
pub use server::run_daemon_server;

/// デーモン通信プロトコルバージョン（スナップショット形式やメッセージ仕様変更時にインクリメント）
pub const DAEMON_PROTOCOL_VERSION: u32 = 5;

/// 特別監視対象の AI エージェント種別
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AiTool {
    Claude = 1,
    Agy = 2,
    Codex = 3,
}

impl AiTool {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Claude),
            2 => Some(Self::Agy),
            3 => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn parse_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Self::Claude),
            "agy" | "antigravity" | "antigravity-cli" => Some(Self::Agy),
            "codex" | "codex-cli" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// AI フックから通知されるペイン状態
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AiHookState {
    /// AI が処理を開始・継続中（緑）
    Working = 1,
    /// AI が完了または確認・質問待ち（黄）
    Waiting = 2,
    /// キャンセル等で待機状態へ戻す（クリア）
    Idle = 3,
}

impl AiHookState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Working),
            2 => Some(Self::Waiting),
            3 => Some(Self::Idle),
            _ => None,
        }
    }

    pub fn parse_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "working" | "start" | "busy" => Some(Self::Working),
            "waiting" | "stop" | "done" | "attention" => Some(Self::Waiting),
            "idle" | "clear" | "cancel" => Some(Self::Idle),
            _ => None,
        }
    }
}

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
    /// AI フックから届いた明示的なペイン状態イベント
    HookEvent {
        pane_id: PaneId,
        state: AiHookState,
        tool: Option<AiTool>,
    },
    /// 各ペインで現在フォアグラウンド稼働中の AI ツール一覧
    AiToolList(Vec<(PaneId, Option<AiTool>)>),
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
