use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::browser::{current_macos_modifiers, install_macos_shortcut_monitor};
use crate::daemon::{DaemonClient, DaemonEvent, default_socket_path};
use crate::layout::AppLayout;
use crate::macos;
use crate::platform::{
    ActiveEventLoop, ApplicationHandler, LogicalPosition, LogicalSize, WindowAttributes,
    WindowEvent,
};
use crate::renderer::Renderer;
use crate::slot::Workspace;
use crate::state::SavedState;
use crate::terminal_pane::AppEvent;
use crate::{
    AUTOSCROLL_INTERVAL, BATTERY_POLL_INTERVAL, CWD_POLL_INTERVAL, DAEMON_EVENT_QUEUE_CAPACITY,
    FRAME_INTERVAL, MEMORY_POLL_INTERVAL, RESTORE_STEP_INTERVAL, SNAPSHOT_POLL_INTERVAL, TentApp,
    selection_autoscroll_delta, spawn_replacement_instance,
};

impl ApplicationHandler for TentApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let saved = self
            .initial_state
            .take()
            .unwrap_or_else(SavedState::default_home);
        self.sidebar_state = saved.sidebar_state;

        let mut attrs = WindowAttributes::default();
        if let Some(geom) = saved.window_geometry {
            attrs.inner_size = LogicalSize::new(f64::from(geom.width), f64::from(geom.height));
            attrs.position = Some(LogicalPosition::new(geom.x, geom.y));
        }
        let window = event_loop.create_window(attrs);

        // Dock / Cmd+Tab に TenT のカスタムアイコンを適用
        macos::install_app_icon();
        // WKWebView フォーカス中でも Cmd+1..0 / Cmd+B / Cmd+L 等を拾えるようにする
        install_macos_shortcut_monitor(self.proxy.clone());

        let scale = window.scale_factor() as f32;
        let gap = (6.0 * scale).round();
        let size = window.inner_size();
        let layout = AppLayout::compute(size.width, size.height, scale, self.sidebar_state);

        let renderer = Renderer::new(window.surface_target(), size.width, size.height, scale)
            .expect("failed to initialize wgpu renderer");

        // デーモンへ接続（未起動なら自動デタッチ起動）し、生きたペインをすべて再接続
        // スリープや描画停止中に GUI 側キューが際限なく膨張するのを防ぐため有界チャネルを使用し、
        // 詰まった際はデーモン側の 16MB ガード＋自動スナップショット再同期へバックプレッシャーを返す
        let (ev_tx, ev_rx) = mpsc::sync_channel::<DaemonEvent>(DAEMON_EVENT_QUEUE_CAPACITY);
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
        self.daemon_pid = attach.daemon_pid;
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
                let mut changed = self.url_bar_edit.is_some();
                self.close_url_bar_edit(false);
                if self.search.is_some() {
                    self.close_search();
                    changed = true;
                }
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
                    || self.search.is_some()
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
            AppEvent::UrlBarSubmit(text) => {
                self.submit_url_bar_edit(&text);
            }
            AppEvent::UrlBarCancel => {
                self.close_url_bar_edit(true);
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
            if ws.update_waiting_states(now, self.window_focused) {
                need_redraw = true;
            }
            let now_outputting = ws.has_any_active_output(now);
            if self.was_any_outputting && !now_outputting {
                need_redraw = true;
            }
            self.was_any_outputting = now_outputting;
        }

        if self.search_resync_due_at().is_some_and(|due| now >= due) {
            need_redraw = true;
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
            if now.duration_since(self.last_memory_poll) >= MEMORY_POLL_INTERVAL {
                self.last_memory_poll = now;
                let self_bytes =
                    macos::get_pid_memory_bytes(std::process::id() as i32).unwrap_or(0);
                let daemon_bytes = self
                    .daemon_pid
                    .and_then(macos::get_pid_memory_bytes)
                    .unwrap_or(0);
                self.memory_overlay = crate::renderer::MemoryOverlay::from_bytes(
                    self_bytes.saturating_add(daemon_bytes),
                );
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

        // ドラッグ選択中のペイン上下端オートスクロール（ペイン外へのドラッグ時のみ）
        if self.can_autoscroll_selection()
            && now.duration_since(self.last_autoscroll) >= AUTOSCROLL_INTERVAL
        {
            self.last_autoscroll = now;
            if let Some(sel) = &self.selection {
                let pid = sel.pane_id;
                let cell_h = self.renderer.as_ref().map(|r| r.cell_h).unwrap_or(20.0);
                let delta = selection_autoscroll_delta(self.cursor_pos.1, sel.pane_rect, cell_h);
                let mut moved = false;
                if delta != 0
                    && let Some(ws) = self.workspace.as_mut()
                    && let Some(root) = ws.active_slot_mut().root.as_mut()
                    && let Some(pane) = root.terminal_pane_mut(pid)
                    && !pane.is_alt_screen()
                {
                    let before_top = pane.viewport_top_screen_row();
                    pane.terminal
                        .scroll_viewport(ghostty_vt::terminal::ScrollViewport::Delta(delta));
                    let after_top = pane.viewport_top_screen_row();
                    if after_top != before_top {
                        pane.mark_dirty();
                        moved = true;
                    }
                }
                if moved {
                    let (px, py) = self.cursor_pos;
                    let _ = self.extend_selection_to_cursor(px, py);
                    need_redraw = true;
                }
            }
        }

        if need_redraw {
            self.request_redraw_now();
        }
        self.update_control_flow(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        self.handle_window_event(event_loop, event);
    }
}
