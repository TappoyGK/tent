mod app_events;
mod app_handler;
mod app_shortcuts;
mod app_state;
mod browser;
mod daemon;
mod hook_cli;
mod key_map;
mod layout;
mod macos;
mod platform;
mod renderer;
mod slot;
mod state;
mod terminal_pane;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ghostty_vt::key::Encoder;

use crate::platform::{
    ActiveEventLoop, AppProxy, ControlFlow, LogicalPosition, LogicalSize, ModifiersState,
    NativeWindow,
};

pub(crate) use crate::app_state::{
    ClickTracker, DividerDragState, SEARCH_RESYNC_INTERVAL, SearchState, SearchTarget,
    SelectionMode, SelectionState, SlotDragState, StatusTimerState, UrlBarEdit,
    selection_autoscroll_delta,
};
use crate::browser::BrowserManager;
use crate::daemon::{DaemonClient, DaemonEvent, default_socket_path};
use crate::layout::{AppLayout, Rect, SidebarState};
use crate::renderer::Renderer;
use crate::slot::Workspace;
use crate::state::{SavedState, WindowGeometry};
use crate::terminal_pane::{PaneId, UrlMatch};

pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const CWD_POLL_INTERVAL: Duration = Duration::from_millis(1000);
pub(crate) const BATTERY_POLL_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(2000);
pub(crate) const RESTORE_STEP_INTERVAL: Duration = Duration::from_millis(350);
const PTY_DRAIN_BUDGET: Duration = Duration::from_millis(8);
pub(crate) const DAEMON_EVENT_QUEUE_CAPACITY: usize = 64;
pub(crate) const MEMORY_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
pub(crate) const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct TentApp {
    pub(crate) proxy: AppProxy,
    pub(crate) initial_state: Option<SavedState>,
    pub(crate) daemon: Option<DaemonClient>,
    /// 初回接続時に取得した常駐デーモンの PID（メモリ合算用。デーモン異常終了後に PID が再利用される可能性は極めて低いため接続時値を保持）
    pub(crate) daemon_pid: Option<i32>,
    pub(crate) daemon_rx: Option<Receiver<DaemonEvent>>,

    /// `wgpu::Surface` が `NativeWindow` より先に確実に drop されるよう `renderer` を前に置く
    pub(crate) renderer: Option<Renderer>,
    pub(crate) window: Option<Rc<NativeWindow>>,
    pub(crate) workspace: Option<Workspace>,
    pub(crate) browsers: BrowserManager,
    pub(crate) layout: Option<AppLayout>,
    pub(crate) sidebar_state: SidebarState,
    pub(crate) key_encoder: Encoder,

    pub(crate) modifiers: ModifiersState,
    pub(crate) cursor_pos: (f32, f32),
    pub(crate) window_focused: bool,
    pub(crate) last_redraw: Instant,
    pub(crate) last_cwd_poll: Instant,
    pub(crate) last_battery_poll: Instant,
    pub(crate) battery_status: Arc<Mutex<Option<macos::BatteryStatus>>>,
    pub(crate) wifi_status: Arc<Mutex<Option<macos::WifiStatus>>>,
    pub(crate) last_memory_poll: Instant,
    pub(crate) memory_overlay: crate::renderer::MemoryOverlay,
    pub(crate) last_snapshot_poll: Instant,
    pub(crate) last_restore_step: Instant,
    pub(crate) restore_queue: VecDeque<usize>,
    pub(crate) was_any_outputting: bool,
    pub(crate) pending_redraw: bool,
    pub(crate) ime_preedit: String,
    pub(crate) last_ime_area: Option<(i32, i32, u32, u32)>,
    pub(crate) scroll_accum_px: f32,

    pub(crate) slot_drag: Option<SlotDragState>,
    pub(crate) divider_drag: Option<DividerDragState>,
    pub(crate) hovered_divider: Option<crate::slot::SplitAxis>,
    pub(crate) cursor_kind: macos::AppCursorKind,
    pub(crate) selection: Option<SelectionState>,
    pub(crate) last_click: Option<ClickTracker>,
    pub(crate) last_autoscroll: Instant,
    pub(crate) hovered_url: Option<(PaneId, UrlMatch)>,
    pub(crate) url_bar_edit: Option<UrlBarEdit>,
    pub(crate) search: Option<SearchState>,
    pub(crate) show_help: bool,
    pub(crate) restart_requested: bool,
    pub(crate) daemon_version_mismatch: bool,
    pub(crate) daemon_upgrade_confirm_until: Option<Instant>,
    pub(crate) version_copied_until: Option<Instant>,
    pub(crate) status_timer: StatusTimerState,
}

impl TentApp {
    fn new(proxy: AppProxy) -> Self {
        let key_encoder = Encoder::new().expect("failed to create ghostty key encoder");
        let saved = SavedState::load();
        let sidebar_state = saved.sidebar_state;
        let now = Instant::now();
        let browsers = BrowserManager::new(proxy.clone());
        Self {
            proxy,
            initial_state: Some(saved),
            daemon: None,
            daemon_pid: None,
            daemon_rx: None,
            window: None,
            renderer: None,
            workspace: None,
            browsers,
            layout: None,
            sidebar_state,
            key_encoder,
            modifiers: ModifiersState::empty(),
            cursor_pos: (0.0, 0.0),
            window_focused: true,
            last_redraw: now - FRAME_INTERVAL,
            last_cwd_poll: now,
            last_battery_poll: now - BATTERY_POLL_INTERVAL,
            battery_status: Arc::new(Mutex::new(None)),
            wifi_status: Arc::new(Mutex::new(None)),
            last_memory_poll: now - MEMORY_POLL_INTERVAL,
            memory_overlay: crate::renderer::MemoryOverlay::default(),
            last_snapshot_poll: now - SNAPSHOT_POLL_INTERVAL,
            last_restore_step: now,
            restore_queue: VecDeque::new(),
            was_any_outputting: false,
            pending_redraw: false,
            ime_preedit: String::new(),
            last_ime_area: None,
            scroll_accum_px: 0.0,
            slot_drag: None,
            divider_drag: None,
            hovered_divider: None,
            cursor_kind: macos::AppCursorKind::Default,
            selection: None,
            last_click: None,
            last_autoscroll: now - AUTOSCROLL_INTERVAL,
            hovered_url: None,
            url_bar_edit: None,
            search: None,
            show_help: false,
            restart_requested: false,
            daemon_version_mismatch: false,
            daemon_upgrade_confirm_until: None,
            version_copied_until: None,
            status_timer: StatusTimerState::default(),
        }
    }

    pub(crate) fn pane_gap(&self) -> f32 {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        (6.0 * scale).round()
    }

    fn current_window_geometry(&self) -> Option<WindowGeometry> {
        let window = self.window.as_ref()?;
        let scale = window.scale_factor();
        let pos = window.outer_position()?;
        let size = window.inner_size();
        let logical_pos: LogicalPosition<i32> = pos.to_logical(scale);
        let logical_size: LogicalSize<u32> = size.to_logical(scale);
        Some(WindowGeometry {
            x: logical_pos.x,
            y: logical_pos.y,
            width: logical_size.width,
            height: logical_size.height,
        })
    }

    pub(crate) fn focus_terminal_view(&self) {
        if let Some(window) = &self.window {
            window.focus_terminal();
        }
    }

    /// WKWebView の最新 URL（YouTube 等の SPA 遷移含む）を Workspace へ同期する
    pub(crate) fn sync_live_browser_urls(&mut self) -> bool {
        let Some(ws) = self.workspace.as_mut() else {
            return false;
        };
        let mut changed = false;
        for (pid, url) in self.browsers.live_urls() {
            if ws.update_browser_url(pid, url) {
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn save_state(&mut self) {
        self.sync_live_browser_urls();
        if let Some(ws) = &self.workspace {
            let state = SavedState {
                sidebar_state: self.sidebar_state,
                active_slot: ws.active_slot,
                window_geometry: self.current_window_geometry(),
                slots: ws.export_saved_slots(),
            };
            state.save();
        }
    }

    /// 現在のアクティブスロットのブラウザ表示・位置・破棄を WKWebView へ同期する
    pub(crate) fn sync_browsers(&mut self) {
        let (Some(window), Some(ws)) = (&self.window, &self.workspace) else {
            return;
        };
        let scale = window.scale_factor();
        let live_ids = ws.all_browser_ids();
        if self.browsers.retain_ids(&live_ids) {
            // 最後の表示中 WebView を閉じた直後でも TentMetalView のカーソル矩形を張り直す
            window.invalidate_cursor_rects();
        }
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.retain_browser_textures(&live_ids);
        }

        if self.show_help {
            // ヘルプダイアログ表示中は WKWebView を一時的に非表示にして前面に描画する
            self.browsers.sync(window, &[], scale);
            window.focus_terminal();
            return;
        }

        let visible = ws.visible_browsers();
        let refs: Vec<(PaneId, Rect, &str)> = visible
            .iter()
            .map(|(id, rect, url)| (*id, *rect, url.as_str()))
            .collect();
        self.browsers.sync(window, &refs, scale);

        if self.url_bar_edit.is_none() && self.search.is_none() {
            if let Some(b) = ws.focused_browser() {
                self.browsers.focus_webview(b.id);
            } else {
                window.focus_terminal();
            }
        }
    }

    pub(crate) fn drain_daemon_events(&mut self) -> bool {
        let (Some(rx), Some(workspace)) = (self.daemon_rx.as_ref(), self.workspace.as_mut()) else {
            return false;
        };
        let start = Instant::now();
        let mut changed = false;
        let mut pane_exited = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                DaemonEvent::Output { pane_id, data } => {
                    workspace.feed_output(pane_id, &data, self.window_focused);
                    changed = true;
                }
                DaemonEvent::Exited { pane_id } => {
                    workspace.handle_pane_exited(pane_id);
                    if self
                        .selection
                        .as_ref()
                        .is_some_and(|s| s.pane_id == pane_id)
                    {
                        self.selection = None;
                    }
                    if self
                        .search
                        .as_ref()
                        .is_some_and(|s| s.target.pane_id() == pane_id)
                    {
                        self.search = None;
                    }
                    pane_exited = true;
                    changed = true;
                }
                DaemonEvent::CwdList(list) => {
                    if workspace.apply_cwd_updates(list) {
                        changed = true;
                    }
                }
                DaemonEvent::AiToolList(list) => {
                    if workspace.apply_ai_tool_updates(list) {
                        changed = true;
                    }
                }
                DaemonEvent::HookEvent {
                    pane_id,
                    state,
                    tool,
                } => {
                    if workspace.apply_hook_event(pane_id, state, tool, self.window_focused) {
                        changed = true;
                    }
                }
                DaemonEvent::Resync(rp) => {
                    if workspace.apply_resync(rp) {
                        changed = true;
                    }
                }
                DaemonEvent::Replay(_) | DaemonEvent::HelloDone { .. } => {}
            }
            if start.elapsed() >= PTY_DRAIN_BUDGET {
                self.pending_redraw = true;
                break;
            }
        }
        if pane_exited {
            self.sync_browsers();
        }
        changed
    }

    pub(crate) fn recompute_layout_and_resize(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let scale = window.scale_factor() as f32;
        let gap = (6.0 * scale).round();
        let layout = AppLayout::compute(size.width, size.height, scale, self.sidebar_state);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(size.width, size.height, scale);
            let cw = renderer.cell_w;
            let ch = renderer.cell_h;
            if let Some(ws) = self.workspace.as_mut() {
                ws.resize_all(layout.main_inner_rect, gap, cw, ch);
            }
        }
        self.layout = Some(layout);
        if self.url_bar_edit.is_some()
            && let Some(rect) = self.current_url_bar_rect()
            && let Some(window) = &self.window
        {
            window.reposition_url_bar(rect);
        }
        self.sync_browsers();
        self.request_redraw_now();
    }

    pub(crate) fn request_redraw_now(&mut self) {
        self.pending_redraw = false;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    pub(crate) fn request_redraw_throttled(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_redraw) >= FRAME_INTERVAL {
            self.request_redraw_now();
        } else {
            self.pending_redraw = true;
        }
    }

    pub(crate) fn update_control_flow(&self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.pending_redraw {
            let next = self.last_redraw + FRAME_INTERVAL;
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
            return;
        }
        let mut wake_at = self.last_cwd_poll + CWD_POLL_INTERVAL;
        if let Some(ws) = &self.workspace
            && ws.has_any_active_output(now)
        {
            wake_at = wake_at.min(now + Duration::from_millis(400));
        }
        if self.browsers.has_any_views() {
            wake_at = wake_at.min(self.last_snapshot_poll + SNAPSHOT_POLL_INTERVAL);
        }
        if !self.restore_queue.is_empty() {
            wake_at = wake_at.min(self.last_restore_step + RESTORE_STEP_INTERVAL);
        }
        if self.status_timer.end_at.is_some() {
            let next_sand = self
                .status_timer
                .last_anim_step
                .map(|t| t + Duration::from_millis(150))
                .unwrap_or(now);
            wake_at = wake_at.min(next_sand);
        }
        if self.can_autoscroll_selection() {
            wake_at = wake_at.min((self.last_autoscroll + AUTOSCROLL_INTERVAL).max(now));
        }
        if let Some(due) = self.search_resync_due_at() {
            wake_at = wake_at.min(due.max(now));
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
    }

    /// 出力・リサイズ等により再検索が保留中であれば、次に自動再検索すべき時刻を返す
    pub(crate) fn search_resync_due_at(&self) -> Option<Instant> {
        let search = self.search.as_ref()?;
        let SearchTarget::Terminal(pid) = search.target else {
            return None;
        };
        if search.query.is_empty() {
            return None;
        }
        let pane = self
            .workspace
            .as_ref()?
            .active_slot_ref()
            .root
            .as_ref()?
            .terminal_pane(pid)?;
        if pane.content_generation == search.content_generation {
            None
        } else {
            Some(search.last_sync_at + SEARCH_RESYNC_INTERVAL)
        }
    }

    pub(crate) fn can_autoscroll_selection(&self) -> bool {
        let Some(sel) = &self.selection else {
            return false;
        };
        if !sel.active || !sel.has_dragged {
            return false;
        }
        let cell_h = self.renderer.as_ref().map(|r| r.cell_h).unwrap_or(20.0);
        if selection_autoscroll_delta(self.cursor_pos.1, sel.pane_rect, cell_h) == 0 {
            return false;
        }
        self.workspace
            .as_ref()
            .and_then(|ws| ws.active_slot_ref().root.as_ref())
            .and_then(|root| root.terminal_pane(sel.pane_id))
            .is_some_and(|pane| !pane.is_alt_screen())
    }

    pub(crate) fn pixel_to_cell(&self, pane_rect: Rect, px: f32, py: f32) -> (u16, u16) {
        let Some(renderer) = &self.renderer else {
            return (0, 0);
        };
        let col = ((px - pane_rect.x) / renderer.cell_w).floor().max(0.0) as u16;
        let row = ((py - pane_rect.y) / renderer.cell_h).floor().max(0.0) as u16;
        (col, row)
    }

    pub(crate) fn extend_selection_to_cursor(&mut self, px: f32, py: f32) -> bool {
        let Some(sel) = self.selection.as_ref() else {
            return false;
        };
        if !sel.active {
            return false;
        }
        let pid = sel.pane_id;
        let rect = sel.pane_rect;
        let mode = sel.mode;
        let vp_cell = self.pixel_to_cell(rect, px, py);

        let computed = self
            .workspace
            .as_ref()
            .and_then(|ws| ws.active_slot_ref().root.as_ref())
            .and_then(|root| root.terminal_pane(pid))
            .and_then(|pane| {
                let (anchor_start, anchor_end) =
                    pane.resolve_tracked_selection_range(&sel.anchor_start, &sel.anchor_end)?;
                let cur_range_old = pane.resolve_tracked_selection_range(&sel.start, &sel.end);
                let (col, srow) = pane.viewport_cell_to_screen(vp_cell);
                let (new_start, new_end) = match mode {
                    SelectionMode::Char => (anchor_start, (col, srow)),
                    SelectionMode::Word => {
                        let (ws, we) = pane.word_bounds_at_screen(col, srow);
                        let cur_start = (ws, srow);
                        let cur_end = (we, srow);
                        if (cur_start.1, cur_start.0) < (anchor_start.1, anchor_start.0) {
                            (cur_start, anchor_end)
                        } else {
                            (anchor_start, cur_end)
                        }
                    }
                    SelectionMode::Line => {
                        let (ls, le) = pane.line_bounds_at_screen(srow);
                        if ls.1 < anchor_start.1 {
                            (ls, anchor_end)
                        } else {
                            (anchor_start, le)
                        }
                    }
                };
                if cur_range_old == Some((new_start, new_end)) {
                    return Some(None);
                }
                let start_pin = pane.track_screen_point(new_start)?;
                let end_pin = pane.track_screen_point(new_end)?;
                Some(Some((start_pin, end_pin)))
            });

        match computed {
            Some(Some((new_start, new_end))) => {
                if let Some(sel_mut) = self.selection.as_mut() {
                    sel_mut.start = new_start;
                    sel_mut.end = new_end;
                    return true;
                }
                false
            }
            Some(None) => false,
            None => {
                // アンカーがスクロールバックパージや CSI 3 J で消滅した場合は選択を解除
                self.selection = None;
                true
            }
        }
    }

    pub(crate) fn update_url_hover(&mut self) {
        let next = if self.modifiers.super_key() && self.slot_drag.is_none() {
            let (px, py) = self.cursor_pos;
            let hit = self.workspace.as_ref().and_then(|ws| ws.pane_at(px, py));
            if let Some((pid, prect)) = hit {
                let (col, row) = self.pixel_to_cell(prect, px, py);
                self.workspace
                    .as_mut()
                    .and_then(|ws| ws.active_slot_mut().root.as_mut())
                    .and_then(|root| root.terminal_pane_mut(pid))
                    .and_then(|pane| pane.url_at_cell(col, row))
                    .map(|m| (pid, m))
            } else {
                None
            }
        } else {
            None
        };

        if self.hovered_url != next {
            self.hovered_url = next;
            self.update_cursor_icon();
            self.request_redraw_now();
        }
    }

    pub(crate) fn update_cursor_icon(&mut self) {
        let axis = self
            .divider_drag
            .as_ref()
            .map(|d| d.axis)
            .or(self.hovered_divider);
        let next_kind = match axis {
            Some(crate::slot::SplitAxis::Vertical) => macos::AppCursorKind::EwResize,
            Some(crate::slot::SplitAxis::Horizontal) => macos::AppCursorKind::NsResize,
            None if self.hovered_url.is_some() => macos::AppCursorKind::Pointer,
            None => macos::AppCursorKind::Default,
        };
        if self.cursor_kind != next_kind {
            self.cursor_kind = next_kind;
            if let Some(window) = &self.window {
                window.set_cursor_kind(next_kind);
            }
        }
    }
}

pub(crate) fn spawn_replacement_instance() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Err(err) = cmd.spawn() {
        eprintln!("TenT: failed to restart: {err}");
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("[{ts}] TenT panic: {info}\n\nBacktrace:\n{bt}\n---\n");
        eprint!("{msg}");
        let log_path = macos::panic_log_path();
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = f.write_all(msg.as_bytes());
        }
    }));

    // `--daemon [socket_path]` / `hook <working|waiting|idle> [--tool claude|agy|codex]`
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--daemon") => {
            let sock_path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(default_socket_path);
            if let Err(err) = daemon::run_daemon_server(&sock_path) {
                eprintln!("TenT daemon error: {err}");
                std::process::exit(1);
            }
            return;
        }
        Some("hook") => {
            hook_cli::run_hook_cli(args);
            return;
        }
        _ => {}
    }

    let proxy = AppProxy::new();
    let app = TentApp::new(proxy.clone());
    platform::run_app(proxy, app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_overlay_budget_threshold_and_formatting() {
        use crate::renderer::MemoryOverlay;

        // 0 バイトは空ラベル・非超過
        let empty = MemoryOverlay::from_bytes(0);
        assert_eq!(empty.label, "");
        assert!(!empty.is_over_budget);

        // 512 MiB = フルスケール(1280 MiB)の 40%・非超過
        let normal = MemoryOverlay::from_bytes(512 * 1024 * 1024);
        assert_eq!(normal.label, "512M");
        assert!(!normal.is_over_budget);
        assert!((normal.fraction - 0.40).abs() < 0.01);

        // 1.0 GiB（ちょうど 80% ライン）はまだ赤にならない
        let at_budget = MemoryOverlay::from_bytes(MemoryOverlay::BUDGET_BYTES);
        assert_eq!(at_budget.label, "1.0G");
        assert!(!at_budget.is_over_budget);
        assert!((at_budget.fraction - 0.80).abs() < 0.01);

        // 1.0 GiB 超過で赤警告（is_over_budget == true）
        let over = MemoryOverlay::from_bytes(MemoryOverlay::BUDGET_BYTES + 1);
        assert!(over.is_over_budget);

        // 1.5 GiB は "1.5G" 表記で fraction は 1.0 にクランプ
        let huge = MemoryOverlay::from_bytes(1536 * 1024 * 1024);
        assert_eq!(huge.label, "1.5G");
        assert!(huge.is_over_budget);
        assert_eq!(huge.fraction, 1.0);

        // 自プロセスの物理メモリが macOS で取得できること
        assert!(macos::get_pid_memory_bytes(std::process::id() as i32).is_some());
    }
}
