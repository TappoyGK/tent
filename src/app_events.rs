use std::time::Instant;

use ghostty_vt::terminal::ScrollViewport;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use crate::browser::current_macos_modifiers;
use crate::key_map::{AppShortcut, encode_key_event, match_app_shortcut};
use crate::layout::{SidebarHit, StatusBarHit};
use crate::macos;
use crate::renderer::{
    RenderOverlays, SearchBarOverlay, SearchHighlightOverlay, SelectionOverlay, UrlHoverOverlay,
};
use crate::slot::{Workspace, ratio_from_pointer};
use crate::{
    ClickTracker, DividerDragState, MULTI_CLICK_INTERVAL, SelectionMode, SelectionState,
    SlotDragState, TentApp,
};

impl TentApp {
    pub(crate) fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.save_state();
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if !focused && self.divider_drag.take().is_some() {
                    self.hovered_divider = None;
                    self.save_state();
                    self.update_cursor_icon();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.recompute_layout_and_resize();
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
                self.update_url_hover();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let px = position.x as f32;
                let py = position.y as f32;
                self.cursor_pos = (px, py);

                // 分割ペイン境界線のドラッグリサイズ中
                if let Some(drag) = self.divider_drag.as_ref() {
                    let path = drag.path.clone();
                    let axis = drag.axis;
                    let parent_rect = drag.parent_rect;
                    let gap = self.pane_gap();
                    let ratio = ratio_from_pointer(axis, parent_rect, gap, px, py);
                    let changed = self
                        .workspace
                        .as_mut()
                        .is_some_and(|ws| ws.set_active_split_ratio(&path, ratio));
                    if changed {
                        self.sync_browsers();
                        self.request_redraw_now();
                    }
                    return;
                }

                self.update_url_hover();

                // 分割ペイン境界線ホバー時のリサイズカーソル表示
                let is_selecting = self.selection.as_ref().is_some_and(|s| s.active);
                let next_divider = if !self.show_help
                    && self.slot_drag.is_none()
                    && !self.status_timer.dragging
                    && !is_selecting
                {
                    let scale = self
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor() as f32)
                        .unwrap_or(1.0);
                    let hit_half = (self.pane_gap() * 0.5).max(5.0 * scale);
                    self.workspace
                        .as_ref()
                        .and_then(|ws| ws.hit_test_divider(px, py, hit_half))
                        .map(|d| d.axis)
                } else {
                    None
                };
                if self.hovered_divider != next_divider {
                    self.hovered_divider = next_divider;
                    self.update_cursor_icon();
                }

                // Focus-follows-mouse: 分割ペイン上にカーソルが乗ったら即フォーカス切替
                if self.window_focused
                    && !self.show_help
                    && self.slot_drag.is_none()
                    && !is_selecting
                    && self.hovered_divider.is_none()
                    && self.url_bar_edit.is_none()
                    && self.search.is_none()
                {
                    let needs_focus = self.workspace.as_ref().is_some_and(|ws| {
                        ws.pane_at(px, py)
                            .is_some_and(|(pid, _)| ws.active_slot_ref().focused_pane != Some(pid))
                    });
                    if needs_focus {
                        if let Some(ws) = self.workspace.as_mut() {
                            ws.focus_pane_at(px, py);
                        }
                        self.sync_browsers();
                        self.request_redraw_now();
                    }
                }

                if self.status_timer.dragging {
                    let disable_snap = self.modifiers.shift_key();
                    let mins = crate::StatusTimerState::minutes_from_x(
                        self.status_timer.drag_rect,
                        px,
                        disable_snap,
                    );
                    if self.status_timer.drag_minutes != mins {
                        self.status_timer.drag_minutes = mins;
                        self.request_redraw_now();
                    }
                    return;
                }

                if let Some(drag) = self.slot_drag.as_mut() {
                    let dx = px - drag.start_pos.0;
                    let dy = py - drag.start_pos.1;
                    if !drag.is_dragging && (dx * dx + dy * dy) >= 36.0 {
                        drag.is_dragging = true;
                    }
                    if drag.is_dragging {
                        let hover =
                            self.layout
                                .as_ref()
                                .and_then(|l| match l.hit_test_sidebar(px, py) {
                                    Some(SidebarHit::Slot(idx)) => Some(idx),
                                    _ => None,
                                });
                        if drag.hover_slot != hover {
                            drag.hover_slot = hover;
                            self.request_redraw_now();
                        }
                    }
                }

                if let Some(sel) = self.selection.as_mut()
                    && sel.active
                {
                    sel.has_dragged = true;
                    if self.extend_selection_to_cursor(px, py) {
                        self.request_redraw_now();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.show_help {
                    self.show_help = false;
                    self.sync_browsers();
                    self.request_redraw_now();
                    return;
                }
                let (px, py) = self.cursor_pos;
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor() as f32)
                    .unwrap_or(1.0);
                let is_browser_bar = self
                    .workspace
                    .as_ref()
                    .and_then(Workspace::focused_browser)
                    .is_some();

                // 1. メイン上部ステータスバーのボタン・URLバー判定
                if let Some(sb_hit) = self.layout.as_ref().and_then(|l| {
                    l.hit_test_status_bar(
                        px,
                        py,
                        scale,
                        is_browser_bar,
                        self.daemon_version_mismatch,
                    )
                }) {
                    if sb_hit != StatusBarHit::UpgradeDaemon
                        && self.daemon_upgrade_confirm_until.take().is_some()
                    {
                        self.request_redraw_now();
                    }
                    match sb_hit {
                        StatusBarHit::Back => {
                            if let Some(b) =
                                self.workspace.as_ref().and_then(Workspace::focused_browser)
                            {
                                self.browsers.go_back(b.id);
                            }
                        }
                        StatusBarHit::Forward => {
                            if let Some(b) =
                                self.workspace.as_ref().and_then(Workspace::focused_browser)
                            {
                                self.browsers.go_forward(b.id);
                            }
                        }
                        StatusBarHit::Reload => {
                            if let Some(b) =
                                self.workspace.as_ref().and_then(Workspace::focused_browser)
                            {
                                self.browsers.reload(b.id);
                            }
                        }
                        StatusBarHit::UrlBar => {
                            self.start_url_bar_edit();
                        }
                        StatusBarHit::OpenExternal => {
                            if let Some(b) =
                                self.workspace.as_ref().and_then(Workspace::focused_browser)
                            {
                                macos::open_url(&b.url);
                            }
                        }
                        StatusBarHit::ToggleBrowser => {
                            self.handle_shortcut(AppShortcut::ToggleBrowser);
                        }
                        StatusBarHit::SplitBrowser => {
                            self.handle_shortcut(AppShortcut::SplitBrowser);
                        }
                        StatusBarHit::ToggleHelp => {
                            self.handle_shortcut(AppShortcut::ToggleHelp);
                        }
                        StatusBarHit::Version => {
                            macos::write_clipboard_text(env!("TENT_DISPLAY_VERSION"));
                            self.version_copied_until =
                                Some(Instant::now() + std::time::Duration::from_millis(1200));
                            self.request_redraw_now();
                        }
                        StatusBarHit::TimerIcon => {
                            // 砂時計クリック: 前回と同じ分数で即座にタイマーを開始
                            if self.status_timer.last_minutes > 0 {
                                let mins = self.status_timer.last_minutes;
                                self.status_timer.dragging = false;
                                self.status_timer.start(mins, Instant::now());
                                self.request_redraw_now();
                            }
                        }
                        StatusBarHit::Timer => {
                            // 終了（赤表示）状態のクリックは再スタートせず一旦オフに戻す
                            if self.status_timer.finished {
                                self.status_timer.finished = false;
                                self.status_timer.end_at = None;
                                self.status_timer.dragging = false;
                                self.request_redraw_now();
                            } else if let Some(layout) = self.layout.as_ref() {
                                let ctrl = layout.status_bar_controls(
                                    scale,
                                    is_browser_bar,
                                    self.daemon_version_mismatch,
                                );
                                self.status_timer.dragging = true;
                                self.status_timer.drag_rect = ctrl.timer_rect;
                                self.status_timer.drag_minutes =
                                    crate::StatusTimerState::minutes_from_x(
                                        ctrl.timer_rect,
                                        px,
                                        self.modifiers.shift_key(),
                                    );
                                self.request_redraw_now();
                            }
                        }
                        StatusBarHit::UpgradeDaemon => {
                            let now = Instant::now();
                            if self
                                .daemon_upgrade_confirm_until
                                .is_some_and(|deadline| now < deadline)
                            {
                                // 2回目クリックで確認完了 → 状態保存・旧デーモン停止・新プロセス再起動
                                self.daemon_upgrade_confirm_until = None;
                                self.save_state();
                                let _ = crate::daemon::stop_running_daemon(
                                    &crate::daemon::default_socket_path(),
                                );
                                self.restart_requested = true;
                                event_loop.exit();
                            } else {
                                // 1回目クリックは誤爆防止のため 5 秒間だけ確認状態へ移行
                                self.daemon_upgrade_confirm_until =
                                    Some(now + std::time::Duration::from_secs(5));
                                self.request_redraw_now();
                            }
                        }
                    }
                    return;
                }

                // ステータスバー外をクリックしたらアップグレード確認状態を解除
                if self.daemon_upgrade_confirm_until.take().is_some() {
                    self.request_redraw_now();
                }

                // URL バー外をクリックしたら編集終了
                if self.url_bar_edit.take().is_some() {
                    self.request_redraw_now();
                }

                // 2. 右サイドバー判定
                let hit = self
                    .layout
                    .as_ref()
                    .and_then(|l| l.hit_test_sidebar(px, py));
                match hit {
                    Some(SidebarHit::ToggleCol(col)) => {
                        self.sidebar_state.toggle_col(col);
                        self.recompute_layout_and_resize();
                        self.save_state();
                    }
                    Some(SidebarHit::Slot(slot_idx)) => {
                        self.slot_drag = Some(SlotDragState {
                            source_slot: slot_idx,
                            start_pos: (px, py),
                            is_dragging: false,
                            hover_slot: Some(slot_idx),
                        });
                    }
                    None => {
                        // 分割ペイン境界線のドラッグリサイズ開始判定
                        let hit_half = (self.pane_gap() * 0.5).max(5.0 * scale);
                        if let Some(div) = self
                            .workspace
                            .as_ref()
                            .and_then(|ws| ws.hit_test_divider(px, py, hit_half))
                        {
                            self.selection = None;
                            self.divider_drag = Some(DividerDragState {
                                path: div.path,
                                axis: div.axis,
                                parent_rect: div.parent_rect,
                            });
                            self.hovered_divider = Some(div.axis);
                            self.update_cursor_icon();
                            self.request_redraw_now();
                            return;
                        }

                        if self.modifiers.super_key() {
                            self.update_url_hover();
                            if let Some((_, ref m)) = self.hovered_url {
                                let url = m.url.clone();
                                if self.modifiers.shift_key() {
                                    // Cmd+Shift+クリック: 外部デフォルトブラウザで開く
                                    macos::open_url(&url);
                                } else {
                                    // Cmd+クリック（デフォルト）: TenT 組み込みブラウザで開く
                                    if let Some(ws) = self.workspace.as_mut() {
                                        let bid = ws.open_url_in_active_browser(&url);
                                        self.sync_browsers();
                                        self.browsers.navigate(bid, &url);
                                        self.save_state();
                                        self.request_redraw_now();
                                    }
                                }
                                return;
                            }
                        }
                        if let Some(ws) = self.workspace.as_mut()
                            && let Some((pid, prect)) = ws.focus_pane_at(px, py)
                        {
                            let vp_cell = self.pixel_to_cell(prect, px, py);
                            let now = Instant::now();
                            let click_tol = 8.0 * scale;
                            let click_count = match &self.last_click {
                                Some(prev)
                                    if prev.pane_id == pid
                                        && now.duration_since(prev.time)
                                            <= MULTI_CLICK_INTERVAL
                                        && (px - prev.pos.0).hypot(py - prev.pos.1)
                                            <= click_tol =>
                                {
                                    (prev.count % 3) + 1
                                }
                                _ => 1,
                            };
                            self.last_click = Some(ClickTracker {
                                time: now,
                                pane_id: pid,
                                pos: (px, py),
                                count: click_count,
                            });

                            let mode = match click_count {
                                2 => SelectionMode::Word,
                                3 => SelectionMode::Line,
                                _ => SelectionMode::Char,
                            };

                            let selection_bounds = self
                                .workspace
                                .as_ref()
                                .and_then(|ws| ws.active_slot_ref().root.as_ref())
                                .and_then(|r| r.terminal_pane(pid))
                                .and_then(|pane| {
                                    let (col, srow) = pane.viewport_cell_to_screen(vp_cell);
                                    let (start_pt, end_pt) = match mode {
                                        SelectionMode::Char => ((col, srow), (col, srow)),
                                        SelectionMode::Word => {
                                            let (ws_col, we_col) =
                                                pane.word_bounds_at_screen(col, srow);
                                            ((ws_col, srow), (we_col, srow))
                                        }
                                        SelectionMode::Line => pane.line_bounds_at_screen(srow),
                                    };
                                    Some((
                                        pane.track_screen_point(start_pt)?,
                                        pane.track_screen_point(end_pt)?,
                                        pane.track_screen_point(start_pt)?,
                                        pane.track_screen_point(end_pt)?,
                                    ))
                                });
                            if let Some((anchor_start, anchor_end, start, end)) = selection_bounds {
                                self.last_autoscroll = now;
                                self.selection = Some(SelectionState {
                                    pane_id: pid,
                                    pane_rect: prect,
                                    mode,
                                    anchor_start,
                                    anchor_end,
                                    start,
                                    end,
                                    active: true,
                                    has_dragged: false,
                                });
                            }
                            self.sync_browsers();
                            self.request_redraw_now();
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.divider_drag.take().is_some() {
                    let (px, py) = self.cursor_pos;
                    let scale = self
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor() as f32)
                        .unwrap_or(1.0);
                    let hit_half = (self.pane_gap() * 0.5).max(5.0 * scale);
                    self.hovered_divider = self
                        .workspace
                        .as_ref()
                        .and_then(|ws| ws.hit_test_divider(px, py, hit_half))
                        .map(|d| d.axis);
                    self.save_state();
                    self.update_cursor_icon();
                    self.request_redraw_now();
                    return;
                }
                if self.status_timer.dragging {
                    self.status_timer.dragging = false;
                    let mins = self.status_timer.drag_minutes;
                    self.status_timer.start(mins, Instant::now());
                    self.request_redraw_now();
                    return;
                }
                if let Some(drag) = self.slot_drag.take() {
                    if drag.is_dragging {
                        if let Some(target) = drag.hover_slot
                            && target != drag.source_slot
                        {
                            if let Some(ws) = self.workspace.as_mut() {
                                ws.swap_slots(drag.source_slot, target);
                            }
                            self.sync_browsers();
                            self.save_state();
                        }
                    } else {
                        if let Some(ws) = self.workspace.as_mut() {
                            ws.select_slot(drag.source_slot);
                            self.selection = None;
                        }
                        self.sync_browsers();
                        self.save_state();
                    }
                    self.request_redraw_now();
                }

                if let Some(sel) = self.selection.as_mut()
                    && sel.active
                {
                    sel.active = false;
                    let empty_char_click = if sel.mode == SelectionMode::Char {
                        if !sel.has_dragged {
                            true
                        } else {
                            self.workspace
                                .as_ref()
                                .and_then(|ws| ws.active_slot_ref().root.as_ref())
                                .and_then(|r| r.terminal_pane(sel.pane_id))
                                .is_none_or(|pane| {
                                    pane.resolve_tracked_point(&sel.start)
                                        == pane.resolve_tracked_point(&sel.end)
                                })
                        }
                    } else {
                        false
                    };
                    if empty_char_click {
                        self.selection = None;
                        self.request_redraw_now();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let cell_h = self
                    .renderer
                    .as_ref()
                    .map(|r| r.cell_h)
                    .unwrap_or(24.0)
                    .max(1.0);
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -(y * 3.0).round() as isize,
                    MouseScrollDelta::PixelDelta(pos) => {
                        self.scroll_accum_px += pos.y as f32;
                        let step = (self.scroll_accum_px / cell_h).trunc() as isize;
                        self.scroll_accum_px -= (step as f32) * cell_h;
                        -step
                    }
                };
                if lines != 0 {
                    let (px, py) = self.cursor_pos;
                    let cell = self
                        .workspace
                        .as_ref()
                        .and_then(|ws| ws.pane_at(px, py))
                        .map(|(_, rect)| self.pixel_to_cell(rect, px, py))
                        .unwrap_or((0, 0));
                    if let Some(ws) = self.workspace.as_mut()
                        && let Some(pane) = ws.active_pane_mut()
                    {
                        pane.handle_scroll(lines, cell.0, cell.1);
                    }
                    self.request_redraw_now();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.modifiers = current_macos_modifiers();
                if self.show_help {
                    if event.state == ElementState::Pressed
                        && (matches!(event.logical_key, Key::Named(NamedKey::Escape))
                            || matches!(
                                match_app_shortcut(&event, self.modifiers),
                                Some(AppShortcut::ToggleHelp)
                            ))
                    {
                        self.show_help = false;
                        self.sync_browsers();
                        self.request_redraw_now();
                    }
                    return;
                }
                if !self.ime_preedit.is_empty() {
                    return;
                }
                if self.url_bar_edit.is_some() {
                    // URL バー編集中でもスロット切替等のアプリショートカットは通す
                    if let Some(shortcut) = match_app_shortcut(&event, self.modifiers)
                        && !matches!(
                            shortcut,
                            AppShortcut::Copy | AppShortcut::Paste | AppShortcut::FocusUrlBar
                        )
                    {
                        self.handle_shortcut(shortcut);
                        return;
                    }
                    self.handle_url_bar_key(&event);
                    return;
                }
                if self.search.is_some() {
                    // 検索バー編集中でもスロット切替等のアプリショートカットは通す
                    if let Some(shortcut) = match_app_shortcut(&event, self.modifiers)
                        && !matches!(
                            shortcut,
                            AppShortcut::Copy | AppShortcut::Paste | AppShortcut::Find
                        )
                    {
                        self.handle_shortcut(shortcut);
                        return;
                    }
                    self.handle_search_key(&event);
                    return;
                }
                if let Some(shortcut) = match_app_shortcut(&event, self.modifiers) {
                    self.handle_shortcut(shortcut);
                    if self.restart_requested {
                        event_loop.exit();
                    }
                    return;
                }

                if let Some(ws) = self.workspace.as_mut() {
                    if ws.active_slot_ref().is_dormant() && event.state == ElementState::Pressed {
                        let idx = ws.active_slot;
                        ws.ensure_slot_started(idx);
                        self.save_state();
                        self.request_redraw_now();
                        return;
                    }
                    if let Some(pane) = ws.active_pane_mut() {
                        let bytes = encode_key_event(
                            &mut self.key_encoder,
                            &pane.terminal,
                            &event,
                            self.modifiers,
                        );
                        if !bytes.is_empty() {
                            self.selection = None;
                            pane.terminal.scroll_viewport(ScrollViewport::Bottom);
                            pane.mark_dirty();
                            pane.write_input(&bytes);
                        }
                    }
                }
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Preedit(text, _) => {
                    self.ime_preedit = text;
                    self.request_redraw_now();
                }
                Ime::Commit(text) => {
                    self.ime_preedit.clear();
                    if !text.is_empty() {
                        if let Some(edit) = self.url_bar_edit.as_mut() {
                            if edit.select_all {
                                edit.text.clear();
                                edit.select_all = false;
                            }
                            edit.text.push_str(&text);
                        } else if let Some(search) = self.search.as_mut() {
                            if search.select_all {
                                search.query.clear();
                                search.select_all = false;
                            }
                            search.query.push_str(&text);
                            self.update_search_results(true);
                        } else if let Some(ws) = self.workspace.as_mut()
                            && let Some(pane) = ws.active_pane_mut()
                        {
                            pane.terminal.scroll_viewport(ScrollViewport::Bottom);
                            pane.mark_dirty();
                            pane.write_input(text.as_bytes());
                        }
                    }
                    self.request_redraw_now();
                }
                Ime::Disabled => {
                    if !self.ime_preedit.is_empty() {
                        self.ime_preedit.clear();
                        self.request_redraw_now();
                    }
                }
                Ime::Enabled => {}
            },
            WindowEvent::DroppedFile(path) => {
                // スクショ等のファイル D&D: フォーカス中のターミナルペインへエスケープ済みパスを貼り付ける
                let escaped = format!("{} ", macos::shell_escape_path(&path));
                if let Some(ws) = self.workspace.as_mut() {
                    if ws.active_slot_ref().is_dormant() {
                        let idx = ws.active_slot;
                        ws.ensure_slot_started(idx);
                    }
                    if let Some(pane) = ws.active_pane_mut() {
                        pane.terminal.scroll_viewport(ScrollViewport::Bottom);
                        pane.mark_dirty();
                        pane.paste_text(&escaped);
                        self.request_redraw_now();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.drain_daemon_events();
                self.last_redraw = Instant::now();
                self.pending_redraw = false;

                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor() as f32)
                    .unwrap_or(1.0);
                let pane_gap = self.pane_gap();
                let overlays = RenderOverlays {
                    ime_preedit: &self.ime_preedit,
                    drag_source_slot: self
                        .slot_drag
                        .as_ref()
                        .filter(|d| d.is_dragging)
                        .map(|d| d.source_slot),
                    drag_hover_slot: self
                        .slot_drag
                        .as_ref()
                        .filter(|d| d.is_dragging)
                        .and_then(|d| d.hover_slot),
                    selection: self.selection.as_ref().and_then(|s| {
                        let pane = self
                            .workspace
                            .as_ref()?
                            .active_slot_ref()
                            .root
                            .as_ref()?
                            .terminal_pane(s.pane_id)?;
                        let (pa, pb) = pane.resolve_tracked_selection_range(&s.start, &s.end)?;
                        if s.mode == SelectionMode::Char && pa == pb {
                            return None;
                        }
                        let (start, end) = pane.screen_selection_to_viewport(pa, pb)?;
                        Some(SelectionOverlay {
                            pane_id: s.pane_id,
                            start,
                            end,
                        })
                    }),
                    hovered_url: self.hovered_url.as_ref().map(|(pid, m)| UrlHoverOverlay {
                        pane_id: *pid,
                        row: m.row,
                        start_col: m.start_col,
                        end_col: m.end_col,
                    }),
                    url_bar_editing: self
                        .url_bar_edit
                        .as_ref()
                        .map(|e| (e.text.as_str(), e.select_all)),
                    search_highlights: self.search.as_ref().and_then(|s| {
                        let crate::SearchTarget::Terminal(pid) = s.target else {
                            return None;
                        };
                        if s.matches.is_empty() {
                            return None;
                        }
                        let pane = self
                            .workspace
                            .as_ref()?
                            .active_slot_ref()
                            .root
                            .as_ref()?
                            .terminal_pane(pid)?;
                        let vp_top = pane.viewport_top_screen_row();
                        let vp_bottom =
                            vp_top.saturating_add(u32::from(pane.rows.saturating_sub(1)));
                        let visible: Vec<(u16, u16, u16, bool)> = s
                            .matches
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| m.screen_row >= vp_top && m.screen_row <= vp_bottom)
                            .map(|(idx, m)| {
                                (
                                    (m.screen_row - vp_top) as u16,
                                    m.start_col,
                                    m.end_col,
                                    s.current_match == Some(idx),
                                )
                            })
                            .collect();
                        if visible.is_empty() {
                            None
                        } else {
                            Some(SearchHighlightOverlay {
                                pane_id: pid,
                                matches: visible,
                            })
                        }
                    }),
                    search_bar: self.search.as_ref().map(|s| {
                        let count_label = if s.target.is_browser() {
                            if s.query.is_empty() {
                                String::new()
                            } else {
                                "Enter/⇧Enter".to_string()
                            }
                        } else if s.query.is_empty() {
                            String::new()
                        } else if s.matches.is_empty() {
                            "0/0".to_string()
                        } else {
                            let cur = s.current_match.map_or(1, |i| i + 1);
                            format!("{cur}/{}", s.matches.len())
                        };
                        SearchBarOverlay {
                            pane_id: s.target.pane_id(),
                            is_browser: s.target.is_browser(),
                            query: s.query.as_str(),
                            select_all: s.select_all,
                            count_label,
                        }
                    }),
                    battery: self.battery_status.lock().ok().and_then(|g| *g),
                    wifi: self.wifi_status.lock().ok().and_then(|g| *g),
                    show_help: self.show_help,
                    daemon_version_mismatch: self.daemon_version_mismatch,
                    daemon_upgrade_confirm: self
                        .daemon_upgrade_confirm_until
                        .is_some_and(|d| Instant::now() < d),
                    version_copied: self
                        .version_copied_until
                        .is_some_and(|d| Instant::now() < d),
                    timer: self.status_timer.overlay(Instant::now()),
                };

                let mut outcome = None;
                if let (Some(renderer), Some(ws), Some(layout)) = (
                    self.renderer.as_mut(),
                    self.workspace.as_mut(),
                    self.layout.as_ref(),
                ) {
                    outcome = Some(renderer.render(ws, layout, scale, pane_gap, &overlays));
                }
                if let Some(res) = outcome {
                    if let (Some(window), Some((cx, cy, cw, ch))) = (&self.window, res.cursor_area)
                    {
                        let area = (
                            cx.round() as i32,
                            cy.round() as i32,
                            cw.round().max(1.0) as u32,
                            ch.round().max(1.0) as u32,
                        );
                        if self.last_ime_area != Some(area) {
                            self.last_ime_area = Some(area);
                            window.set_ime_cursor_area(
                                PhysicalPosition::new(area.0, area.1),
                                PhysicalSize::new(area.2, area.3),
                            );
                        }
                    }
                    if res.needs_redraw {
                        self.request_redraw_now();
                    }
                }
                self.update_control_flow(event_loop);
            }
            _ => {}
        }
    }
}
