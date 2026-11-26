use ghostty_vt::terminal::ScrollViewport;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};

use crate::browser::{DEFAULT_BROWSER_URL, normalize_url_input};
use crate::key_map::AppShortcut;
use crate::macos;
use crate::slot::{SplitAxis, Workspace};
use crate::{TentApp, UrlBarEdit};

impl TentApp {
    pub(crate) fn start_url_bar_edit(&mut self) {
        self.sync_live_browser_urls();
        let Some(ws) = &self.workspace else {
            return;
        };
        let Some(b) = ws.focused_browser() else {
            return;
        };
        self.url_bar_edit = Some(UrlBarEdit {
            pane_id: b.id,
            text: b.url.clone(),
            select_all: true,
        });
        self.browsers.focus_parent();
        self.request_redraw_now();
    }

    pub(crate) fn handle_url_bar_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return true;
        }
        let Some(edit) = self.url_bar_edit.as_mut() else {
            return false;
        };

        if self.modifiers.super_key() && !self.modifiers.control_key() {
            match &event.logical_key {
                Key::Character(c) if c.eq_ignore_ascii_case("a") => {
                    edit.select_all = true;
                    self.request_redraw_now();
                    return true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("c") => {
                    macos::write_clipboard_text(&edit.text);
                    return true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("v") => {
                    if let Some(clip) = macos::read_clipboard_text() {
                        let single_line = clip.replace(['\r', '\n'], "");
                        if edit.select_all {
                            edit.text.clear();
                            edit.select_all = false;
                        }
                        edit.text.push_str(&single_line);
                        self.request_redraw_now();
                    }
                    return true;
                }
                _ => {}
            }
        }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                let pid = edit.pane_id;
                self.url_bar_edit = None;
                self.browsers.focus_webview(pid);
                self.request_redraw_now();
            }
            Key::Named(NamedKey::Enter) => {
                let edit = self.url_bar_edit.take().unwrap();
                let url = normalize_url_input(&edit.text);
                if let Some(ws) = self.workspace.as_mut() {
                    ws.update_browser_url(edit.pane_id, url.clone());
                }
                self.browsers.navigate(edit.pane_id, &url);
                self.browsers.focus_webview(edit.pane_id);
                self.save_state();
                self.request_redraw_now();
            }
            Key::Named(NamedKey::Backspace) => {
                if edit.select_all {
                    edit.text.clear();
                    edit.select_all = false;
                } else {
                    edit.text.pop();
                }
                self.request_redraw_now();
            }
            _ => {
                if !self.modifiers.super_key()
                    && !self.modifiers.control_key()
                    && let Some(text) = &event.text
                {
                    let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !filtered.is_empty() {
                        if edit.select_all {
                            edit.text.clear();
                            edit.select_all = false;
                        }
                        edit.text.push_str(&filtered);
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
            self.url_bar_edit = None;
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
                        let text = pane.extract_selection_text(sel.start, sel.end);
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
        }
    }
}
