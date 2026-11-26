use std::collections::HashMap;

use crate::browser::BrowserPaneMeta;
use crate::daemon::{DaemonClient, ReplayedPane};
use crate::layout::Rect;
use crate::state::{SavedPaneTree, SavedSlot};
use crate::terminal_pane::{PaneId, TerminalPane};

use super::pane_tree::{Pane, PaneNode, SlotViewMode};
use super::slot_item::Slot;
use super::{NUM_SLOTS, Workspace};

impl Workspace {
    #[allow(clippy::too_many_arguments)]
    pub fn from_session(
        main_area: Rect,
        pane_gap: f32,
        cell_w: f32,
        cell_h: f32,
        saved_slots: [SavedSlot; NUM_SLOTS],
        saved_active_slot: usize,
        replayed_panes: Vec<ReplayedPane>,
        daemon_next_pane_id: PaneId,
        daemon: DaemonClient,
    ) -> Self {
        let is_reconnect = !replayed_panes.is_empty();
        let mut live_map: HashMap<PaneId, ReplayedPane> = replayed_panes
            .into_iter()
            .map(|rp| (rp.pane_id, rp))
            .collect();

        let mut max_id = daemon_next_pane_id.max(1);
        // 復元時の新規採番が後続スロットの保存済みペインID（特にブラウザID）と衝突しないよう、
        // 全スロットの保存済みIDを事前に予約しておく
        for saved in &saved_slots {
            if let Some(fid) = saved.focused_pane {
                max_id = max_id.max(fid + 1);
            }
            if let Some(t) = &saved.tree {
                max_id = max_id.max(max_saved_tree_id(t) + 1);
            }
            if let Some(bt) = &saved.browser_tree {
                max_id = max_id.max(max_saved_tree_id(bt) + 1);
            }
        }
        let mut slots = Vec::with_capacity(NUM_SLOTS);

        for saved in saved_slots {
            let reattached_root = saved
                .tree
                .as_ref()
                .and_then(|t| reattach_tree(t, &mut live_map, is_reconnect, &daemon, &mut max_id));

            let browser_root = if let Some(btree) = &saved.browser_tree {
                restore_browser_tree(btree, &mut max_id)
            } else {
                saved.browser_url.map(|url| {
                    let id = max_id;
                    max_id += 1;
                    PaneNode::Leaf(Pane::Browser(Box::new(BrowserPaneMeta::new(id, url))))
                })
            };
            let browser_focused_pane = browser_root.as_ref().map(PaneNode::first_pane_id);
            let view_mode = if saved.is_browser_mode && browser_root.is_some() {
                SlotViewMode::Browser
            } else {
                SlotViewMode::Panes
            };

            if let Some(root) = reattached_root {
                let focused = saved
                    .focused_pane
                    .filter(|id| root.pane(*id).is_some())
                    .unwrap_or_else(|| root.first_pane_id());
                let cwd = root
                    .terminal_pane(focused)
                    .map(|p| p.cwd.clone())
                    .unwrap_or(saved.last_cwd);
                slots.push(Slot {
                    root: Some(root),
                    focused_pane: Some(focused),
                    last_cwd: cwd,
                    saved_tree: None,
                    view_mode,
                    browser_root,
                    browser_focused_pane,
                });
            } else {
                slots.push(Slot {
                    root: None,
                    focused_pane: None,
                    last_cwd: saved.last_cwd,
                    saved_tree: saved.tree,
                    view_mode,
                    browser_root,
                    browser_focused_pane,
                });
            }
        }

        // どのスロットにも紐付かなかった生きたペインがあれば空きスロットへ収容
        let mut orphans: Vec<ReplayedPane> = live_map.into_values().collect();
        orphans.sort_by_key(|p| p.pane_id);
        for rp in orphans {
            max_id = max_id.max(rp.pane_id + 1);
            if let Some(empty_slot) = slots.iter_mut().find(|s| s.is_dormant())
                && let Ok(pane) = TerminalPane::attach_from_replay(rp, daemon.clone())
            {
                let id = pane.id;
                empty_slot.last_cwd = pane.cwd.clone();
                empty_slot.focused_pane = Some(id);
                empty_slot.root = Some(PaneNode::Leaf(Pane::Terminal(Box::new(pane))));
                empty_slot.saved_tree = None;
            }
        }

        let active_slot = saved_active_slot.min(NUM_SLOTS - 1);
        let mut ws = Self {
            slots,
            active_slot,
            next_pane_id: max_id,
            main_area,
            pane_gap,
            cell_w,
            cell_h,
            daemon,
        };

        ws.resize_all(main_area, pane_gap, cell_w, cell_h);

        // アクティブスロットが休眠中（かつブラウザモードでない）なら起動
        ws.ensure_slot_started(active_slot);
        ws
    }

    pub fn ensure_slot_started(&mut self, index: usize) {
        if index >= self.slots.len()
            || self.slots[index].root.is_some()
            || self.slots[index].view_mode == SlotViewMode::Browser
        {
            return;
        }
        let cols = (self.main_area.w / self.cell_w).floor().max(10.0) as u16;
        let rows = (self.main_area.h / self.cell_h).floor().max(4.0) as u16;

        let saved_tree = self.slots[index].saved_tree.take();
        let fallback_cwd = self.slots[index].last_cwd.clone();

        let spawned_root = if let Some(tree) = saved_tree {
            spawn_from_saved_tree(&tree, &mut self.next_pane_id, cols, rows, &self.daemon)
        } else {
            let id = self.next_pane_id;
            self.next_pane_id += 1;
            TerminalPane::spawn_via_daemon(id, cols, rows, fallback_cwd, self.daemon.clone())
                .ok()
                .map(|p| PaneNode::Leaf(Pane::Terminal(Box::new(p))))
        };

        if let Some(root) = spawned_root {
            let first_id = root.first_pane_id();
            self.slots[index].root = Some(root);
            self.slots[index].focused_pane = Some(first_id);
            let (area, gap, cw, ch) = (self.main_area, self.pane_gap, self.cell_w, self.cell_h);
            self.slots[index].resize_to_area(area, gap, cw, ch);
        }
    }

    pub fn export_saved_slots(&self) -> [SavedSlot; NUM_SLOTS] {
        std::array::from_fn(|i| self.slots[i].to_saved_slot())
    }
}

/// 保存済み分割木に含まれる最大の PaneId を返す（復元前の ID 予約用）
fn max_saved_tree_id(tree: &SavedPaneTree) -> PaneId {
    match tree {
        SavedPaneTree::Leaf { pane_id, .. } | SavedPaneTree::Browser { pane_id, .. } => *pane_id,
        SavedPaneTree::Split { first, second, .. } => {
            max_saved_tree_id(first).max(max_saved_tree_id(second))
        }
    }
}

/// 保存されていたブラウザ分割木からブラウザペイン群を復元する
fn restore_browser_tree(saved: &SavedPaneTree, next_id: &mut PaneId) -> Option<PaneNode> {
    match saved {
        SavedPaneTree::Browser { url, .. } => {
            let id = *next_id;
            *next_id += 1;
            Some(PaneNode::Leaf(Pane::Browser(Box::new(
                BrowserPaneMeta::new(id, url.clone()),
            ))))
        }
        SavedPaneTree::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let f = restore_browser_tree(first, next_id);
            let s = restore_browser_tree(second, next_id);
            match (f, s) {
                (Some(a), Some(b)) => Some(PaneNode::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        SavedPaneTree::Leaf { .. } => None,
    }
}

/// 保存されていた分割木のうち、デーモン上で生きているペインとブラウザペインを再構築する
fn reattach_tree(
    saved: &SavedPaneTree,
    live_map: &mut HashMap<PaneId, ReplayedPane>,
    is_reconnect: bool,
    daemon: &DaemonClient,
    max_id: &mut PaneId,
) -> Option<PaneNode> {
    match saved {
        SavedPaneTree::Leaf { pane_id, .. } => {
            let rp = live_map.remove(pane_id)?;
            *max_id = (*max_id).max(rp.pane_id + 1);
            let pane = TerminalPane::attach_from_replay(rp, daemon.clone()).ok()?;
            Some(PaneNode::Leaf(Pane::Terminal(Box::new(pane))))
        }
        SavedPaneTree::Browser { pane_id, url } => {
            // 再接続セッション中は保存されていたブラウザペインもそのまま復元する
            // （完全コールドスタート時は saved_tree として休眠保持し、スロットを開いたときに復元）
            if !is_reconnect {
                return None;
            }
            *max_id = (*max_id).max(*pane_id + 1);
            Some(PaneNode::Leaf(Pane::Browser(Box::new(
                BrowserPaneMeta::new(*pane_id, url.clone()),
            ))))
        }
        SavedPaneTree::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let f = reattach_tree(first, live_map, is_reconnect, daemon, max_id);
            let s = reattach_tree(second, live_map, is_reconnect, daemon, max_id);
            match (f, s) {
                (Some(a), Some(b)) => Some(PaneNode::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
    }
}

/// 保存されていた分割木から新規にペイン群を起動する
fn spawn_from_saved_tree(
    saved: &SavedPaneTree,
    next_id: &mut PaneId,
    cols: u16,
    rows: u16,
    daemon: &DaemonClient,
) -> Option<PaneNode> {
    match saved {
        SavedPaneTree::Leaf { cwd, .. } => {
            let id = *next_id;
            *next_id += 1;
            let pane =
                TerminalPane::spawn_via_daemon(id, cols, rows, cwd.clone(), daemon.clone()).ok()?;
            Some(PaneNode::Leaf(Pane::Terminal(Box::new(pane))))
        }
        SavedPaneTree::Browser { url, .. } => {
            let id = *next_id;
            *next_id += 1;
            Some(PaneNode::Leaf(Pane::Browser(Box::new(
                BrowserPaneMeta::new(id, url.clone()),
            ))))
        }
        SavedPaneTree::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let f = spawn_from_saved_tree(first, next_id, cols, rows, daemon);
            let s = spawn_from_saved_tree(second, next_id, cols, rows, daemon);
            match (f, s) {
                (Some(a), Some(b)) => Some(PaneNode::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
    }
}
