use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Local};

use crate::browser::BrowserPaneMeta;
use crate::layout::Rect;
use crate::state::{SavedPaneTree, SavedSlot};
use crate::terminal_pane::{PaneId, TerminalPane};

use super::pane_tree::{Pane, PaneNode, SlotViewMode};

pub struct Slot {
    pub root: Option<PaneNode>,
    pub focused_pane: Option<PaneId>,
    pub last_cwd: PathBuf,
    pub saved_tree: Option<SavedPaneTree>,
    pub view_mode: SlotViewMode,
    pub browser_root: Option<PaneNode>,
    pub browser_focused_pane: Option<PaneId>,
}

impl Slot {
    pub fn number_label(index: usize) -> &'static str {
        match index {
            0 => "1",
            1 => "2",
            2 => "3",
            3 => "4",
            4 => "5",
            5 => "6",
            6 => "7",
            7 => "8",
            8 => "9",
            9 => "0",
            _ => "?",
        }
    }

    pub fn is_dormant(&self) -> bool {
        self.root.is_none() && self.view_mode == SlotViewMode::Panes
    }

    pub fn is_browser_focused(&self) -> bool {
        self.focused_browser().is_some()
    }

    pub fn active_focused_pane(&self) -> Option<PaneId> {
        match self.view_mode {
            SlotViewMode::Browser => self
                .browser_focused_pane
                .or_else(|| self.browser_root.as_ref().map(PaneNode::first_pane_id)),
            SlotViewMode::Panes => self
                .focused_pane
                .or_else(|| self.root.as_ref().map(PaneNode::first_pane_id)),
        }
    }

    pub fn is_playing_audio(&self) -> bool {
        let mut playing = false;
        if let Some(broot) = &self.browser_root {
            broot.for_each_browser(&mut |b| {
                if b.is_playing_audio {
                    playing = true;
                }
            });
        }
        if !playing && let Some(root) = &self.root {
            root.for_each_browser(&mut |b| {
                if b.is_playing_audio {
                    playing = true;
                }
            });
        }
        playing
    }

    pub fn focused_pane_item(&self) -> Option<&Pane> {
        let root = self.root.as_ref()?;
        let id = self.focused_pane.unwrap_or_else(|| root.first_pane_id());
        root.pane(id).or_else(|| root.pane(root.first_pane_id()))
    }

    pub fn focused_pane_ref(&self) -> Option<&TerminalPane> {
        if self.view_mode == SlotViewMode::Browser {
            return None;
        }
        self.focused_pane_item().and_then(Pane::as_terminal)
    }

    pub fn focused_pane_mut(&mut self) -> Option<&mut TerminalPane> {
        if self.view_mode == SlotViewMode::Browser {
            return None;
        }
        let root = self.root.as_mut()?;
        let id = self.focused_pane.unwrap_or_else(|| root.first_pane_id());
        if root.pane(id).is_some() {
            root.terminal_pane_mut(id)
        } else {
            let first = root.first_pane_id();
            self.focused_pane = Some(first);
            root.terminal_pane_mut(first)
        }
    }

    pub fn focused_browser(&self) -> Option<&BrowserPaneMeta> {
        if self.view_mode == SlotViewMode::Browser {
            let broot = self.browser_root.as_ref()?;
            let id = self
                .browser_focused_pane
                .unwrap_or_else(|| broot.first_pane_id());
            return broot
                .pane(id)
                .or_else(|| broot.pane(broot.first_pane_id()))
                .and_then(Pane::as_browser);
        }
        self.focused_pane_item().and_then(Pane::as_browser)
    }

    /// スロットの代表ペイン（現在の表示モードにおける先頭ペイン。フォーカス移動では変わらない）
    pub fn primary_pane(&self) -> Option<&Pane> {
        if self.view_mode == SlotViewMode::Browser {
            self.browser_root.as_ref().map(PaneNode::first_pane)
        } else {
            self.root.as_ref().map(PaneNode::first_pane)
        }
    }

    /// スロット先頭ペインのカレントディレクトリ（ステータスバー表示用。フォーカス移動で変わらない）
    pub fn primary_terminal_cwd(&self) -> &std::path::Path {
        self.root
            .as_ref()
            .and_then(|r| r.first_pane().as_terminal())
            .map(|p| p.cwd.as_path())
            .unwrap_or(self.last_cwd.as_path())
    }

    pub fn folder_name(&self) -> String {
        match self.primary_pane() {
            Some(Pane::Browser(b)) => b.display_label(),
            Some(Pane::Terminal(p)) => p.folder_name.clone(),
            None => crate::terminal_pane::extract_folder_name(&self.last_cwd),
        }
    }

    pub fn is_actively_outputting(&self, now: Instant) -> bool {
        let mut active = false;
        if let Some(root) = &self.root {
            root.for_each_terminal(&mut |p| {
                if p.is_actively_outputting(now) {
                    active = true;
                }
            });
        }
        active
    }

    pub fn has_notification(&self) -> bool {
        let mut notif = false;
        if let Some(root) = &self.root {
            root.for_each_terminal(&mut |p| {
                if p.has_notification {
                    notif = true;
                }
            });
        }
        notif
    }

    pub fn is_waiting_input(&self) -> bool {
        let mut waiting = false;
        if let Some(root) = &self.root {
            root.for_each_terminal(&mut |p| {
                if p.is_waiting_input {
                    waiting = true;
                }
            });
        }
        waiting
    }

    pub fn clear_attention(&mut self) {
        if let Some(root) = &mut self.root {
            root.for_each_terminal_mut(&mut |p| {
                p.has_notification = false;
                p.is_waiting_input = false;
                p.had_background_output = false;
                p.background_output_started_at = None;
                p.active_job_started_at = None;
                p.mark_screen_seen();
            });
        }
    }

    pub fn mark_leaving_active(&mut self, now: Instant) {
        if let Some(root) = &mut self.root {
            root.for_each_terminal_mut(&mut |p| {
                p.mark_screen_seen();
                if p.is_actively_outputting(now) {
                    p.had_background_output = true;
                    p.background_output_started_at.get_or_insert(now);
                }
            });
        }
    }

    pub fn latest_output_time_str(&self) -> String {
        let mut latest: Option<DateTime<Local>> = None;
        if let Some(root) = &self.root {
            root.for_each_terminal(&mut |p| {
                latest = Some(match latest {
                    Some(cur) if cur >= p.last_output_at => cur,
                    _ => p.last_output_at,
                });
            });
        }
        latest
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default()
    }

    pub fn compute_pane_rects(&self, area: Rect, gap: f32) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        match self.view_mode {
            SlotViewMode::Panes => {
                if let Some(root) = &self.root {
                    root.compute_rects(area, gap, &mut out);
                }
            }
            SlotViewMode::Browser => {
                if let Some(broot) = &self.browser_root {
                    broot.compute_rects(area, gap, &mut out);
                }
            }
        }
        out
    }

    pub fn resize_to_area(&mut self, area: Rect, gap: f32, cell_w: f32, cell_h: f32) {
        let mut rects = Vec::new();
        if let Some(root) = &self.root {
            root.compute_rects(area, gap, &mut rects);
        }
        let cw_px = cell_w.round().max(1.0) as u32;
        let ch_px = cell_h.round().max(1.0) as u32;
        if let Some(root) = &mut self.root {
            for (pid, rect) in rects {
                let cols = (rect.w / cell_w).floor().max(10.0) as u16;
                let rows = (rect.h / cell_h).floor().max(4.0) as u16;
                if let Some(pane) = root.terminal_pane_mut(pid) {
                    pane.resize(cols, rows, cw_px, ch_px);
                }
            }
        }
    }

    pub fn to_saved_slot(&self) -> SavedSlot {
        let last_cwd = self
            .focused_pane_ref()
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| self.last_cwd.clone());
        let tree = self
            .root
            .as_ref()
            .map(PaneNode::to_saved_tree)
            .or_else(|| self.saved_tree.clone());
        let browser_tree = self.browser_root.as_ref().map(PaneNode::to_saved_tree);
        let browser_url = self.browser_root.as_ref().and_then(|br| {
            let id = self
                .browser_focused_pane
                .unwrap_or_else(|| br.first_pane_id());
            br.pane(id)
                .or_else(|| br.pane(br.first_pane_id()))
                .and_then(Pane::as_browser)
                .map(|b| b.url.clone())
        });
        SavedSlot {
            last_cwd,
            focused_pane: self.focused_pane,
            tree,
            is_browser_mode: self.view_mode == SlotViewMode::Browser,
            browser_url,
            browser_tree,
        }
    }
}
