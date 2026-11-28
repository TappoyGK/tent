use std::time::{Duration, Instant};

use crate::layout::Rect;
use crate::terminal_pane::{PaneId, SearchMatch, TrackedGridRef};

/// 出力継続中の自動再検索スロットル間隔（毎フレーム全走査による描画詰まりを防止）
pub(crate) const SEARCH_RESYNC_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionMode {
    Char,
    Word,
    Line,
}

pub(crate) struct ClickTracker {
    pub(crate) time: Instant,
    pub(crate) pane_id: PaneId,
    pub(crate) pos: (f32, f32),
    pub(crate) count: u8,
}

pub(crate) struct SlotDragState {
    pub(crate) source_slot: usize,
    pub(crate) start_pos: (f32, f32),
    pub(crate) is_dragging: bool,
    pub(crate) hover_slot: Option<usize>,
}

pub(crate) struct DividerDragState {
    pub(crate) path: Vec<bool>,
    pub(crate) axis: crate::slot::SplitAxis,
    pub(crate) parent_rect: Rect,
}

pub(crate) struct SelectionState {
    pub(crate) pane_id: PaneId,
    pub(crate) pane_rect: Rect,
    pub(crate) mode: SelectionMode,
    pub(crate) anchor_start: TrackedGridRef,
    pub(crate) anchor_end: TrackedGridRef,
    pub(crate) start: TrackedGridRef,
    pub(crate) end: TrackedGridRef,
    pub(crate) active: bool,
    pub(crate) has_dragged: bool,
}

/// ドラッグ選択中のカーソル Y 座標がペインの上下端より外に出たとき、自動スクロール行数（負=上, 正=下）を返す
pub(crate) fn selection_autoscroll_delta(py: f32, pane_rect: Rect, cell_h: f32) -> isize {
    if pane_rect.h <= 1.0 {
        return 0;
    }
    let ch = cell_h.max(1.0);
    if py < pane_rect.y {
        let overshoot = ((pane_rect.y - py) / ch).floor().max(0.0) as isize;
        -(1 + overshoot).min(4)
    } else if py > pane_rect.bottom() {
        let overshoot = ((py - pane_rect.bottom()) / ch).floor().max(0.0) as isize;
        (1 + overshoot).min(4)
    } else {
        0
    }
}

pub(crate) struct UrlBarEdit {
    pub(crate) pane_id: PaneId,
    pub(crate) text: String,
    pub(crate) select_all: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchTarget {
    Terminal(PaneId),
    Browser(PaneId),
}

impl SearchTarget {
    pub(crate) fn pane_id(self) -> PaneId {
        match self {
            Self::Terminal(id) | Self::Browser(id) => id,
        }
    }

    pub(crate) fn is_browser(self) -> bool {
        matches!(self, Self::Browser(_))
    }
}

pub(crate) struct SearchState {
    pub(crate) target: SearchTarget,
    pub(crate) query: String,
    pub(crate) select_all: bool,
    pub(crate) matches: Vec<SearchMatch>,
    pub(crate) current_match: Option<usize>,
    /// 現在マッチ開始位置を追跡するピン（スクロールバック削除・リフロー追従用）
    pub(crate) current_pin: Option<TrackedGridRef>,
    /// 最後に検索を実行した時点の `TerminalPane::content_generation`
    pub(crate) content_generation: u64,
    /// 最後に再検索を実行した時刻（出力継続中のスロットル用）
    pub(crate) last_sync_at: Instant,
}

#[derive(Default)]
pub(crate) struct StatusTimerState {
    pub(crate) dragging: bool,
    pub(crate) drag_rect: Rect,
    pub(crate) drag_minutes: u32,
    pub(crate) last_minutes: u32,
    pub(crate) total_secs: f32,
    pub(crate) end_at: Option<Instant>,
    pub(crate) finished: bool,
    pub(crate) last_anim_step: Option<Instant>,
    pub(crate) anim_tick: u32,
}

impl StatusTimerState {
    pub(crate) fn start(&mut self, mins: u32, now: Instant) {
        if mins > 0 {
            let secs = u64::from(mins) * 60;
            self.last_minutes = mins;
            self.total_secs = secs as f32;
            self.end_at = Some(now + Duration::from_secs(secs));
            self.last_anim_step = Some(now);
            self.anim_tick = 0;
            self.finished = false;
        } else {
            self.total_secs = 0.0;
            self.end_at = None;
            self.last_anim_step = None;
            self.finished = false;
        }
    }

    /// 右端=0分、左端=60分としてカーソル X 座標から分数を算出する。
    /// 通常時は 5 / 15 / 25 / 45 分付近（±2.5分）でスナップし、Shift 押下時は 1 分刻みで自由設定する。
    pub(crate) fn minutes_from_x(rect: Rect, px: f32, disable_snap: bool) -> u32 {
        if rect.w <= 0.0 {
            return 0;
        }
        let ratio = ((rect.right() - px) / rect.w).clamp(0.0, 1.0);
        let raw = ratio * 60.0;
        if !disable_snap {
            for snap in [5.0f32, 15.0, 25.0, 45.0] {
                if (raw - snap).abs() <= 2.5 {
                    return snap as u32;
                }
            }
        }
        raw.round().clamp(0.0, 60.0) as u32
    }

    pub(crate) fn overlay(&self, now: Instant) -> crate::renderer::TimerOverlay {
        if self.dragging {
            let m = self.drag_minutes;
            return crate::renderer::TimerOverlay {
                fraction: (m as f32) / 60.0,
                progress: 1.0,
                label: if m > 0 {
                    format!("{m}m")
                } else {
                    String::new()
                },
                is_dragging: true,
                is_running: false,
                is_finished: false,
                anim_tick: 0,
            };
        }
        if let Some(end) = self.end_at
            && now < end
        {
            let rem_secs = end.duration_since(now).as_secs_f32();
            let rem_mins = (rem_secs / 60.0).ceil() as u32;
            let progress = if self.total_secs > 0.0 {
                (rem_secs / self.total_secs).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return crate::renderer::TimerOverlay {
                fraction: (rem_secs / 3600.0).clamp(0.0, 1.0),
                progress,
                label: format!("{rem_mins}m"),
                is_dragging: false,
                is_running: true,
                is_finished: false,
                anim_tick: self.anim_tick,
            };
        }
        if self.finished {
            return crate::renderer::TimerOverlay {
                fraction: 0.0,
                progress: 0.0,
                label: "0m".to_string(),
                is_dragging: false,
                is_running: false,
                is_finished: true,
                anim_tick: 0,
            };
        }
        crate::renderer::TimerOverlay::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_timer_snap_and_shift_override() {
        let rect = Rect::new(100.0, 0.0, 120.0, 20.0); // 2px = 1分, right=220 (0m), left=100 (60m)
        // 右端 = 0分、左端 = 60分
        assert_eq!(StatusTimerState::minutes_from_x(rect, 220.0, false), 0);
        assert_eq!(StatusTimerState::minutes_from_x(rect, 100.0, false), 60);

        // 23分相当 (220 - 46 = 174) は通常ドラッグで 25分にスナップ
        assert_eq!(StatusTimerState::minutes_from_x(rect, 174.0, false), 25);
        // Shift+ドラッグでスナップ無効 → 23分のまま
        assert_eq!(StatusTimerState::minutes_from_x(rect, 174.0, true), 23);

        // 5 / 15 / 45 分のスナップ確認
        assert_eq!(StatusTimerState::minutes_from_x(rect, 212.0, false), 5); // 4分 → 5分
        assert_eq!(StatusTimerState::minutes_from_x(rect, 188.0, false), 15); // 16分 → 15分
        assert_eq!(StatusTimerState::minutes_from_x(rect, 132.0, false), 45); // 44分 → 45分
    }

    #[test]
    fn test_selection_autoscroll_delta() {
        let pane = Rect::new(0.0, 100.0, 400.0, 300.0); // y: 100..400
        let cell_h = 20.0;

        // ペイン内部（最上行の上半分・最下行の下半分を含む）ではスクロールしない
        assert_eq!(selection_autoscroll_delta(100.0, pane, cell_h), 0);
        assert_eq!(selection_autoscroll_delta(105.0, pane, cell_h), 0);
        assert_eq!(selection_autoscroll_delta(250.0, pane, cell_h), 0);
        assert_eq!(selection_autoscroll_delta(395.0, pane, cell_h), 0);
        assert_eq!(selection_autoscroll_delta(400.0, pane, cell_h), 0);

        // ペイン上端の外は負方向（上スクロール）
        assert_eq!(selection_autoscroll_delta(95.0, pane, cell_h), -1);
        assert!(selection_autoscroll_delta(60.0, pane, cell_h) < -1);
        assert_eq!(selection_autoscroll_delta(-500.0, pane, cell_h), -4);

        // ペイン下端の外は正方向（下スクロール）
        assert_eq!(selection_autoscroll_delta(405.0, pane, cell_h), 1);
        assert!(selection_autoscroll_delta(440.0, pane, cell_h) > 1);
        assert_eq!(selection_autoscroll_delta(1000.0, pane, cell_h), 4);
    }
}
