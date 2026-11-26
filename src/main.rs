mod app_events;
mod app_shortcuts;
mod browser;
mod daemon;
mod key_map;
mod layout;
mod macos;
mod renderer;
mod slot;
mod state;
mod terminal_pane;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ghostty_vt::key::Encoder;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Window, WindowId};

use crate::browser::{BrowserManager, current_macos_modifiers, install_macos_shortcut_monitor};
use crate::daemon::{DaemonClient, DaemonEvent, default_socket_path};
use crate::layout::{AppLayout, Rect, SidebarState};
use crate::renderer::Renderer;
use crate::slot::Workspace;
use crate::state::{SavedState, WindowGeometry};
use crate::terminal_pane::{AppEvent, PaneId, UrlMatch};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const CWD_POLL_INTERVAL: Duration = Duration::from_millis(1000);
const BATTERY_POLL_INTERVAL: Duration = Duration::from_secs(30);
const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const RESTORE_STEP_INTERVAL: Duration = Duration::from_millis(350);
const PTY_DRAIN_BUDGET: Duration = Duration::from_millis(8);

struct SlotDragState {
    source_slot: usize,
    start_pos: (f32, f32),
    is_dragging: bool,
    hover_slot: Option<usize>,
}

struct SelectionState {
    pane_id: PaneId,
    pane_rect: Rect,
    start: (u16, u16),
    end: (u16, u16),
    active: bool,
}

struct UrlBarEdit {
    pane_id: PaneId,
    text: String,
    select_all: bool,
}

#[derive(Default)]
struct StatusTimerState {
    dragging: bool,
    drag_rect: Rect,
    drag_minutes: u32,
    last_minutes: u32,
    total_secs: f32,
    end_at: Option<Instant>,
    finished: bool,
    last_anim_step: Option<Instant>,
    anim_tick: u32,
}

impl StatusTimerState {
    fn start(&mut self, mins: u32, now: Instant) {
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
    fn minutes_from_x(rect: Rect, px: f32, disable_snap: bool) -> u32 {
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

    fn overlay(&self, now: Instant) -> crate::renderer::TimerOverlay {
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

struct TentApp {
    proxy: EventLoopProxy<AppEvent>,
    initial_state: Option<SavedState>,
    daemon: Option<DaemonClient>,
    daemon_rx: Option<Receiver<DaemonEvent>>,

    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    workspace: Option<Workspace>,
    browsers: BrowserManager,
    layout: Option<AppLayout>,
    sidebar_state: SidebarState,
    key_encoder: Encoder,

    modifiers: ModifiersState,
    cursor_pos: (f32, f32),
    window_focused: bool,
    last_redraw: Instant,
    last_cwd_poll: Instant,
    last_battery_poll: Instant,
    battery_status: Arc<Mutex<Option<macos::BatteryStatus>>>,
    wifi_status: Arc<Mutex<Option<macos::WifiStatus>>>,
    last_snapshot_poll: Instant,
    last_restore_step: Instant,
    restore_queue: VecDeque<usize>,
    was_any_outputting: bool,
    pending_redraw: bool,
    ime_preedit: String,
    last_ime_area: Option<(i32, i32, u32, u32)>,
    scroll_accum_px: f32,

    slot_drag: Option<SlotDragState>,
    selection: Option<SelectionState>,
    hovered_url: Option<(PaneId, UrlMatch)>,
    url_bar_edit: Option<UrlBarEdit>,
    show_help: bool,
    restart_requested: bool,
    daemon_version_mismatch: bool,
    daemon_upgrade_confirm_until: Option<Instant>,
    version_copied_until: Option<Instant>,
    status_timer: StatusTimerState,
}

impl TentApp {
    fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        let key_encoder = Encoder::new().expect("failed to create ghostty key encoder");
        let saved = SavedState::load();
        let sidebar_state = saved.sidebar_state;
        let now = Instant::now();
        let browsers = BrowserManager::new(proxy.clone());
        Self {
            proxy,
            initial_state: Some(saved),
            daemon: None,
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
            last_snapshot_poll: now - SNAPSHOT_POLL_INTERVAL,
            last_restore_step: now,
            restore_queue: VecDeque::new(),
            was_any_outputting: false,
            pending_redraw: false,
            ime_preedit: String::new(),
            last_ime_area: None,
            scroll_accum_px: 0.0,
            slot_drag: None,
            selection: None,
            hovered_url: None,
            url_bar_edit: None,
            show_help: false,
            restart_requested: false,
            daemon_version_mismatch: false,
            daemon_upgrade_confirm_until: None,
            version_copied_until: None,
            status_timer: StatusTimerState::default(),
        }
    }

    fn pane_gap(&self) -> f32 {
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
        let pos = window.outer_position().ok()?;
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

    /// WKWebView の最新 URL（YouTube 等の SPA 遷移含む）を Workspace へ同期する
    fn sync_live_browser_urls(&mut self) -> bool {
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

    fn save_state(&mut self) {
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
    fn sync_browsers(&mut self) {
        let (Some(window), Some(ws)) = (&self.window, &self.workspace) else {
            return;
        };
        let scale = window.scale_factor();
        let live_ids = ws.all_browser_ids();
        self.browsers.retain_ids(&live_ids);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.retain_browser_textures(&live_ids);
        }

        if self.show_help {
            // ヘルプダイアログ表示中は WKWebView を一時的に非表示にして前面に描画する
            self.browsers.sync(window, &[], scale);
            self.browsers.focus_parent();
            return;
        }

        let visible = ws.visible_browsers();
        let refs: Vec<(PaneId, Rect, &str)> = visible
            .iter()
            .map(|(id, rect, url)| (*id, *rect, url.as_str()))
            .collect();
        self.browsers.sync(window, &refs, scale);

        if self.url_bar_edit.is_none() {
            if let Some(b) = ws.focused_browser() {
                self.browsers.focus_webview(b.id);
            } else {
                self.browsers.focus_parent();
            }
        }
    }

    fn drain_daemon_events(&mut self) -> bool {
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
                    pane_exited = true;
                    changed = true;
                }
                DaemonEvent::CwdList(list) => {
                    if workspace.apply_cwd_updates(list) {
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

    fn recompute_layout_and_resize(&mut self) {
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
        self.sync_browsers();
        self.request_redraw_now();
    }

    fn request_redraw_now(&mut self) {
        self.pending_redraw = false;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn request_redraw_throttled(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_redraw) >= FRAME_INTERVAL {
            self.request_redraw_now();
        } else {
            self.pending_redraw = true;
        }
    }

    fn update_control_flow(&self, event_loop: &ActiveEventLoop) {
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
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
    }

    fn pixel_to_cell(&self, pane_rect: Rect, px: f32, py: f32) -> (u16, u16) {
        let Some(renderer) = &self.renderer else {
            return (0, 0);
        };
        let col = ((px - pane_rect.x) / renderer.cell_w).floor().max(0.0) as u16;
        let row = ((py - pane_rect.y) / renderer.cell_h).floor().max(0.0) as u16;
        (col, row)
    }

    fn update_url_hover(&mut self) {
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
            if let Some(window) = &self.window {
                let icon = if next.is_some() {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                };
                window.set_cursor(icon);
            }
            self.hovered_url = next;
            self.request_redraw_now();
        }
    }
}

impl ApplicationHandler<AppEvent> for TentApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let saved = self
            .initial_state
            .take()
            .unwrap_or_else(SavedState::default_home);
        self.sidebar_state = saved.sidebar_state;

        let mut attrs = Window::default_attributes().with_title("TenT");
        if let Some(geom) = saved.window_geometry {
            attrs = attrs
                .with_inner_size(LogicalSize::new(
                    f64::from(geom.width),
                    f64::from(geom.height),
                ))
                .with_position(LogicalPosition::new(geom.x, geom.y));
        } else {
            attrs = attrs.with_inner_size(LogicalSize::new(1440.0, 880.0));
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        window.set_ime_allowed(true);

        // Dock / Cmd+Tab に TenT のカスタムアイコンを適用
        macos::install_app_icon();
        // WKWebView フォーカス中でも Cmd+1..0 / Cmd+B / Cmd+L 等を拾えるようにする
        install_macos_shortcut_monitor(self.proxy.clone());
        // Finder ファイル・スクショ右下サムネイル等の D&D を確実に受け取る
        macos::install_file_drop_handler(&window, self.proxy.clone());

        let scale = window.scale_factor() as f32;
        let gap = (6.0 * scale).round();
        let size = window.inner_size();
        let layout = AppLayout::compute(size.width, size.height, scale, self.sidebar_state);

        let renderer =
            Renderer::new(Arc::clone(&window), scale).expect("failed to initialize wgpu renderer");

        // デーモンへ接続（未起動なら自動デタッチ起動）し、生きたペインをすべて再接続
        // 大量出力時も Output / Exited を絶対に欠落させないため無制限チャネルを使用（ドレインは8ms予算で制御）
        let (ev_tx, ev_rx) = mpsc::channel::<DaemonEvent>();
        let proxy_for_daemon = self.proxy.clone();
        let sock_path = default_socket_path();
        let attach = DaemonClient::connect_or_spawn(&sock_path, true, move |ev| {
            let _ = ev_tx.send(ev);
            let _ = proxy_for_daemon.send_event(AppEvent::DaemonReady);
        })
        .expect("failed to connect to tent daemon");

        let workspace = Workspace::from_session(
            layout.main_inner_rect,
            gap,
            renderer.cell_w,
            renderer.cell_h,
            saved.slots,
            saved.active_slot,
            attach.replayed_panes,
            attach.next_pane_id,
            attach.client.clone(),
        );

        let restore_queue: VecDeque<usize> = workspace.slots_needing_restore().into();

        self.daemon = Some(attach.client);
        self.daemon_rx = Some(ev_rx);
        self.daemon_version_mismatch = attach.daemon_version_mismatch;
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.workspace = Some(workspace);
        self.layout = Some(layout);
        self.restore_queue = restore_queue;
        self.last_restore_step = Instant::now();

        self.sync_browsers();
        self.save_state();
        self.request_redraw_now();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.save_state();
        if self.restart_requested {
            spawn_replacement_instance();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::DaemonReady => {
                if self.drain_daemon_events() {
                    self.request_redraw_throttled();
                }
                self.update_control_flow(event_loop);
            }
            AppEvent::Shortcut(shortcut) => {
                self.modifiers = current_macos_modifiers();
                self.handle_shortcut(shortcut);
                if self.restart_requested {
                    event_loop.exit();
                    return;
                }
                self.update_control_flow(event_loop);
            }
            AppEvent::WebViewClicked { px, py } => {
                let mut changed = self.url_bar_edit.take().is_some();
                if let Some(ws) = self.workspace.as_mut()
                    && ws.focus_pane_at(px, py).is_some()
                {
                    changed = true;
                }
                if changed {
                    self.request_redraw_now();
                }
            }
            AppEvent::WebViewHovered { px, py } => {
                if !self.window_focused
                    || self.show_help
                    || self.slot_drag.is_some()
                    || self.url_bar_edit.is_some()
                {
                    return;
                }
                let needs_focus = self.workspace.as_ref().is_some_and(|ws| {
                    ws.pane_at(px, py).is_some_and(|(pid, _)| {
                        ws.active_slot_ref().active_focused_pane() != Some(pid)
                    })
                });
                if needs_focus {
                    if let Some(ws) = self.workspace.as_mut() {
                        ws.focus_pane_at(px, py);
                    }
                    self.sync_browsers();
                    self.request_redraw_now();
                }
            }
            AppEvent::BrowserUrlChanged { pane_id, url } => {
                // サブフレームの URL で上書きされないよう、実メインフレームの URL を優先して同期
                let live_changed = self.sync_live_browser_urls();
                let mut changed = live_changed;
                if !live_changed
                    && (url.starts_with("http://") || url.starts_with("https://"))
                    && let Some(ws) = self.workspace.as_mut()
                    && ws.update_browser_url(pane_id, url)
                {
                    changed = true;
                }
                if changed {
                    self.last_snapshot_poll =
                        Instant::now() - SNAPSHOT_POLL_INTERVAL + Duration::from_millis(500);
                    self.save_state();
                    self.request_redraw_now();
                }
            }
            AppEvent::BrowserTitleChanged { pane_id, title } => {
                let url_changed = self.sync_live_browser_urls();
                let title_changed = self
                    .workspace
                    .as_mut()
                    .is_some_and(|ws| ws.update_browser_title(pane_id, title));
                if url_changed {
                    self.save_state();
                }
                if url_changed || title_changed {
                    self.last_snapshot_poll =
                        Instant::now() - SNAPSHOT_POLL_INTERVAL + Duration::from_millis(300);
                    self.request_redraw_now();
                }
            }
            AppEvent::BrowserSnapshot {
                pane_id,
                width,
                height,
                rgba,
            } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.update_browser_snapshot(pane_id, width, height, &rgba);
                    self.request_redraw_now();
                }
            }
            AppEvent::BrowserAudioState {
                pane_id,
                is_playing,
            } => {
                if let Some(ws) = self.workspace.as_mut()
                    && ws.update_browser_audio(pane_id, is_playing)
                {
                    self.request_redraw_now();
                }
            }
            AppEvent::BrowserOpenNewWindow { pane_id, url } => {
                // target="_blank" や window.open() のリンクは同じブラウザタブ内で開く（戻るで戻れる）
                if let Some(ws) = self.workspace.as_mut() {
                    ws.update_browser_url(pane_id, url.clone());
                }
                self.browsers.navigate(pane_id, &url);
                self.save_state();
                self.request_redraw_now();
            }
            AppEvent::BrowserSwipePreview {
                dir,
                progress,
                ready,
            } => {
                if let Some(b) = self.workspace.as_ref().and_then(Workspace::focused_browser) {
                    self.browsers.set_swipe_preview(b.id, dir, progress, ready);
                }
            }
            AppEvent::DragHover { px, py } => {
                if let Some(ws) = self.workspace.as_mut() {
                    let needs_focus = ws
                        .pane_at(px, py)
                        .is_some_and(|(pid, _)| ws.active_slot_ref().focused_pane != Some(pid));
                    if needs_focus {
                        ws.focus_pane_at(px, py);
                        self.request_redraw_now();
                    }
                }
            }
            AppEvent::DroppedFiles { paths, pos } => {
                if paths.is_empty() {
                    return;
                }
                let mut escaped = String::new();
                for path in &paths {
                    escaped.push_str(&macos::shell_escape_path(path));
                    escaped.push(' ');
                }
                if let Some(ws) = self.workspace.as_mut() {
                    if let Some((px, py)) = pos {
                        ws.focus_pane_at(px, py);
                    }
                    if ws.active_slot_ref().is_dormant() {
                        let idx = ws.active_slot;
                        ws.ensure_slot_started(idx);
                    }
                    if let Some(pane) = ws.active_pane_mut() {
                        pane.terminal
                            .scroll_viewport(ghostty_vt::terminal::ScrollViewport::Bottom);
                        pane.mark_dirty();
                        pane.paste_text(&escaped);
                        self.request_redraw_now();
                    }
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut need_redraw = false;

        if self.drain_daemon_events() {
            self.request_redraw_throttled();
        } else if self.pending_redraw && now.duration_since(self.last_redraw) >= FRAME_INTERVAL {
            need_redraw = true;
        }

        if let Some(ws) = self.workspace.as_mut() {
            if ws.update_waiting_states(now) {
                need_redraw = true;
            }
            let now_outputting = ws.has_any_active_output(now);
            if self.was_any_outputting && !now_outputting {
                need_redraw = true;
            }
            self.was_any_outputting = now_outputting;
        }

        // 起動後の段階的セッション復旧（アクティブスロットの後、残りスロットを1つずつ順次復旧）
        if !self.restore_queue.is_empty()
            && now.duration_since(self.last_restore_step) >= RESTORE_STEP_INTERVAL
            && let Some(slot_idx) = self.restore_queue.pop_front()
        {
            self.last_restore_step = now;
            if let Some(ws) = self.workspace.as_mut() {
                if ws.slots[slot_idx].saved_tree.is_some() && ws.slots[slot_idx].root.is_none() {
                    ws.ensure_slot_started(slot_idx);
                }
                let slot_browsers = ws.browsers_in_slot(slot_idx);
                if let Some(window) = &self.window {
                    let scale = window.scale_factor();
                    for (bid, rect, url) in slot_browsers {
                        self.browsers.warm_up_hidden(window, bid, rect, &url, scale);
                    }
                }
                need_redraw = true;
            }
        }

        if now.duration_since(self.last_cwd_poll) >= CWD_POLL_INTERVAL {
            self.last_cwd_poll = now;
            if let Some(daemon) = &self.daemon {
                daemon.query_cwds();
            }
            if self.browsers.has_any_views() {
                self.browsers.poll_audio_states();
            }
            if self.sync_live_browser_urls() {
                self.save_state();
            }
            if now.duration_since(self.last_battery_poll) >= BATTERY_POLL_INTERVAL {
                self.last_battery_poll = now;
                let batt_slot = Arc::clone(&self.battery_status);
                let wifi_slot = Arc::clone(&self.wifi_status);
                std::thread::spawn(move || {
                    let batt = macos::read_battery_status();
                    if let Ok(mut guard) = batt_slot.lock() {
                        *guard = batt;
                    }
                    let wifi = macos::read_wifi_status();
                    if let Ok(mut guard) = wifi_slot.lock() {
                        *guard = Some(wifi);
                    }
                });
            }
            // 上部ステータスバーの秒表示時計を更新
            need_redraw = true;
        }

        if self.browsers.has_any_views()
            && now.duration_since(self.last_snapshot_poll) >= SNAPSHOT_POLL_INTERVAL
        {
            self.last_snapshot_poll = now;
            if let Some(ws) = &self.workspace {
                let mut target_ids = Vec::new();
                for (i, slot) in ws.slots.iter().enumerate() {
                    if self.sidebar_state.is_col_collapsed(i / 5) {
                        continue;
                    }
                    if slot.view_mode == crate::slot::SlotViewMode::Browser {
                        if let Some(broot) = &slot.browser_root {
                            broot.for_each_browser(&mut |b| target_ids.push(b.id));
                        }
                    } else if let Some(root) = &slot.root {
                        root.for_each_browser(&mut |b| target_ids.push(b.id));
                    }
                }
                self.browsers.capture_snapshots(&target_ids);
            }
        }

        if let Some(deadline) = self.daemon_upgrade_confirm_until
            && now >= deadline
        {
            self.daemon_upgrade_confirm_until = None;
            need_redraw = true;
        }
        if let Some(deadline) = self.version_copied_until
            && now >= deadline
        {
            self.version_copied_until = None;
            need_redraw = true;
        }

        if let Some(end) = self.status_timer.end_at {
            if now >= end {
                self.status_timer.end_at = None;
                self.status_timer.last_anim_step = None;
                self.status_timer.finished = true;
                macos::play_system_bell();
                macos::send_notification(
                    "TenT Timer".to_string(),
                    "タイマーが終了しました".to_string(),
                );
                need_redraw = true;
            } else if self
                .status_timer
                .last_anim_step
                .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(150))
            {
                self.status_timer.last_anim_step = Some(now);
                self.status_timer.anim_tick = self.status_timer.anim_tick.wrapping_add(1);
                need_redraw = true;
            }
        }

        if need_redraw {
            self.request_redraw_now();
        }
        self.update_control_flow(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        self.handle_window_event(event_loop, event);
    }
}

fn spawn_replacement_instance() {
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

    // `--daemon [socket_path]` が指定された場合はバックグラウンドデーモンとして稼働
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--daemon") {
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

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .expect("failed to build event loop");
    let proxy = event_loop.create_proxy();
    let mut app = TentApp::new(proxy);
    event_loop.run_app(&mut app).expect("event loop error");
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
}
