mod selection;
mod snapshot;
#[cfg(test)]
mod tests;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use ghostty_vt::paste;
use ghostty_vt::render::{CellIterator, RowIterator};
pub use ghostty_vt::screen::TrackedGridRef;
use ghostty_vt::style::RgbColor;
use ghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace};
use ghostty_vt::{Event as VtEvent, RenderState, Terminal, TerminalOptions};

use crate::daemon::{DaemonClient, ReplayedPane};
use crate::macos;

pub use selection::UrlMatch;
use selection::{
    extract_selection_from_screen, extract_selection_from_terminal, find_url_at_cell,
    line_bounds_at_screen, screen_selection_to_viewport, viewport_top_screen_row,
    word_bounds_at_screen,
};
pub use snapshot::{MainScreenSnapshot, MinimapSnapshot};
pub type PaneId = u64;

const NOTIFICATION_COOLDOWN: Duration = Duration::from_secs(1);
const STARTUP_GRACE_PERIOD: Duration = Duration::from_secs(3);
const RESIZE_GRACE_PERIOD: Duration = Duration::from_secs(1);
const WAITING_IDLE_THRESHOLD: Duration = Duration::from_secs(4);
/// 裏スロットでの完了・入力待ち OS 通知を出すための最小実行時間（短時間ノイズ抑制）
const BACKGROUND_NOTIFY_MIN_DURATION: Duration = Duration::from_secs(5);
/// DEC Mode 2026 (Synchronized Output) で描画ホールドを許可する最大時間（固まり防止）
const SYNC_OUTPUT_MAX_HOLD: Duration = Duration::from_secs(1);

/// イベントループへのカスタム通知
#[derive(Debug, Clone)]
pub enum AppEvent {
    DaemonReady,
    Shortcut(crate::key_map::AppShortcut),
    WebViewClicked {
        px: f32,
        py: f32,
    },
    WebViewHovered {
        px: f32,
        py: f32,
    },
    BrowserUrlChanged {
        pane_id: PaneId,
        url: String,
    },
    BrowserTitleChanged {
        pane_id: PaneId,
        title: String,
    },
    BrowserSnapshot {
        pane_id: PaneId,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    BrowserAudioState {
        pane_id: PaneId,
        is_playing: bool,
    },
    BrowserOpenNewWindow {
        pane_id: PaneId,
        url: String,
    },
    BrowserSwipePreview {
        dir: i8,
        progress: f32,
        ready: bool,
    },
    DragHover {
        px: f32,
        py: f32,
    },
    DroppedFiles {
        paths: Vec<PathBuf>,
        pos: Option<(f32, f32)>,
    },
}

/// 1つのターミナルペイン（libghostty-vt + デーモンクライアント接続）
pub struct TerminalPane {
    pub id: PaneId,
    pub terminal: Terminal,
    render_state: RenderState,
    row_iter: RowIterator,
    cell_iter: CellIterator,
    daemon: DaemonClient,
    pub cols: u16,
    pub rows: u16,
    last_cell_px: (u32, u32),
    pub cwd: PathBuf,
    pub folder_name: String,
    pub last_output_at: DateTime<Local>,
    pub last_output_instant: Option<Instant>,
    created_at: Instant,
    last_resize_instant: Option<Instant>,
    sync_hold_since: Option<Instant>,
    pub has_notification: bool,
    pub had_background_output: bool,
    /// 裏スロットで実行中インジケータ（`esc to interrupt` 等）を表示しながら出力が続いている開始時刻
    pub work_indicator_started_at: Option<Instant>,
    /// OSC 133;C で明示的に開始されたコマンドの開始時刻（133;D で消費）
    pub osc133_job_started_at: Option<Instant>,
    /// チャンク境界を跨ぐ OSC 133 マーカー検出用の直近末尾バッファ（最大 6 バイト）
    osc133_tail: [u8; 6],
    osc133_tail_len: u8,
    pub is_waiting_input: bool,
    last_seen_screen_hash: u64,
    last_notification_sent: Option<Instant>,
    pub minimap: MinimapSnapshot,
    minimap_dirty: bool,
    /// 描画キャッシュ無効化用の世代カウンタ
    pub generation: u64,
}

impl TerminalPane {
    fn apply_default_colors(terminal: &mut Terminal) {
        let _ = terminal.set_default_bg_color(Some(RgbColor {
            r: 0x16,
            g: 0x18,
            b: 0x1e,
        }));
        let _ = terminal.set_default_fg_color(Some(RgbColor {
            r: 0xd8,
            g: 0xde,
            b: 0xe9,
        }));
        let _ = terminal.set_default_cursor_color(Some(RgbColor {
            r: 0x88,
            g: 0xc0,
            b: 0xd0,
        }));
    }

    fn new_vt(cols: u16, rows: u16) -> Result<Terminal, Box<dyn std::error::Error>> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: cols.max(10),
            rows: rows.max(4),
            ..Default::default()
        })?;
        Self::apply_default_colors(&mut terminal);
        Ok(terminal)
    }

    /// デーモン上で新しいシェルを起動してペインを作成する
    pub fn spawn_via_daemon(
        pane_id: PaneId,
        cols: u16,
        rows: u16,
        initial_cwd: PathBuf,
        daemon: DaemonClient,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let cols = cols.max(10);
        let rows = rows.max(4);
        daemon.spawn_pane(pane_id, cols, rows, &initial_cwd);

        let terminal = Self::new_vt(cols, rows)?;
        let folder_name = extract_folder_name(&initial_cwd);

        Ok(Self {
            id: pane_id,
            terminal,
            render_state: RenderState::new()?,
            row_iter: RowIterator::new()?,
            cell_iter: CellIterator::new()?,
            daemon,
            cols,
            rows,
            last_cell_px: (0, 0),
            cwd: initial_cwd,
            folder_name,
            last_output_at: Local::now(),
            last_output_instant: None,
            created_at: Instant::now(),
            last_resize_instant: None,
            sync_hold_since: None,
            has_notification: false,
            had_background_output: false,
            work_indicator_started_at: None,
            osc133_job_started_at: None,
            osc133_tail: [0; 6],
            osc133_tail_len: 0,
            is_waiting_input: false,
            last_seen_screen_hash: 0,
            last_notification_sent: None,
            minimap: MinimapSnapshot {
                cols,
                rows,
                ..Default::default()
            },
            minimap_dirty: true,
            generation: 1,
        })
    }

    /// デーモンに生きている既存ペインへ再接続し、スナップショット（または互換 VT リプレイ）を静かに復元する
    pub fn attach_from_replay(
        replayed: ReplayedPane,
        daemon: DaemonClient,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let cols = replayed.cols.max(10);
        let rows = replayed.rows.max(4);
        let mut cwd = replayed.cwd;

        let terminal = if let Some(mut decoded) =
            crate::daemon::decode_terminal_snapshot(&replayed.replay_bytes)
        {
            Self::apply_default_colors(&mut decoded);
            decoded
        } else {
            let mut terminal = Self::new_vt(cols, rows)?;
            if !replayed.replay_bytes.is_empty() {
                terminal.vt_write(&replayed.replay_bytes);
                // リプレイ中に発生した通知や問い合わせは破棄し、最新の PWD だけ反映
                for ev in terminal.events() {
                    if let VtEvent::PwdChanged(uri) = ev
                        && let Some(path) = parse_pwd_uri(&uri)
                    {
                        cwd = path;
                    }
                }
            }
            terminal
        };

        let folder_name = extract_folder_name(&cwd);
        let mut pane = Self {
            id: replayed.pane_id,
            terminal,
            render_state: RenderState::new()?,
            row_iter: RowIterator::new()?,
            cell_iter: CellIterator::new()?,
            daemon,
            cols,
            rows,
            last_cell_px: (0, 0),
            cwd,
            folder_name,
            last_output_at: Local::now(),
            last_output_instant: None,
            created_at: Instant::now(),
            last_resize_instant: None,
            sync_hold_since: None,
            has_notification: false,
            had_background_output: false,
            work_indicator_started_at: None,
            osc133_job_started_at: None,
            osc133_tail: [0; 6],
            osc133_tail_len: 0,
            is_waiting_input: false,
            last_seen_screen_hash: 0,
            last_notification_sent: None,
            minimap: MinimapSnapshot {
                cols,
                rows,
                ..Default::default()
            },
            minimap_dirty: true,
            generation: 1,
        };
        pane.mark_screen_seen();
        Ok(pane)
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.daemon.send_input(self.id, bytes.to_vec());
        }
    }

    /// マウスホイール／トラックパッドスクロールを端末モードに応じて処理する
    /// - マウストラッキング有効時（Claude Code / vim / tmux 等）: SGR/X10 ホイールシーケンス送信
    /// - 代替画面（less / man 等）: 上下矢印キー送信
    /// - 通常画面（zsh / agy 等）: スクロールバックビューポート移動
    pub fn handle_scroll(&mut self, lines: isize, col: u16, row: u16) {
        if lines == 0 {
            return;
        }
        if self.terminal.is_mouse_tracking().unwrap_or(false) {
            let count = lines.unsigned_abs().min(10);
            let btn: u8 = if lines < 0 { 64 } else { 65 };
            let cx = col.saturating_add(1).min(self.cols.max(1));
            let cy = row.saturating_add(1).min(self.rows.max(1));
            let mut buf = Vec::with_capacity(count * 16);
            if self.terminal.mode(Mode::SGR_MOUSE).unwrap_or(false) {
                for _ in 0..count {
                    buf.extend_from_slice(format!("\x1b[<{btn};{cx};{cy}M").as_bytes());
                }
            } else {
                let bx = 32u8.saturating_add(cx.min(223) as u8);
                let by = 32u8.saturating_add(cy.min(223) as u8);
                for _ in 0..count {
                    buf.extend_from_slice(&[0x1b, b'[', b'M', 32 + btn, bx, by]);
                }
            }
            self.write_input(&buf);
            return;
        }

        let in_alt_screen = self.terminal.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false)
            || self.terminal.mode(Mode::ALT_SCREEN).unwrap_or(false)
            || self.terminal.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false);
        if in_alt_screen {
            let count = lines.unsigned_abs().min(10);
            let app_cursor = self.terminal.mode(Mode::DECCKM).unwrap_or(false);
            let seq: &[u8] = match (lines < 0, app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let mut buf = Vec::with_capacity(count * seq.len());
            for _ in 0..count {
                buf.extend_from_slice(seq);
            }
            self.write_input(&buf);
            return;
        }

        self.terminal
            .scroll_viewport(ghostty_vt::terminal::ScrollViewport::Delta(lines));
        self.mark_dirty();
    }

    pub fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        let mut out = vec![0u8; data.len() + 32];
        match paste::encode(&mut data, bracketed, &mut out) {
            Ok(len) => self.write_input(&out[..len]),
            Err(ghostty_vt::Error::OutOfSpace { required }) => {
                let mut grown = vec![0u8; required];
                if let Ok(len) = paste::encode(&mut data, bracketed, &mut grown) {
                    self.write_input(&grown[..len]);
                }
            }
            Err(_) => {}
        }
    }

    pub fn update_cwd_from_daemon(&mut self, path: PathBuf) -> bool {
        if path != self.cwd {
            self.folder_name = extract_folder_name(&path);
            self.cwd = path;
            return true;
        }
        false
    }

    /// 過負荷時の自動再同期で受け取った完全スナップショットを現在の端末へ反映する
    /// GUI 側で最新のリサイズが行われていた場合は、旧サイズへ巻き戻さず最新サイズを維持する
    pub fn apply_resync_snapshot(&mut self, replayed: ReplayedPane) {
        if let Some(mut decoded) = crate::daemon::decode_terminal_snapshot(&replayed.replay_bytes) {
            Self::apply_default_colors(&mut decoded);
            let target_cols = self.cols.max(10);
            let target_rows = self.rows.max(4);
            let snap_cols = decoded.cols().unwrap_or(replayed.cols);
            let snap_rows = decoded.rows().unwrap_or(replayed.rows);
            if snap_cols != target_cols || snap_rows != target_rows {
                let _ = decoded.resize(
                    target_cols,
                    target_rows,
                    self.last_cell_px.0,
                    self.last_cell_px.1,
                );
            }
            self.terminal = decoded;
            self.update_cwd_from_daemon(replayed.cwd);
            self.last_output_at = Local::now();
            self.last_output_instant = Some(Instant::now());
            self.mark_dirty();
        }
    }

    fn try_send_os_notification(&mut self, title: String, body: String) {
        let now = Instant::now();
        if self
            .last_notification_sent
            .is_some_and(|t| now.duration_since(t) < NOTIFICATION_COOLDOWN)
        {
            return;
        }
        self.last_notification_sent = Some(now);
        macos::send_notification(title, body);
    }

    pub fn feed_pty_bytes(
        &mut self,
        bytes: &[u8],
        is_active: bool,
        window_focused: bool,
        slot_label: &str,
    ) {
        if bytes.is_empty() {
            return;
        }
        let now = Instant::now();
        let osc133_completed_long = self.process_osc_133_markers(bytes, now);
        self.terminal.vt_write(bytes);
        let in_startup_grace = now.duration_since(self.created_at) < STARTUP_GRACE_PERIOD;
        let is_resize_redraw = self
            .last_resize_instant
            .is_some_and(|t| now.duration_since(t) < RESIZE_GRACE_PERIOD);
        if !is_resize_redraw {
            self.last_output_at = Local::now();
            self.last_output_instant = Some(now);
            self.is_waiting_input = false;
            // 処理が再開して新規出力が流れた場合は古い通知フラグを解除する
            // （同一チャンク内に新規通知・BEL が含まれていれば後段で再点灯する）
            self.has_notification = false;
            if !is_active && !in_startup_grace {
                self.had_background_output = true;
                if self.has_active_work_indicator() {
                    self.work_indicator_started_at.get_or_insert(now);
                }
            }
        } else if !self.had_background_output {
            // リサイズ猶予中の再描画は未確認出力とみなさず基準ハッシュを更新する
            self.mark_screen_seen();
        }
        if osc133_completed_long && !is_active && !in_startup_grace {
            self.is_waiting_input = true;
            self.had_background_output = false;
            self.work_indicator_started_at = None;
            let title = format!("TenT [{}] {}", slot_label, self.folder_name);
            self.try_send_os_notification(
                title,
                "コマンドが完了し入力待ちになりました".to_string(),
            );
        }
        self.minimap_dirty = true;
        self.generation = self.generation.wrapping_add(1);

        let events: Vec<VtEvent> = self.terminal.events().collect();
        let mut saw_bell = false;
        let mut last_desktop_notif: Option<(String, String)> = None;

        for event in events {
            match event {
                VtEvent::PtyWrite(resp) => {
                    self.write_input(&resp);
                }
                VtEvent::PwdChanged(uri) => {
                    if let Some(path) = parse_pwd_uri(&uri) {
                        self.folder_name = extract_folder_name(&path);
                        self.cwd = path;
                    }
                }
                VtEvent::Bell => {
                    saw_bell = true;
                }
                VtEvent::DesktopNotification { title, body } => {
                    last_desktop_notif = Some((title, body));
                }
                _ => {}
            }
        }

        if let Some((title, body)) = last_desktop_notif {
            if !is_active {
                self.has_notification = true;
            }
            if !is_active || !window_focused {
                let notif_title = if title.is_empty() {
                    format!("TenT [{}] {}", slot_label, self.folder_name)
                } else {
                    format!("TenT [{}] {}", slot_label, title)
                };
                let notif_body = if body.is_empty() {
                    self.folder_name.clone()
                } else {
                    body
                };
                self.try_send_os_notification(notif_title, notif_body);
            }
        } else if saw_bell && !in_startup_grace && !is_resize_redraw {
            if !is_active {
                self.has_notification = true;
            }
            if !is_active || !window_focused {
                let title = format!("TenT [{}] {}", slot_label, self.folder_name);
                self.try_send_os_notification(title, "注意が必要です (BEL)".to_string());
            }
        }
    }

    pub fn mark_dirty(&mut self) {
        self.minimap_dirty = true;
        self.generation = self.generation.wrapping_add(1);
    }

    /// 画面およびスクロールバックをクリアする（macOS 標準の Cmd+K 相当）。
    /// プロトコル v4+ のデーモン接続時は明示的な Clear コマンドを送り、
    /// PTY 出力と順序を保ったままデーモン・GUI 双方の画面とスクロールバックを消去する。
    pub fn clear_screen_and_scrollback(&mut self) {
        use ghostty_vt::terminal::ScrollViewport;

        if self.daemon.supports_clear() {
            self.daemon.clear_pane(self.id);
        } else {
            self.terminal.vt_write(b"\x1b[3J\x1b[2J\x1b[H");
        }
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        // 通常スクリーン（シェル等）では Ctrl+L も送り、クリア後にプロンプトを再描画する
        let is_alt = self.terminal.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false)
            || self.terminal.mode(Mode::ALT_SCREEN).unwrap_or(false);
        if !is_alt {
            self.write_input(b"\x0c");
        }
        self.mark_screen_seen();
        self.mark_dirty();
    }

    /// DEC Mode 2026（Synchronized Output）が有効で画面書き換え途中の場合は true を返す。
    /// 書き換え途中のチラつき（画面クリア直後の中間フレーム表示）を防ぐため、
    /// 解除されるまで（最大 1 秒）前の完成済みフレームを維持する。
    pub fn is_render_held(&mut self, now: Instant) -> bool {
        let sync_on = self.terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
        if !sync_on {
            self.sync_hold_since = None;
            return false;
        }
        let started = *self.sync_hold_since.get_or_insert(now);
        now.duration_since(started) < SYNC_OUTPUT_MAX_HOLD
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_w_px: u32, cell_h_px: u32) {
        let cols = cols.max(10);
        let rows = rows.max(4);
        if self.cols == cols && self.rows == rows {
            return;
        }
        // リサイズ時点で未確認の変化がなかった場合は、リサイズによる折り返し変化を
        // 「未確認の出力」と誤認しないよう基準ハッシュを新サイズへ追従させる
        let screen_was_unseen_clean = self.current_screen_hash() == self.last_seen_screen_hash;
        self.cols = cols;
        self.rows = rows;
        self.last_cell_px = (cell_w_px, cell_h_px);
        self.last_resize_instant = Some(Instant::now());
        let _ = self.terminal.resize(cols, rows, cell_w_px, cell_h_px);
        if screen_was_unseen_clean {
            self.mark_screen_seen();
        }
        self.daemon.resize_pane(self.id, cols, rows);
        self.mark_dirty();
    }

    /// 直近2秒以内に出力があったか（LED・出力中ハイライト用）
    pub fn is_actively_outputting(&self, now: Instant) -> bool {
        self.last_output_instant
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
    }

    /// 現在のビューポート上の可視テキスト全体のハッシュを計算する
    /// （カーソル点滅や同一内容の再描画で「入力待ち」が誤復活するのを防ぐため）
    pub fn current_screen_hash(&mut self) -> u64 {
        if self.rows == 0 || self.cols == 0 {
            return 0;
        }
        let text = extract_selection_from_terminal(
            &self.terminal,
            self.cols,
            self.rows,
            (0, 0),
            (self.cols.saturating_sub(1), self.rows.saturating_sub(1)),
        );
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// ユーザーがこのペインを確認した時点の画面内容を記憶する
    pub fn mark_screen_seen(&mut self) {
        self.last_seen_screen_hash = self.current_screen_hash();
    }

    /// ビューポート内に AI エージェントの実行中インジケータ（`esc to interrupt` 等）が表示中か判定する
    pub fn has_active_work_indicator(&mut self) -> bool {
        if self.rows == 0 || self.cols == 0 {
            return false;
        }
        let end_row = self.rows.saturating_sub(1);
        let end_col = self.cols.saturating_sub(1);
        let visible = extract_selection_from_terminal(
            &self.terminal,
            self.cols,
            self.rows,
            (0, 0),
            (end_col, end_row),
        );
        let lower = visible.to_ascii_lowercase();
        lower.contains("esc to interrupt")
            || lower.contains("ctrl+c to interrupt")
            || lower.contains("esc to cancel")
    }

    /// 実行中インジケータ（AI エージェント等）を伴う裏スロット処理が
    /// 通知しきい値以上続いてから完了したか判定する
    pub(crate) fn should_notify_background_completion(&self, now: Instant) -> bool {
        let end = self.last_output_instant.unwrap_or(now);
        self.work_indicator_started_at.is_some_and(|start| {
            end.saturating_duration_since(start) >= BACKGROUND_NOTIFY_MIN_DURATION
        })
    }

    /// 裏タブで出力が止まったときに「AI入力待ち」へ遷移させ、
    /// 実行中インジケータを伴う長時間処理が完了した場合は OS 通知する
    pub fn update_waiting_state(
        &mut self,
        now: Instant,
        is_active: bool,
        slot_label: Option<&str>,
    ) -> bool {
        if is_active {
            self.had_background_output = false;
            self.work_indicator_started_at = None;
            return false;
        }
        let idle_long_enough = self
            .last_output_instant
            .is_some_and(|t| now.duration_since(t) >= WAITING_IDLE_THRESHOLD);
        if self.had_background_output && idle_long_enough {
            // Codex / Claude Code 等が処理中（esc to interrupt 表示中）なら入力待ちにしない。
            // ただし 4 秒以上出力が止まっている場合は過去ログ残留と区別するため開始時刻は引き継がない。
            if self.has_active_work_indicator() {
                self.work_indicator_started_at = None;
                return false;
            }
            let should_notify = self.should_notify_background_completion(now);
            self.had_background_output = false;
            self.work_indicator_started_at = None;
            // 最後に確認した時点から可視テキストが一切変わっていなければ（カーソル点滅・同一再描画等）黄色にしない
            let cur_hash = self.current_screen_hash();
            if cur_hash == self.last_seen_screen_hash {
                return false;
            }
            if !self.is_waiting_input {
                self.is_waiting_input = true;
                if should_notify && let Some(label) = slot_label {
                    let title = format!("TenT [{}] {}", label, self.folder_name);
                    self.try_send_os_notification(
                        title,
                        "出力が完了し入力待ちになりました".to_string(),
                    );
                }
                return true;
            }
        }
        false
    }

    /// 指定のセル範囲 (col, row) から選択テキストを抽出する（ビューポート座標版）
    #[cfg(test)]
    pub fn extract_selection_text(&mut self, a: (u16, u16), b: (u16, u16)) -> String {
        extract_selection_from_terminal(&self.terminal, self.cols, self.rows, a, b)
    }

    /// 現在のビューポート先頭行の `Screen` 絶対行番号を返す
    pub fn viewport_top_screen_row(&self) -> u32 {
        viewport_top_screen_row(&self.terminal)
    }

    /// ビューポートセル `(col, vp_row)` をスクロールバック込みの `Screen` 絶対座標へ変換する
    pub fn viewport_cell_to_screen(&self, cell: (u16, u16)) -> (u16, u32) {
        let vp_top = viewport_top_screen_row(&self.terminal);
        let col = cell.0.min(self.cols.saturating_sub(1));
        let row = cell.1.min(self.rows.saturating_sub(1));
        (col, vp_top.saturating_add(u32::from(row)))
    }

    /// `Screen` 絶対座標 `(col, screen_row)` に対する追跡ピン（`TrackedGridRef`）を生成する。
    /// スクロールバック先頭パージやリフローに自動追従し、消滅した場合は無効になる。
    pub fn track_screen_point(&self, pt: (u16, u32)) -> Option<TrackedGridRef> {
        self.terminal
            .track_grid_ref(Point::Screen(PointCoordinate { x: pt.0, y: pt.1 }))
            .ok()
    }

    /// 追跡ピンを現在の `Screen` 絶対座標へ解決する（パージや `CSI 3 J` で消滅していれば `None`）
    pub fn resolve_tracked_point(&self, pin: &TrackedGridRef) -> Option<(u16, u32)> {
        let gr = pin.snapshot(&self.terminal).ok()??;
        let coord = self
            .terminal
            .point_from_grid_ref(&gr, PointSpace::Screen)
            .ok()??;
        Some((coord.x, coord.y))
    }

    /// 追跡ピンで指定された選択範囲からテキストを抽出する（ピン無効時は空文字列）
    pub fn extract_tracked_selection_text(&self, a: &TrackedGridRef, b: &TrackedGridRef) -> String {
        let (Some(pa), Some(pb)) = (self.resolve_tracked_point(a), self.resolve_tracked_point(b))
        else {
            return String::new();
        };
        self.extract_screen_selection_text(pa, pb)
    }

    /// 追跡ピンで指定された選択範囲を現在のビューポート範囲へ投影する（ハイライト描画用）
    #[cfg(test)]
    pub fn tracked_selection_to_viewport(
        &self,
        a: &TrackedGridRef,
        b: &TrackedGridRef,
    ) -> Option<((u16, u16), (u16, u16))> {
        let pa = self.resolve_tracked_point(a)?;
        let pb = self.resolve_tracked_point(b)?;
        self.screen_selection_to_viewport(pa, pb)
    }

    /// `Screen` 絶対座標 `(col, screen_row)` の単語範囲 `(start_col, end_col)` を返す
    pub fn word_bounds_at_screen(&self, col: u16, screen_row: u32) -> (u16, u16) {
        word_bounds_at_screen(&self.terminal, self.cols, col, screen_row)
    }

    /// `Screen` 絶対座標 `screen_row` を含む論理行（ソフトラップ連結）全体の範囲を返す
    pub fn line_bounds_at_screen(&self, screen_row: u32) -> ((u16, u32), (u16, u32)) {
        line_bounds_at_screen(&self.terminal, self.cols, screen_row)
    }

    /// `Screen` 絶対座標の範囲から選択テキストを抽出する（Cmd+C コピー・スクロールバック跨ぎ対応）
    pub fn extract_screen_selection_text(&self, a: (u16, u32), b: (u16, u32)) -> String {
        extract_selection_from_screen(&self.terminal, self.cols, a, b)
    }

    /// `Screen` 絶対座標の選択範囲を現在のビューポート範囲へ投影する（ハイライト描画用）
    pub fn screen_selection_to_viewport(
        &self,
        a: (u16, u32),
        b: (u16, u32),
    ) -> Option<((u16, u16), (u16, u16))> {
        let vp_top = viewport_top_screen_row(&self.terminal);
        screen_selection_to_viewport(a, b, vp_top, self.cols, self.rows)
    }

    /// 代替画面（vim / less / tmux 等）が表示中か返す
    pub fn is_alt_screen(&self) -> bool {
        self.terminal.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false)
            || self.terminal.mode(Mode::ALT_SCREEN).unwrap_or(false)
            || self.terminal.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false)
    }

    /// 指定のビューポートセル (col, row) にある URL（OSC 8 ハイパーリンクまたは http/https）を返す
    pub fn url_at_cell(&mut self, col: u16, row: u16) -> Option<UrlMatch> {
        find_url_at_cell(&self.terminal, self.cols, col, row)
    }

    /// OSC 133 シェル統合マーカー（`133;A` / `133;C` / `133;D`）をチャンク境界を跨いで検出・処理する。
    /// 通知しきい値以上実行されたコマンドの終了（`133;D`）を検出した場合は `true` を返す。
    fn process_osc_133_markers(&mut self, bytes: &[u8], now: Instant) -> bool {
        const MARKER_LEN: usize = 7; // b"\x1b]133;X"
        const TAIL_CAP: usize = MARKER_LEN - 1;

        let mut completed_long = false;
        let tail_len = usize::from(self.osc133_tail_len);

        // 1. 前回末尾（最大 6B）+ 今回先頭（最大 6B）で境界を跨ぐ 7B ウィンドウを走査
        if tail_len > 0 {
            let mut bridge = [0u8; TAIL_CAP * 2];
            bridge[..tail_len].copy_from_slice(&self.osc133_tail[..tail_len]);
            let head_len = bytes.len().min(TAIL_CAP);
            bridge[tail_len..tail_len + head_len].copy_from_slice(&bytes[..head_len]);
            let total = tail_len + head_len;
            if total >= MARKER_LEN {
                for w in bridge[..total].windows(MARKER_LEN) {
                    if self.handle_osc_133_window(w, now) {
                        completed_long = true;
                    }
                }
            }
        }

        // 2. 今回のバイト列内部の 7B ウィンドウを走査
        for w in bytes.windows(MARKER_LEN) {
            if self.handle_osc_133_window(w, now) {
                completed_long = true;
            }
        }

        // 3. 次回チャンク用に直近最大 6B を保持
        if bytes.len() >= TAIL_CAP {
            self.osc133_tail
                .copy_from_slice(&bytes[bytes.len() - TAIL_CAP..]);
            self.osc133_tail_len = TAIL_CAP as u8;
        } else {
            let mut combined = [0u8; TAIL_CAP * 2];
            combined[..tail_len].copy_from_slice(&self.osc133_tail[..tail_len]);
            combined[tail_len..tail_len + bytes.len()].copy_from_slice(bytes);
            let total = tail_len + bytes.len();
            let keep = total.min(TAIL_CAP);
            self.osc133_tail[..keep].copy_from_slice(&combined[total - keep..total]);
            self.osc133_tail_len = keep as u8;
        }

        completed_long
    }

    fn handle_osc_133_window(&mut self, w: &[u8], now: Instant) -> bool {
        if w == b"\x1b]133;C" {
            self.osc133_job_started_at = Some(now);
            false
        } else if w == b"\x1b]133;D" {
            self.osc133_job_started_at.take().is_some_and(|start| {
                now.saturating_duration_since(start) >= BACKGROUND_NOTIFY_MIN_DURATION
            })
        } else {
            false
        }
    }
}

pub fn extract_folder_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

fn parse_pwd_uri(uri: &str) -> Option<PathBuf> {
    if let Some(rest) = uri.strip_prefix("file://") {
        let slash_idx = rest.find('/')?;
        let encoded_path = &rest[slash_idx..];
        let decoded = percent_decode(encoded_path);
        Some(PathBuf::from(decoded))
    } else if uri.starts_with('/') {
        Some(PathBuf::from(uri))
    } else {
        None
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
