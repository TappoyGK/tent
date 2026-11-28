use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use chrono::Local;
use ghostty_vt::Terminal;
use ghostty_vt::screen::CellWide;
use ghostty_vt::terminal::{Point, PointCoordinate};

use super::selection::extract_selection_from_terminal;
use super::{BACKGROUND_NOTIFY_MIN_DURATION, TerminalPane, WAITING_IDLE_THRESHOLD};
use crate::daemon::{AiHookState, AiTool};

impl TerminalPane {
    /// このペインが AI フック駆動モードで管理されているか返す。
    /// フックを 1 回も設定していない環境では一般ルールを維持し、フックを受信した AI セッションのみ切り替える。
    pub fn is_ai_hook_managed(&self) -> bool {
        self.active_ai_tool.is_some() && self.ai_hook_active
    }

    /// デーモンから届いたフォアグラウンド AI ツール種別を反映する。
    /// シェル復帰時（`None`）にフック管理モードを解除して一般ルールへ戻す。
    pub fn update_ai_tool_from_daemon(&mut self, tool: Option<AiTool>) -> bool {
        if self.active_ai_tool == tool {
            return false;
        }
        self.active_ai_tool = tool;
        if tool.is_none() {
            // AI 終了してシェルへ戻ったためフック状態を解除し、現在画面を基準にする
            self.ai_hook_active = false;
            self.ai_hook_state = None;
            self.ai_working_since = None;
            self.had_background_output = false;
            self.work_indicator_started_at = None;
            self.last_output_instant = None;
            self.mark_screen_seen();
        }
        true
    }

    /// AI フック（`tent hook`）から届いた明示的な状態遷移を適用する。
    pub fn apply_ai_hook_event(
        &mut self,
        state: AiHookState,
        tool: Option<AiTool>,
        is_active: bool,
        window_focused: bool,
        slot_label: &str,
    ) -> bool {
        if let Some(t) = tool {
            self.active_ai_tool = Some(t);
        }
        self.ai_hook_active = true;
        let now = Instant::now();
        self.had_background_output = false;
        self.work_indicator_started_at = None;
        self.osc133_job_started_at = None;

        match state {
            AiHookState::Working => {
                self.ai_hook_state = Some(AiHookState::Working);
                self.ai_working_since.get_or_insert(now);
                self.is_waiting_input = false;
                self.has_notification = false;
                self.last_output_at = Local::now();
                self.last_output_instant = Some(now);
            }
            AiHookState::Waiting => {
                // あいだに Working / Idle / ユーザー入力（write_input） / スロット確認（clear_attention）を
                // 挟まずに届いた重複 Waiting（例: PermissionRequest の 6 秒後に来る Notification）を抑止する
                if self.ai_hook_state == Some(AiHookState::Waiting) {
                    return false;
                }
                let worked_duration = self
                    .ai_working_since
                    .take()
                    .map(|start| now.saturating_duration_since(start));
                self.ai_hook_state = Some(AiHookState::Waiting);
                self.last_output_at = Local::now();
                self.last_output_instant = None;
                if !is_active {
                    self.is_waiting_input = true;
                }
                let long_enough =
                    worked_duration.is_none_or(|d| d >= BACKGROUND_NOTIFY_MIN_DURATION);
                if (!is_active || !window_focused) && long_enough {
                    let title = format!("TenT [{}] {}", slot_label, self.folder_name);
                    self.try_send_os_notification(
                        title,
                        "AI の応答が完了／確認待ちになりました".to_string(),
                    );
                }
            }
            AiHookState::Idle => {
                self.ai_hook_state = None;
                self.ai_working_since = None;
                self.is_waiting_input = false;
                self.has_notification = false;
                self.last_output_instant = None;
            }
        }
        true
    }

    /// 処理中・出力中か（LED・出力中ハイライト用）。
    /// AI フック管理モード中はフック由来の `Working` 状態を正とし、それ以外は直近 2 秒以内の出力で判定する。
    pub fn is_actively_outputting(&self, now: Instant) -> bool {
        if self.is_ai_hook_managed() {
            return self.ai_hook_state == Some(AiHookState::Working);
        }
        self.last_output_instant
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
    }

    /// 現在のアクティブ画面（`Point::Active`）各行の可視テキストハッシュ配列をヒープ確保なしで計算する。
    /// 空行は `None`、非空行は `Some(hash)`。スクロールバック閲覧中でも最新出力を正確に検知できる。
    pub fn current_active_line_hashes(&self) -> Vec<Option<u64>> {
        if self.rows == 0 || self.cols == 0 {
            return Vec::new();
        }
        let mut hashes = Vec::with_capacity(usize::from(self.rows));
        for row in 0..self.rows {
            hashes.push(hash_active_line(&self.terminal, self.cols, row));
        }
        hashes
    }

    /// ユーザーがこのペインを確認した時点の各行ハッシュと総行数を記憶する
    pub fn mark_screen_seen(&mut self) {
        let lines = self.current_active_line_hashes();
        self.last_seen_line_hashes = lines.clone();
        self.last_seen_total_rows = self.terminal.total_rows().unwrap_or(0);
        self.last_active_line_hashes = lines;
        self.active_lines_dirty = false;
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

    /// 裏タブで出力が止まったときに「入力待ち」へ遷移させ、
    /// 実行中インジケータを伴う長時間処理が完了した場合は OS 通知する。
    /// AI 稼働中はフック駆動のため、ここでの汎用アイドル判定は行わない。
    pub fn update_waiting_state(
        &mut self,
        now: Instant,
        is_active: bool,
        slot_label: Option<&str>,
    ) -> bool {
        if self.is_ai_hook_managed() {
            return false;
        }
        if is_active {
            self.had_background_output = false;
            self.work_indicator_started_at = None;
            return false;
        }
        let idle_long_enough = self
            .last_output_instant
            .is_some_and(|t| now.duration_since(t) >= WAITING_IDLE_THRESHOLD);
        if self.had_background_output && idle_long_enough {
            if self.has_active_work_indicator() {
                self.work_indicator_started_at = None;
                return false;
            }
            let should_notify = self.should_notify_background_completion(now);
            self.had_background_output = false;
            self.work_indicator_started_at = None;
            let cur_lines = self.current_active_line_hashes();
            let cur_total_rows = self.terminal.total_rows().unwrap_or(0);
            let has_visible_change = cur_total_rows != self.last_seen_total_rows
                || cur_lines != self.last_seen_line_hashes;
            if !has_visible_change {
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

    /// OSC 133 シェル統合マーカー（`133;A` / `133;C` / `133;D`）をチャンク境界を跨いで検出・処理する。
    /// 通知しきい値以上実行されたコマンドの終了（`133;D`）を検出した場合は `true` を返す。
    pub(super) fn process_osc_133_markers(&mut self, bytes: &[u8], now: Instant) -> bool {
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

/// アクティブ画面の 1 行をヒープ確保なしでハッシュ化する（空行なら `None`）
fn hash_active_line(terminal: &Terminal, cols: u16, row: u16) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    let mut grapheme_buf = ['\0'; 16];
    let mut pending_spaces: u16 = 0;
    let mut has_non_space = false;

    for col in 0..cols {
        let Ok(gr) = terminal.grid_ref(Point::Active(PointCoordinate {
            x: col,
            y: u32::from(row),
        })) else {
            pending_spaces = pending_spaces.saturating_add(1);
            continue;
        };
        let wide = gr
            .cell()
            .ok()
            .and_then(|c| c.wide().ok())
            .unwrap_or(CellWide::Narrow);
        if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
            continue;
        }
        let len = gr.graphemes(&mut grapheme_buf).unwrap_or(0);
        let is_space = len == 0 || (len == 1 && grapheme_buf[0] == ' ');
        if is_space {
            pending_spaces = pending_spaces.saturating_add(1);
        } else {
            if pending_spaces > 0 {
                0u32.hash(&mut hasher);
                pending_spaces.hash(&mut hasher);
                pending_spaces = 0;
            }
            for &ch in &grapheme_buf[..len] {
                ch.hash(&mut hasher);
            }
            has_non_space = true;
        }
    }

    has_non_space.then(|| hasher.finish())
}
