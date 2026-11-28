use crate::terminal_pane::PaneId;

/// マウス選択範囲
#[derive(Clone, Copy, Debug)]
pub struct SelectionOverlay {
    pub pane_id: PaneId,
    pub start: (u16, u16),
    pub end: (u16, u16),
}

/// ホバー中 URL の下線ハイライト範囲
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UrlHoverOverlay {
    pub pane_id: PaneId,
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

/// ステータスバー横長タイマーの表示状態
#[derive(Clone, Debug, Default)]
pub struct TimerOverlay {
    /// 残りまたは設定中の割合（0.0 = 0分, 1.0 = 60分）
    pub fraction: f32,
    /// タイマー設定時間に対する残り割合（1.0 = 開始直後, 0.0 = 終了。砂時計用）
    pub progress: f32,
    /// 表示する分数ラベル（例: "25m"、停止中は空）
    pub label: String,
    pub is_dragging: bool,
    pub is_running: bool,
    pub is_finished: bool,
    /// 砂粒落下アニメーション用フレームカウンタ
    pub anim_tick: u32,
}

/// ペイン内検索のビューポート内ハイライト一覧
#[derive(Clone, Debug)]
pub struct SearchHighlightOverlay {
    pub pane_id: PaneId,
    /// `(vp_row, start_col, end_col, is_current)`
    pub matches: Vec<(u16, u16, u16, bool)>,
}

/// ペイン内検索バーの表示状態
#[derive(Clone, Debug)]
pub struct SearchBarOverlay<'a> {
    pub pane_id: PaneId,
    pub is_browser: bool,
    pub query: &'a str,
    pub select_all: bool,
    pub count_label: String,
}

/// ステータスバーのメモリ使用量バー表示状態
/// 80% ライン = 1.0 GiB（GPU 込みの実測目安）、100% フルスケール = 1.25 GiB
#[derive(Clone, Debug, Default)]
pub struct MemoryOverlay {
    /// フルスケール（1.25 GiB）に対する使用率（0.0..=1.0）
    pub fraction: f32,
    /// 80% 警告ライン（1.0 GiB）を超過しているか
    pub is_over_budget: bool,
    /// バー中央に表示する現在値ラベル（例: "480M", "1.2G"）
    pub label: String,
}

impl MemoryOverlay {
    /// ターミナル＋デーモン合計（GPU/Metal フットプリント込み）の基準ライン（1.0 GiB = バーの 80%）
    pub const BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
    /// バーの 100% フルスケール（1.25 GiB = 1.0 GiB / 0.8）
    pub const FULL_SCALE_BYTES: u64 = 1280 * 1024 * 1024;

    pub fn from_bytes(bytes: u64) -> Self {
        if bytes == 0 {
            return Self::default();
        }
        let fraction = (bytes as f64 / Self::FULL_SCALE_BYTES as f64).clamp(0.0, 1.0) as f32;
        Self {
            fraction,
            is_over_budget: bytes > Self::BUDGET_BYTES,
            label: crate::macos::format_memory_bytes(bytes),
        }
    }
}

/// 描画時のオーバーレイ状態（IME・D&D・テキスト選択・URLホバー・URLバー編集・検索・システム状態・ヘルプ）
pub struct RenderOverlays<'a> {
    pub ime_preedit: &'a str,
    pub drag_source_slot: Option<usize>,
    pub drag_hover_slot: Option<usize>,
    pub selection: Option<SelectionOverlay>,
    pub hovered_url: Option<UrlHoverOverlay>,
    pub url_bar_editing: Option<(&'a str, bool)>,
    pub search_highlights: Option<SearchHighlightOverlay>,
    pub search_bar: Option<SearchBarOverlay<'a>>,
    pub battery: Option<crate::macos::BatteryStatus>,
    pub wifi: Option<crate::macos::WifiStatus>,
    pub show_help: bool,
    pub daemon_version_mismatch: bool,
    pub daemon_upgrade_confirm: bool,
    pub version_copied: bool,
    pub timer: TimerOverlay,
    pub memory: &'a MemoryOverlay,
}

/// 1フレーム描画の戻り値
pub struct RenderOutcome {
    pub cursor_area: Option<(f32, f32, f32, f32)>,
    pub needs_redraw: bool,
}
