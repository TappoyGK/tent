use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use chrono::Local;
use ghostty_vt::Terminal;
use ghostty_vt::screen::CellWide;
use ghostty_vt::terminal::{Point, PointCoordinate};

use super::selection::extract_selection_from_terminal;
use super::{
    AGY_PERMISSION_PROMPT_IDLE, BACKGROUND_NOTIFY_MIN_DURATION, TerminalPane,
    WAITING_IDLE_THRESHOLD,
};
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
            self.ai_permission_prompt_detected = false;
            self.detected_agy_permission_prompt = None;
            self.agy_prompt_candidate = None;
            self.dismissed_agy_permission_prompt = None;
            self.last_agy_prompt_checked_gen = None;
            self.last_agy_prompt_snapshot = None;
            self.agy_prompt_absent_since = None;
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
        let was_prompt_detected = self.ai_permission_prompt_detected;
        let prev_detected = self.detected_agy_permission_prompt.take();
        if was_prompt_detected
            && let Some(sig) = self.last_agy_prompt_snapshot.clone().or(prev_detected)
        {
            // 画面検知による Waiting をフック（Stop 等）が上書きした際、
            // そのダイアログを応答済みへ移して次ターンで古い残存ダイアログを再検知しないようにする
            self.dismissed_agy_permission_prompt = Some(sig);
        }
        self.ai_permission_prompt_detected = false;
        self.agy_prompt_absent_since = None;
        self.agy_prompt_candidate = None;
        self.last_agy_prompt_checked_gen = None;
        // 応答済み署名は画面からダイアログが消える（検出が None になる）まで保持し、
        // PreInvocation（Working）や次ターン開始で古い残存ダイアログを再検知しないようにする。
        if matches!(state, AiHookState::Idle) && self.active_ai_tool != Some(AiTool::Agy) {
            self.dismissed_agy_permission_prompt = None;
        }
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
                // 挟まずに届いた重複 Waiting（例: PermissionRequest の 6 秒後に来る Notification）を抑止する。
                // ただし画面検知由来の Waiting はフックの Waiting（Stop 等）を抑止しない。
                if self.ai_hook_state == Some(AiHookState::Waiting) && !was_prompt_detected {
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

    /// アクティブ画面の最後の非空行から最大 16 行以内（折り返し行は連結）に agy の
    /// パーミッション確認プロンプトが表示されているか判定し、見出し〜最後拒否行の署名を返す。
    pub(super) fn detect_agy_permission_prompt(&self) -> Option<AgyPermissionPromptSignature> {
        if self.rows == 0 || self.cols == 0 {
            return None;
        }
        let mut last_non_empty_row = None;
        for row in (0..self.rows).rev() {
            if hash_active_line(&self.terminal, self.cols, row).is_some() {
                last_non_empty_row = Some(row);
                break;
            }
        }
        let end_row = last_non_empty_row?;
        let start_row = end_row.saturating_sub(15);

        let mut logical_lines: Vec<String> =
            Vec::with_capacity(usize::from(end_row - start_row + 1));
        let mut grapheme_buf = ['\0'; 16];
        let mut prev_wrapped = false;
        for row in start_row..=end_row {
            let is_wrapped = self
                .terminal
                .grid_ref(Point::Active(PointCoordinate {
                    x: 0,
                    y: u32::from(row),
                }))
                .ok()
                .and_then(|gr| gr.row().ok())
                .and_then(|r| r.is_wrapped().ok())
                .unwrap_or(false);

            let mut row_text = String::with_capacity(usize::from(self.cols));
            for col in 0..self.cols {
                let Ok(gr) = self.terminal.grid_ref(Point::Active(PointCoordinate {
                    x: col,
                    y: u32::from(row),
                })) else {
                    row_text.push(' ');
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
                if len == 0 {
                    row_text.push(' ');
                } else {
                    for &ch in &grapheme_buf[..len] {
                        row_text.push(ch);
                    }
                }
            }
            if !is_wrapped {
                let trimmed_len = row_text.trim_end().len();
                row_text.truncate(trimmed_len);
            }
            if prev_wrapped && let Some(last) = logical_lines.last_mut() {
                last.push_str(&row_text);
            } else {
                logical_lines.push(row_text);
            }
            prev_wrapped = is_wrapped;
        }

        let line_refs: Vec<&str> = logical_lines.iter().map(String::as_str).collect();
        let (allow_start, last_deny) = match_agy_permission_prompt_lines(&line_refs)?;

        // 実物の見出し行を、ひとつ上のダイアログの選択肢行の手前まで遡って探す。
        // 見つかれば見出しから最後の拒否行までを署名に含め（見出しより上の行は含めない）、
        // 見つからなければ選択肢行のみで署名を作る。
        let mut sig_start = allow_start;
        for idx in (0..allow_start).rev() {
            let opt_lower = strip_option_affixes(&logical_lines[idx]).to_ascii_lowercase();
            if is_agy_allow_option(&opt_lower) || is_agy_deny_option(&opt_lower) {
                break;
            }
            if is_agy_prompt_heading(&logical_lines[idx]) {
                sig_start = idx;
                break;
            }
        }

        let mut line_hashes = Vec::with_capacity(last_deny - sig_start + 1);
        for (idx, line) in logical_lines[sig_start..=last_deny].iter().enumerate() {
            let abs_idx = sig_start + idx;
            let normalized = if abs_idx >= allow_start {
                strip_option_affixes(line).to_ascii_lowercase()
            } else {
                strip_border_and_trim(line).to_ascii_lowercase()
            };
            let mut hasher = DefaultHasher::new();
            normalized.hash(&mut hasher);
            line_hashes.push(hasher.finish());
        }

        Some(AgyPermissionPromptSignature { line_hashes })
    }

    /// 裏タブで出力が止まったときに「入力待ち」へ遷移させ、
    /// 実行中インジケータを伴う長時間処理が完了した場合は OS 通知する。
    /// AI 稼働中はフック駆動のため汎用アイドル判定は行わないが、
    /// `PermissionRequest` フックを持たない `agy` では `Working` 中のパーミッション確認 UI のみ補完検知する。
    pub fn update_waiting_state(
        &mut self,
        now: Instant,
        is_active: bool,
        window_focused: bool,
        slot_label: Option<&str>,
    ) -> bool {
        if self.is_ai_hook_managed() {
            if self.active_ai_tool != Some(AiTool::Agy) {
                return false;
            }
            let should_scan = self.ai_hook_state == Some(AiHookState::Working)
                || self.ai_permission_prompt_detected
                || self.dismissed_agy_permission_prompt.is_some();
            if !should_scan {
                return false;
            }
            if self.last_agy_prompt_checked_gen != Some(self.content_generation) {
                self.last_agy_prompt_checked_gen = Some(self.content_generation);
                self.last_agy_prompt_snapshot = self.detect_agy_permission_prompt();
            }
            let Some(sig) = self.last_agy_prompt_snapshot.clone() else {
                self.agy_prompt_candidate = None;
                // 一瞬のフレーム描き替え（2 分割フレームやリサイズ再描画）による誤再検知・重複通知を防ぐため、
                // 応答済み署名のクリアおよび Waiting からの自動復帰はいずれも None が 1.2 秒以上継続してから行う
                let absent_since = *self.agy_prompt_absent_since.get_or_insert(now);
                if now.saturating_duration_since(absent_since) >= AGY_PERMISSION_PROMPT_IDLE {
                    self.dismissed_agy_permission_prompt = None;
                    self.agy_prompt_absent_since = None;
                    if self.ai_permission_prompt_detected {
                        self.ai_permission_prompt_detected = false;
                        self.detected_agy_permission_prompt = None;
                        self.ai_hook_state = Some(AiHookState::Working);
                        self.is_waiting_input = false;
                        return true;
                    }
                }
                return false;
            };
            self.agy_prompt_absent_since = None;

            if self.ai_permission_prompt_detected {
                // Waiting 中（自動復帰待ち 1.2 秒以内を含む）に別内容のダイアログへ切り替わった場合は
                // 新しい許可待ちとして通知・時刻更新を行う
                if self
                    .detected_agy_permission_prompt
                    .as_ref()
                    .is_some_and(|cur| !cur.matches(&sig))
                {
                    self.detected_agy_permission_prompt = Some(sig);
                    self.ai_hook_state = Some(AiHookState::Waiting);
                    self.last_output_at = Local::now();
                    self.last_output_instant = None;
                    if !is_active {
                        self.is_waiting_input = true;
                    }
                    if (!is_active || !window_focused)
                        && let Some(label) = slot_label
                    {
                        let title = format!("TenT [{}] {}", label, self.folder_name);
                        self.try_send_os_notification(
                            title,
                            "AI がパーミッション確認を待っています".to_string(),
                        );
                    }
                    return true;
                }
                // アクティブスロットで検知したまま裏スロットへ回った場合は黄色表示を立てる
                if self.ai_hook_state == Some(AiHookState::Waiting)
                    && !is_active
                    && !self.is_waiting_input
                {
                    self.is_waiting_input = true;
                    return true;
                }
                return false;
            }

            if self.ai_hook_state == Some(AiHookState::Working) {
                if self
                    .dismissed_agy_permission_prompt
                    .as_ref()
                    .is_some_and(|d| d.matches(&sig))
                {
                    self.agy_prompt_candidate = None;
                    return false;
                }
                // 出力全体が止まっているかだけでなく、ダイアログ内容が 1.2 秒間変化せずに留まっているかで判定する
                // （ダイアログ表示中に見出しより上のスピナーや経過時間が更新され続けるケースにも対応）
                let first_seen = match &self.agy_prompt_candidate {
                    Some((prev, t)) if prev == &sig => *t,
                    _ => {
                        let initial = self.last_pty_output_instant.map_or(now, |p| p.min(now));
                        self.agy_prompt_candidate = Some((sig.clone(), initial));
                        initial
                    }
                };
                if now.saturating_duration_since(first_seen) >= AGY_PERMISSION_PROMPT_IDLE {
                    self.ai_hook_state = Some(AiHookState::Waiting);
                    self.ai_permission_prompt_detected = true;
                    self.detected_agy_permission_prompt = Some(sig);
                    self.agy_prompt_absent_since = None;
                    self.dismissed_agy_permission_prompt = None;
                    self.agy_prompt_candidate = None;
                    self.last_output_at = Local::now();
                    self.last_output_instant = None;
                    if !is_active {
                        self.is_waiting_input = true;
                    }
                    if (!is_active || !window_focused)
                        && let Some(label) = slot_label
                    {
                        let title = format!("TenT [{}] {}", label, self.folder_name);
                        self.try_send_os_notification(
                            title,
                            "AI がパーミッション確認を待っています".to_string(),
                        );
                    }
                    return true;
                }
            }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgyPermissionPromptSignature {
    /// 実物の見出し行（存在する場合）から最後の拒否選択肢行までの正規化ハッシュ列。
    /// 見出しより上の行（スピナー等）や `Point::Screen` 行番号を含めないことで、
    /// スピナー更新・リサイズのリフロー・スクロールバックパージでの誤再検知を防ぎつつ、
    /// コマンドや URL の異なる同選択肢ダイアログを区別する。
    line_hashes: Vec<u64>,
}

impl AgyPermissionPromptSignature {
    /// `other` がこの応答済みダイアログと同一か判定する。
    /// 見出しの書き換えや出力継続で上側の行が欠けても、残った行が応答済み列の接尾辞と一致すれば同一とみなす。
    fn matches(&self, other: &Self) -> bool {
        !other.line_hashes.is_empty() && self.line_hashes.ends_with(&other.line_hashes)
    }
}

/// パーミッション確認ダイアログに対する確定・キャンセル応答キー（応答済み署名を記録する対象）か判定する。
/// `agy` で確定動作が確認できていない文字・数字・`y`/`n`・スペース・貼り付け等は `false` を返し、
/// ダイアログが残れば 1.2 秒後に再検知させる。
pub(super) fn is_prompt_confirm_input(bytes: &[u8]) -> bool {
    matches!(bytes, b"\r" | b"\n" | b"\r\n" | b"\x1b" | b"\x03")
}

/// パーミッション確認ダイアログの選択肢移動のみを行うキー入力か判定する（確定・キャンセルではない）。
/// 連続送出された矢印キー（`\x1b[B\x1b[B`）、修飾付き／数値付き CSI 矢印（`\x1b[1;2B` / `\x1b[1B`）、
/// SS3 矢印、Tab / Shift+Tab、マウスシーケンスのみで構成されている場合に `true` を返す。
pub(super) fn is_prompt_navigation_input(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let mut rest = bytes;
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix(b"\t") {
            rest = next;
        } else if let Some(next) = rest.strip_prefix(b"\x1b[Z") {
            rest = next;
        } else if rest.len() >= 3
            && rest[0] == 0x1b
            && rest[1] == b'O'
            && matches!(rest[2], b'A'..=b'D')
        {
            rest = &rest[3..];
        } else if let Some(after_csi) = rest.strip_prefix(b"\x1b[") {
            if after_csi.starts_with(b"<") {
                if let Some(end_idx) = after_csi.iter().position(|&b| b == b'M' || b == b'm') {
                    rest = &after_csi[end_idx + 1..];
                } else {
                    return false;
                }
            } else if after_csi.starts_with(b"M") && after_csi.len() >= 4 {
                rest = &after_csi[4..];
            } else {
                let param_len = after_csi
                    .iter()
                    .take_while(|b| b.is_ascii_digit() || **b == b';')
                    .count();
                if param_len < after_csi.len() && matches!(after_csi[param_len], b'A'..=b'D') {
                    rest = &after_csi[param_len + 1..];
                } else {
                    return false;
                }
            }
        } else {
            return false;
        }
    }
    true
}

fn strip_border_and_trim(line: &str) -> &str {
    let mut s = line.trim();
    if let Some(rest) = s
        .strip_prefix('│')
        .or_else(|| s.strip_prefix('┃'))
        .or_else(|| s.strip_prefix('║'))
    {
        s = rest.trim_start();
    }
    if let Some(rest) = s
        .strip_suffix('│')
        .or_else(|| s.strip_suffix('┃'))
        .or_else(|| s.strip_suffix('║'))
    {
        s = rest.trim_end();
    }
    s
}

/// `agy` 実物のパーミッション確認ダイアログ見出し行か判定する（署名開始位置の特定用）。
fn is_agy_prompt_heading(line: &str) -> bool {
    matches!(
        strip_border_and_trim(line).to_ascii_lowercase().as_str(),
        "run this command?"
            | "allow access to this url?"
            | "allow creation of this file?"
            | "allow calling this tool?"
            | "do you want to proceed?"
            | "allow sandbox bypass for command execution?"
            | "grant requested permission?"
            | "accept this file edit?"
            | "allow access to this file?"
            | "allow administrator elevation?"
    )
}

/// ダイアログ選択肢の行頭装飾（空白・枠線・選択カーソル・連番）と行末の枠線・空白を取り除く。
/// diff 行（`+` / `-`）の誤検知を防ぐため `+` / `-` は取り除かない。
fn strip_option_affixes(line: &str) -> &str {
    let mut s = strip_border_and_trim(line);
    if let Some(first) = s.chars().next()
        && matches!(
            first,
            '>' | '❯' | '›' | '▸' | '▶' | '→' | '●' | '○' | '◉' | '•' | '*'
        )
    {
        s = s[first.len_utf8()..].trim_start();
    }
    let digits_len = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_len > 0 && digits_len <= 2 {
        let after_digits = &s[digits_len..];
        if let Some(rest) = after_digits
            .strip_prefix(". ")
            .or_else(|| after_digits.strip_prefix(") "))
        {
            s = rest.trim_start();
        }
    }
    s.trim_end()
}

fn is_agy_allow_option(opt_lower: &str) -> bool {
    matches!(
        opt_lower,
        "yes, run command"
            | "yes, allow access"
            | "yes, allow creation"
            | "yes, allow tool call"
            | "yes, accept this change"
            | "yes, elevate"
            | "yes, grant permission"
    ) || opt_lower.starts_with("yes, and always allow ")
        || opt_lower.starts_with("yes, grant permission for ")
        || opt_lower.starts_with("yes, and always grant permission for ")
        || opt_lower.starts_with("yes, use auto mode until you exit")
        || opt_lower.starts_with("yes, and use auto mode until you exit")
        || (opt_lower.starts_with("yes, and tell ") && opt_lower.ends_with(" what to do next"))
}

fn is_agy_deny_option(opt_lower: &str) -> bool {
    matches!(
        opt_lower,
        "no, deny"
            | "no, deny access"
            | "no, deny creation"
            | "no, deny tool call"
            | "no, deny permission"
            | "no, reject this change"
    ) || opt_lower.starts_with("no, and always deny ")
        || (opt_lower.starts_with("no, and tell ")
            && opt_lower.ends_with(" what to do differently"))
}

/// アクティブ画面の論理行スライスから `agy` のパーミッション確認ダイアログ（実物の許可／拒否選択肢の組）を
/// 検出し、`(許可選択肢ブロック先頭インデックス, 最後の拒否選択肢インデックス)` を返す。
/// - 末尾 3 非空行（フッター）に `esc to interrupt` がある場合は実行中とみなして除外する。
/// - 最後の拒否行から上へ向かって 10 行以内の直近の許可行ブロックと組むことで、
///   画面上部に過去のダイアログが残っていても最新ダイアログを取りこぼさない。
fn match_agy_permission_prompt_lines(lines: &[&str]) -> Option<(usize, usize)> {
    // 末尾 3 非空行（フッター）に esc to interrupt がある場合は実行中とみなす
    let mut non_empty_seen = 0usize;
    for line in lines.iter().rev() {
        if line.trim().is_empty() {
            continue;
        }
        if line.to_ascii_lowercase().contains("esc to interrupt") {
            return None;
        }
        non_empty_seen += 1;
        if non_empty_seen >= 3 {
            break;
        }
    }

    let mut is_allow = Vec::with_capacity(lines.len());
    let mut last_deny: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        let opt_lower = strip_option_affixes(line).to_ascii_lowercase();
        let allow = is_agy_allow_option(&opt_lower);
        is_allow.push(allow);
        if !allow && is_agy_deny_option(&opt_lower) {
            last_deny = Some(idx);
        }
    }

    let deny_idx = last_deny?;
    let min_search = deny_idx.saturating_sub(10);
    // 最後の拒否行から上へ向かって、10 行以内にある最も近い許可行を探す
    let nearest_allow = (min_search..deny_idx).rev().find(|&i| is_allow[i])?;
    // 同じダイアログ内の連続した許可選択肢（例: Yes, run command / Yes, and always allow ...）の先頭まで遡る
    let mut allow_start = nearest_allow;
    while allow_start > min_search && is_allow[allow_start - 1] {
        allow_start -= 1;
    }
    Some((allow_start, deny_idx))
}

/// 画面テキストに `agy` のパーミッション確認プロンプトが含まれているか判定する。
/// 通常会話・Markdown 箇条書き・diff・JSON 等での偽陽性を防ぐため、末尾 3 非空行（フッター）の `esc to interrupt` を除外した上で
/// `agy` 実物の決まった許可／拒否選択肢（完全一致または可変引数付き定型文言）が近接して並んでいる場合のみ `true` を返す。
#[cfg(test)]
pub(super) fn has_agy_permission_prompt_text(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    match_agy_permission_prompt_lines(&lines).is_some()
}
