use ghostty_vt::terminal::ScrollViewport;

use crate::browser::{DEFAULT_BROWSER_URL, normalize_url_input};
use crate::key_map::AppShortcut;
use crate::macos;
use crate::platform::{ElementState, Key, KeyEvent, NamedKey};
use crate::slot::{SplitAxis, Workspace};
use crate::{SearchState, SearchTarget, TentApp, UrlBarEdit};

impl TentApp {
    pub(crate) fn current_url_bar_rect(&self) -> Option<crate::layout::Rect> {
        let layout = self.layout.as_ref()?;
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        let ctrl = layout.status_bar_controls(scale, true, self.daemon_version_mismatch);
        Some(ctrl.url_bar_rect)
    }

    pub(crate) fn start_url_bar_edit(&mut self) {
        self.sync_live_browser_urls();
        let Some((pane_id, url)) = self
            .workspace
            .as_ref()
            .and_then(Workspace::focused_browser)
            .map(|b| (b.id, b.url.clone()))
        else {
            return;
        };
        let Some(url_rect) = self.current_url_bar_rect() else {
            return;
        };
        self.url_bar_edit = Some(UrlBarEdit { pane_id });
        if let Some(window) = &self.window {
            window.show_url_bar(url_rect, &url);
        }
        self.request_redraw_now();
    }

    pub(crate) fn close_url_bar_edit(&mut self, restore_webview_focus: bool) {
        if let Some(edit) = self.url_bar_edit.take() {
            if let Some(window) = &self.window {
                window.hide_url_bar();
            }
            if restore_webview_focus
                && self
                    .workspace
                    .as_ref()
                    .and_then(Workspace::focused_browser)
                    .is_some_and(|b| b.id == edit.pane_id)
            {
                self.browsers.focus_webview(edit.pane_id);
            }
            self.request_redraw_now();
        }
    }

    pub(crate) fn submit_url_bar_edit(&mut self, raw_text: &str) {
        let Some(edit) = self.url_bar_edit.take() else {
            return;
        };
        if let Some(window) = &self.window {
            window.hide_url_bar();
        }
        let url = normalize_url_input(raw_text);
        if let Some(ws) = self.workspace.as_mut() {
            ws.update_browser_url(edit.pane_id, url.clone());
        }
        self.browsers.navigate(edit.pane_id, &url);
        self.browsers.focus_webview(edit.pane_id);
        self.save_state();
        self.request_redraw_now();
    }

    pub(crate) fn start_search(&mut self) {
        self.close_url_bar_edit(false);
        let Some(ws) = self.workspace.as_mut() else {
            return;
        };
        if ws.active_slot_ref().is_dormant() {
            let idx = ws.active_slot;
            ws.ensure_slot_started(idx);
        }
        let target = if let Some(b) = ws.focused_browser() {
            SearchTarget::Browser(b.id)
        } else if let Some(p) = ws.active_pane_mut() {
            SearchTarget::Terminal(p.id)
        } else {
            return;
        };

        // 既に同じペインで検索バーが開いていれば全選択状態にする
        if let Some(search) = self.search.as_mut()
            && search.target == target
        {
            search.select_all = true;
            if target.is_browser() {
                self.focus_terminal_view();
            }
            self.request_redraw_now();
            return;
        }

        // 別のブラウザペインで検索中だった場合はハイライトを解除
        if let Some(prev) = self.search.take()
            && let SearchTarget::Browser(prev_id) = prev.target
        {
            self.browsers.clear_find_highlight(prev_id);
        }

        // ターミナルでテキスト選択中なら、その文字列を初期クエリとして引き継ぐ
        let initial_query = if let SearchTarget::Terminal(pid) = target
            && let Some(sel) = &self.selection
            && sel.pane_id == pid
            && let Some(root) = ws.active_slot_ref().root.as_ref()
            && let Some(pane) = root.terminal_pane(pid)
        {
            let text = pane.extract_tracked_selection_text(&sel.start, &sel.end);
            text.lines().next().unwrap_or("").trim().to_string()
        } else {
            String::new()
        };

        let has_initial = !initial_query.is_empty();
        self.search = Some(SearchState {
            target,
            query: initial_query,
            select_all: has_initial,
            matches: Vec::new(),
            current_match: None,
            current_pin: None,
            content_generation: 0,
            last_sync_at: std::time::Instant::now(),
        });
        if target.is_browser() {
            self.focus_terminal_view();
        }
        if has_initial {
            self.update_search_results(true);
        }
        self.request_redraw_now();
    }

    pub(crate) fn close_search(&mut self) {
        if let Some(search) = self.search.take() {
            if let SearchTarget::Browser(pid) = search.target {
                self.browsers.clear_find_highlight(pid);
                if self
                    .workspace
                    .as_ref()
                    .and_then(Workspace::focused_browser)
                    .is_some_and(|b| b.id == pid)
                {
                    self.browsers.focus_webview(pid);
                }
            }
            self.request_redraw_now();
        }
    }

    /// 出力追加・スクロールバック先頭パージ・リサイズ等で端末内容が変化していれば再検索し、
    /// 追跡ピンを使って現在のマッチ位置を座標ベースで引き継ぐ。
    /// `force == false` のときは `SEARCH_RESYNC_INTERVAL` 未満の連続再検索を間引き、
    /// 出力継続中のメインスレッド描画詰まりを防ぐ。
    /// 対象ペインが現在のアクティブスロットに存在しない場合は検索を閉じる。
    pub(crate) fn sync_search_matches_if_needed(&mut self, force: bool) {
        let Some(target) = self.search.as_ref().map(|s| s.target) else {
            return;
        };
        match target {
            SearchTarget::Terminal(pid) => {
                let pane_exists = self
                    .workspace
                    .as_ref()
                    .and_then(|ws| ws.active_slot_ref().root.as_ref())
                    .and_then(|r| r.terminal_pane(pid))
                    .is_some();
                if !pane_exists {
                    self.close_search();
                    return;
                }
                let Some(search) = self.search.as_mut() else {
                    return;
                };
                if search.query.is_empty() {
                    return;
                }
                let Some(pane) = self
                    .workspace
                    .as_ref()
                    .and_then(|ws| ws.active_slot_ref().root.as_ref())
                    .and_then(|r| r.terminal_pane(pid))
                else {
                    return;
                };
                if pane.content_generation == search.content_generation {
                    return;
                }
                let now = std::time::Instant::now();
                if !force && now.duration_since(search.last_sync_at) < crate::SEARCH_RESYNC_INTERVAL
                {
                    return;
                }
                let prev_pos = search
                    .current_pin
                    .as_ref()
                    .and_then(|pin| pane.resolve_tracked_point(pin));
                let had_current = search.current_match.is_some();
                let found = pane.search_screen(&search.query);
                let next_idx = crate::terminal_pane::resolve_tracked_search_match_index(
                    &found,
                    prev_pos,
                    had_current,
                );
                let next_pin = next_idx
                    .and_then(|i| found.get(i))
                    .and_then(|m| pane.track_screen_point((m.start_col, m.start_row)));
                search.matches = found;
                search.current_match = next_idx;
                search.current_pin = next_pin;
                search.content_generation = pane.content_generation;
                search.last_sync_at = now;
            }
            SearchTarget::Browser(pid) => {
                let pane_exists = self
                    .workspace
                    .as_ref()
                    .and_then(Workspace::focused_browser)
                    .is_some_and(|b| b.id == pid);
                if !pane_exists {
                    self.close_search();
                }
            }
        }
    }

    pub(crate) fn update_search_results(&mut self, jump_to_match: bool) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let query = search.query.clone();
        match search.target {
            SearchTarget::Terminal(pid) => {
                let mut found = Vec::new();
                let mut current_pin = None;
                let mut content_generation = 0;
                if let Some(ws) = self.workspace.as_mut()
                    && let Some(root) = ws.active_slot_mut().root.as_mut()
                    && let Some(pane) = root.terminal_pane_mut(pid)
                {
                    content_generation = pane.content_generation;
                    found = pane.search_screen(&query);
                    if let Some(last) = found.last() {
                        current_pin = pane.track_screen_point((last.start_col, last.start_row));
                        if jump_to_match {
                            pane.scroll_to_screen_row(last.start_row);
                        }
                    }
                }
                search.current_match = if found.is_empty() {
                    None
                } else {
                    Some(found.len() - 1)
                };
                search.current_pin = current_pin;
                search.content_generation = content_generation;
                search.last_sync_at = std::time::Instant::now();
                search.matches = found;
            }
            SearchTarget::Browser(pid) => {
                self.browsers.find_in_page(pid, &query, false, true);
            }
        }
    }

    pub(crate) fn step_search_match(&mut self, backwards: bool) {
        self.sync_search_matches_if_needed(true);
        let Some(search) = self.search.as_mut() else {
            return;
        };
        match search.target {
            SearchTarget::Terminal(pid) => {
                if search.query.is_empty() || search.matches.is_empty() {
                    return;
                }
                let len = search.matches.len();
                let next_idx = match search.current_match {
                    Some(cur) if cur < len => {
                        if backwards {
                            (cur + len - 1) % len
                        } else {
                            (cur + 1) % len
                        }
                    }
                    _ => {
                        if backwards {
                            len - 1
                        } else {
                            0
                        }
                    }
                };
                let target_match = search.matches[next_idx];
                search.current_match = Some(next_idx);
                if let Some(ws) = self.workspace.as_mut()
                    && let Some(root) = ws.active_slot_mut().root.as_mut()
                    && let Some(pane) = root.terminal_pane_mut(pid)
                {
                    search.current_pin =
                        pane.track_screen_point((target_match.start_col, target_match.start_row));
                    pane.scroll_to_screen_row(target_match.start_row);
                }
            }
            SearchTarget::Browser(pid) => {
                let query = search.query.clone();
                if !query.is_empty() {
                    self.browsers.find_in_page(pid, &query, backwards, false);
                }
            }
        }
    }

    pub(crate) fn handle_search_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return true;
        }
        let Some(search) = self.search.as_mut() else {
            return false;
        };

        if self.modifiers.super_key() && !self.modifiers.control_key() {
            match &event.logical_key {
                Key::Character(c) if c.eq_ignore_ascii_case("a") => {
                    search.select_all = true;
                    self.request_redraw_now();
                    return true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("c") => {
                    macos::write_clipboard_text(&search.query);
                    return true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("v") => {
                    if let Some(clip) = macos::read_clipboard_text() {
                        let single_line = clip.replace(['\r', '\n'], "");
                        if search.select_all {
                            search.query.clear();
                            search.select_all = false;
                        }
                        search.query.push_str(&single_line);
                        self.update_search_results(true);
                        self.request_redraw_now();
                    }
                    return true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("g") => {
                    let backwards = self.modifiers.shift_key();
                    self.step_search_match(backwards);
                    self.request_redraw_now();
                    return true;
                }
                _ => {}
            }
        }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close_search();
            }
            Key::Named(NamedKey::Enter) => {
                let backwards = self.modifiers.shift_key();
                self.step_search_match(backwards);
                self.request_redraw_now();
            }
            Key::Named(NamedKey::Backspace) => {
                if search.select_all {
                    search.query.clear();
                    search.select_all = false;
                } else {
                    search.query.pop();
                }
                self.update_search_results(true);
                self.request_redraw_now();
            }
            _ => {
                if !self.modifiers.super_key()
                    && !self.modifiers.control_key()
                    && let Some(text) = &event.text
                {
                    let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !filtered.is_empty() {
                        if search.select_all {
                            search.query.clear();
                            search.select_all = false;
                        }
                        search.query.push_str(&filtered);
                        self.update_search_results(true);
                        self.request_redraw_now();
                    }
                }
            }
        }
        true
    }

    pub(crate) fn handle_shortcut(&mut self, shortcut: AppShortcut) {
        // URL バー編集中でもスロット切替やブラウザ切替を受けたら編集を閉じる
        if !matches!(
            shortcut,
            AppShortcut::Copy | AppShortcut::Paste | AppShortcut::FocusUrlBar
        ) {
            self.close_url_bar_edit(false);
        }
        // 検索バー編集中でも検索操作以外のナビゲーションを受けたら検索を閉じる
        if !matches!(
            shortcut,
            AppShortcut::Copy | AppShortcut::Paste | AppShortcut::Find
        ) {
            self.close_search();
        }

        match shortcut {
            AppShortcut::SelectSlot(slot_idx) => {
                if let Some(ws) = self.workspace.as_mut() {
                    let target = if ws.active_slot == slot_idx {
                        (slot_idx + 5) % crate::slot::NUM_SLOTS
                    } else {
                        slot_idx
                    };
                    ws.select_slot(target);
                }
                self.selection = None;
                self.scroll_accum_px = 0.0;
                self.sync_browsers();
                self.save_state();
                self.request_redraw_now();
            }
            AppShortcut::ToggleCol(col) => {
                self.sidebar_state.toggle_col(col);
                self.recompute_layout_and_resize();
                self.save_state();
            }
            AppShortcut::ToggleAllCols => {
                self.sidebar_state.toggle_all();
                self.recompute_layout_and_resize();
                self.save_state();
            }
            AppShortcut::SplitVertical => {
                self.sync_live_browser_urls();
                if let Some(ws) = self.workspace.as_mut() {
                    ws.split_active_pane(SplitAxis::Vertical);
                }
                self.selection = None;
                self.sync_browsers();
                self.save_state();
                self.request_redraw_now();
            }
            AppShortcut::SplitHorizontal => {
                self.sync_live_browser_urls();
                if let Some(ws) = self.workspace.as_mut() {
                    ws.split_active_pane(SplitAxis::Horizontal);
                }
                self.selection = None;
                self.sync_browsers();
                self.save_state();
                self.request_redraw_now();
            }
            AppShortcut::ClosePane => {
                if let Some(ws) = self.workspace.as_mut() {
                    ws.close_focused_pane();
                }
                self.selection = None;
                self.sync_browsers();
                self.save_state();
                self.request_redraw_now();
            }
            AppShortcut::FocusPane(dir) => {
                if let Some(ws) = self.workspace.as_mut() {
                    ws.focus_pane_dir(dir);
                }
                self.selection = None;
                self.sync_browsers();
                self.request_redraw_now();
            }
            AppShortcut::ResizePane(dir) => {
                let changed = self
                    .workspace
                    .as_mut()
                    .is_some_and(|ws| ws.resize_focused_pane_dir(dir, 0.05));
                if changed {
                    self.selection = None;
                    self.sync_browsers();
                    self.save_state();
                    self.request_redraw_now();
                }
            }
            AppShortcut::ToggleBrowser => {
                if let Some(ws) = self.workspace.as_mut() {
                    ws.toggle_active_slot_browser();
                }
                self.selection = None;
                self.sync_browsers();
                self.save_state();
                self.request_redraw_now();
            }
            AppShortcut::SplitBrowser => {
                self.sync_live_browser_urls();
                if let Some(ws) = self.workspace.as_mut() {
                    let url = ws
                        .focused_browser()
                        .map(|b| b.url.clone())
                        .unwrap_or_else(|| DEFAULT_BROWSER_URL.to_string());
                    ws.split_active_pane_with_browser(SplitAxis::Vertical, &url);
                }
                self.selection = None;
                self.sync_browsers();
                self.save_state();
                self.request_redraw_now();
            }
            AppShortcut::FocusUrlBar => {
                if self
                    .workspace
                    .as_ref()
                    .and_then(Workspace::focused_browser)
                    .is_some()
                {
                    self.start_url_bar_edit();
                }
            }
            AppShortcut::ReloadBrowser => {
                if let Some(b) = self.workspace.as_ref().and_then(Workspace::focused_browser) {
                    let id = b.id;
                    self.browsers.reload(id);
                }
            }
            AppShortcut::BrowserBack => {
                if let Some(b) = self.workspace.as_ref().and_then(Workspace::focused_browser) {
                    let id = b.id;
                    self.browsers.go_back(id);
                }
            }
            AppShortcut::BrowserForward => {
                if let Some(b) = self.workspace.as_ref().and_then(Workspace::focused_browser) {
                    let id = b.id;
                    self.browsers.go_forward(id);
                }
            }
            AppShortcut::RestartApp => {
                self.save_state();
                self.restart_requested = true;
            }
            AppShortcut::ToggleHelp => {
                self.show_help = !self.show_help;
                self.sync_browsers();
                self.request_redraw_now();
            }
            AppShortcut::Copy => {
                if let Some(sel) = &self.selection {
                    if let Some(ws) = self.workspace.as_mut()
                        && let Some(root) = ws.active_slot_mut().root.as_mut()
                        && let Some(pane) = root.terminal_pane_mut(sel.pane_id)
                    {
                        let text = pane.extract_tracked_selection_text(&sel.start, &sel.end);
                        if !text.is_empty() {
                            macos::write_clipboard_text(&text);
                        }
                    }
                    self.selection = None;
                    self.request_redraw_now();
                }
            }
            AppShortcut::Paste => {
                if let Some(ws) = self.workspace.as_mut()
                    && let Some(pane) = ws.active_pane_mut()
                    && let Some(text) = macos::resolve_terminal_paste_payload(
                        macos::read_clipboard_text(),
                        macos::save_clipboard_image_to_temp,
                    )
                {
                    pane.terminal.scroll_viewport(ScrollViewport::Bottom);
                    pane.mark_dirty();
                    pane.paste_text(&text);
                    self.request_redraw_now();
                }
            }
            AppShortcut::ClearScreen => {
                self.selection = None;
                if let Some(ws) = self.workspace.as_mut()
                    && let Some(pane) = ws.active_pane_mut()
                {
                    pane.clear_screen_and_scrollback();
                    self.request_redraw_now();
                }
            }
            AppShortcut::Find => {
                self.start_search();
            }
        }
    }
}
