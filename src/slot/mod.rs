mod pane_tree;
mod session;
mod slot_item;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Instant;

use crate::browser::{BrowserPaneMeta, DEFAULT_BROWSER_URL};
use crate::daemon::{DaemonClient, ReplayedPane};
use crate::key_map::FocusDir;
use crate::layout::Rect;
use crate::terminal_pane::{PaneId, TerminalPane};

pub use pane_tree::{Pane, PaneNode, SlotViewMode, SplitAxis, SplitDivider, ratio_from_pointer};
pub use slot_item::Slot;

use pane_tree::{remove_node, select_next_pane, split_node_mut};

pub const NUM_SLOTS: usize = 10;

pub struct Workspace {
    pub slots: Vec<Slot>,
    pub active_slot: usize,
    next_pane_id: PaneId,
    main_area: Rect,
    pane_gap: f32,
    cell_w: f32,
    cell_h: f32,
    daemon: DaemonClient,
}

impl Workspace {
    #[allow(clippy::too_many_arguments)]
    pub fn active_slot_ref(&self) -> &Slot {
        &self.slots[self.active_slot]
    }

    pub fn active_slot_mut(&mut self) -> &mut Slot {
        &mut self.slots[self.active_slot]
    }

    pub fn active_pane_mut(&mut self) -> Option<&mut TerminalPane> {
        self.slots[self.active_slot].focused_pane_mut()
    }

    pub fn focused_browser(&self) -> Option<&BrowserPaneMeta> {
        self.slots[self.active_slot].focused_browser()
    }

    /// 指定スロットが休眠中なら起動する（保存されていた分割構成があればそのまま復元）
    pub fn select_slot(&mut self, index: usize) {
        if index < self.slots.len() {
            if index != self.active_slot {
                let now = Instant::now();
                self.slots[self.active_slot].mark_leaving_active(now);
            }
            self.active_slot = index;
            self.ensure_slot_started(index);
            self.slots[index].clear_attention();
        }
    }

    /// アクティブスロットを ペイン表示 ⇔ スロット専用ブラウザ表示 で切り替える（Cmd+B）
    /// ブラウザへ切り替わった場合はその PaneId を返す
    pub fn toggle_active_slot_browser(&mut self) -> Option<PaneId> {
        let idx = self.active_slot;
        match self.slots[idx].view_mode {
            SlotViewMode::Panes => {
                if self.slots[idx].browser_root.is_none() {
                    let id = self.next_pane_id;
                    self.next_pane_id += 1;
                    self.slots[idx].browser_root = Some(PaneNode::Leaf(Pane::Browser(Box::new(
                        BrowserPaneMeta::new(id, DEFAULT_BROWSER_URL),
                    ))));
                    self.slots[idx].browser_focused_pane = Some(id);
                }
                self.slots[idx].view_mode = SlotViewMode::Browser;
                self.slots[idx].active_focused_pane()
            }
            SlotViewMode::Browser => {
                self.slots[idx].view_mode = SlotViewMode::Panes;
                self.ensure_slot_started(idx);
                None
            }
        }
    }

    /// 指定 URL をアクティブスロットのブラウザで開く（Cmd+Shift+Click 等）
    pub fn open_url_in_active_browser(&mut self, url: &str) -> PaneId {
        let idx = self.active_slot;
        let normalized = crate::browser::normalize_url_input(url);
        let saved_focused = self.slots[idx].browser_focused_pane;
        if let Some(broot) = self.slots[idx].browser_root.as_mut() {
            let target = saved_focused.unwrap_or_else(|| broot.first_pane_id());
            if let Some(b) = broot.pane_mut(target).and_then(Pane::as_browser_mut) {
                b.url = normalized;
                let id = b.id;
                self.slots[idx].browser_focused_pane = Some(id);
                self.slots[idx].view_mode = SlotViewMode::Browser;
                return id;
            }
        }
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        self.slots[idx].browser_root = Some(PaneNode::Leaf(Pane::Browser(Box::new(
            BrowserPaneMeta::new(id, normalized),
        ))));
        self.slots[idx].browser_focused_pane = Some(id);
        self.slots[idx].view_mode = SlotViewMode::Browser;
        id
    }

    /// アクティブペインを分割してブラウザペインを追加する（Cmd+Shift+B）
    pub fn split_active_pane_with_browser(&mut self, axis: SplitAxis, url: &str) -> Option<PaneId> {
        let idx = self.active_slot;
        if self.slots[idx].view_mode == SlotViewMode::Browser {
            return self.split_browser_mode_pane(idx, axis, url);
        }
        self.ensure_slot_started(idx);
        let target_id = self.slots[idx].focused_pane?;

        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let new_browser = Pane::Browser(Box::new(BrowserPaneMeta::new(new_id, url)));

        let mut opt_new = Some(new_browser);
        if let Some(root) = self.slots[idx].root.take() {
            let (updated, ok) = split_node_mut(root, target_id, axis, &mut opt_new);
            self.slots[idx].root = Some(updated);
            if ok {
                self.slots[idx].focused_pane = Some(new_id);
                let (area, gap, cw, ch) = (self.main_area, self.pane_gap, self.cell_w, self.cell_h);
                self.slots[idx].resize_to_area(area, gap, cw, ch);
                return Some(new_id);
            }
        }
        None
    }

    fn split_browser_mode_pane(
        &mut self,
        idx: usize,
        axis: SplitAxis,
        url: &str,
    ) -> Option<PaneId> {
        let broot = self.slots[idx].browser_root.take()?;
        let target_id = self.slots[idx]
            .browser_focused_pane
            .unwrap_or_else(|| broot.first_pane_id());
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;
        let new_browser = Pane::Browser(Box::new(BrowserPaneMeta::new(new_id, url)));
        let mut opt_new = Some(new_browser);
        let (updated, ok) = split_node_mut(broot, target_id, axis, &mut opt_new);
        self.slots[idx].browser_root = Some(updated);
        if ok {
            self.slots[idx].browser_focused_pane = Some(new_id);
            Some(new_id)
        } else {
            None
        }
    }

    /// 現在アクティブスロット上で画面に表示すべきブラウザ一覧（PaneId, 物理矩形, URL）を返す
    pub fn visible_browsers(&self) -> Vec<(PaneId, Rect, String)> {
        self.browsers_in_slot(self.active_slot)
    }

    /// 指定スロットに属する現在アクティブなブラウザ一覧（PaneId, 物理矩形, URL）を返す
    pub fn browsers_in_slot(&self, slot_idx: usize) -> Vec<(PaneId, Rect, String)> {
        let Some(slot) = self.slots.get(slot_idx) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let active_root = match slot.view_mode {
            SlotViewMode::Browser => slot.browser_root.as_ref(),
            SlotViewMode::Panes => slot.root.as_ref(),
        };
        if let Some(root) = active_root {
            let rects = slot.compute_pane_rects(self.main_area, self.pane_gap);
            for (pid, rect) in rects {
                if let Some(b) = root.pane(pid).and_then(Pane::as_browser) {
                    out.push((pid, rect, b.url.clone()));
                }
            }
        }
        out
    }

    /// 起動時の段階的復旧（1スロットずつ）対象となる残スロット一覧を返す
    pub fn slots_needing_restore(&self) -> Vec<usize> {
        let mut list = Vec::new();
        for (i, slot) in self.slots.iter().enumerate() {
            if i == self.active_slot {
                continue;
            }
            let needs_terminal = slot.saved_tree.is_some() && slot.root.is_none();
            let needs_browser = !self.browsers_in_slot(i).is_empty();
            if needs_terminal || needs_browser {
                list.push(i);
            }
        }
        list
    }

    /// 全スロットに存在する全ブラウザペインの ID 一覧（閉じられた WebView の破棄判定用）
    pub fn all_browser_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        for slot in &self.slots {
            if let Some(broot) = &slot.browser_root {
                broot.for_each_browser(&mut |b| ids.push(b.id));
            }
            if let Some(root) = &slot.root {
                root.for_each_browser(&mut |b| ids.push(b.id));
            }
        }
        ids
    }

    pub fn update_browser_url(&mut self, pane_id: PaneId, url: String) -> bool {
        for slot in &mut self.slots {
            for tree in [slot.browser_root.as_mut(), slot.root.as_mut()]
                .into_iter()
                .flatten()
            {
                if let Some(b) = tree.pane_mut(pane_id).and_then(Pane::as_browser_mut) {
                    if b.url != url {
                        b.url = url;
                        return true;
                    }
                    return false;
                }
            }
        }
        false
    }

    pub fn update_browser_title(&mut self, pane_id: PaneId, title: String) -> bool {
        for slot in &mut self.slots {
            for tree in [slot.browser_root.as_mut(), slot.root.as_mut()]
                .into_iter()
                .flatten()
            {
                if let Some(b) = tree.pane_mut(pane_id).and_then(Pane::as_browser_mut) {
                    if b.title != title {
                        b.title = title;
                        return true;
                    }
                    return false;
                }
            }
        }
        false
    }

    pub fn update_browser_audio(&mut self, pane_id: PaneId, is_playing: bool) -> bool {
        for slot in &mut self.slots {
            for tree in [slot.browser_root.as_mut(), slot.root.as_mut()]
                .into_iter()
                .flatten()
            {
                if let Some(b) = tree.pane_mut(pane_id).and_then(Pane::as_browser_mut) {
                    if b.is_playing_audio != is_playing {
                        b.is_playing_audio = is_playing;
                        return true;
                    }
                    return false;
                }
            }
        }
        false
    }

    pub fn update_waiting_states(&mut self, now: Instant) -> bool {
        let active = self.active_slot;
        let mut changed = false;
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            let is_active = idx == active;
            let slot_label = Slot::number_label(idx);
            if let Some(root) = &mut slot.root {
                root.for_each_terminal_mut(&mut |p| {
                    if p.update_waiting_state(now, is_active, Some(slot_label)) {
                        changed = true;
                    }
                });
            }
        }
        changed
    }

    pub fn swap_slots(&mut self, a: usize, b: usize) {
        if a >= self.slots.len() || b >= self.slots.len() || a == b {
            return;
        }
        self.slots.swap(a, b);
        if self.active_slot == a {
            self.active_slot = b;
        } else if self.active_slot == b {
            self.active_slot = a;
        }
    }

    pub fn split_active_pane(&mut self, axis: SplitAxis) {
        let idx = self.active_slot;
        // ブラウザモード中なら、分割元の現在の URL を引き継いでブラウザを分割する
        if self.slots[idx].view_mode == SlotViewMode::Browser {
            let cur_url = self.slots[idx]
                .focused_browser()
                .map(|b| b.url.clone())
                .unwrap_or_else(|| DEFAULT_BROWSER_URL.to_string());
            self.split_browser_mode_pane(idx, axis, &cur_url);
            return;
        }
        // 分割内のブラウザペインにフォーカス中なら、そのペインの URL を引き継いで分割
        if let Some(cur_url) = self.slots[idx].focused_browser().map(|b| b.url.clone()) {
            self.split_active_pane_with_browser(axis, &cur_url);
            return;
        }
        self.ensure_slot_started(idx);
        let Some(target_id) = self.slots[idx].focused_pane else {
            return;
        };
        let cwd = self.slots[idx]
            .focused_pane_ref()
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| self.slots[idx].last_cwd.clone());

        let new_id = self.next_pane_id;
        self.next_pane_id += 1;

        let cols = (self.main_area.w / self.cell_w).floor().max(10.0) as u16;
        let rows = (self.main_area.h / self.cell_h).floor().max(4.0) as u16;

        match TerminalPane::spawn_via_daemon(new_id, cols, rows, cwd, self.daemon.clone()) {
            Ok(new_pane) => {
                let mut opt_new = Some(Pane::Terminal(Box::new(new_pane)));
                if let Some(root) = self.slots[idx].root.take() {
                    let (updated, ok) = split_node_mut(root, target_id, axis, &mut opt_new);
                    self.slots[idx].root = Some(updated);
                    if ok {
                        self.slots[idx].focused_pane = Some(new_id);
                        let (area, gap, cw, ch) =
                            (self.main_area, self.pane_gap, self.cell_w, self.cell_h);
                        self.slots[idx].resize_to_area(area, gap, cw, ch);
                    }
                }
            }
            Err(err) => {
                eprintln!("TenT: failed to split pane: {err}");
            }
        }
    }

    /// アクティブスロットのフォーカス中ペイン（またはスロットブラウザ）を閉じる（Cmd+W）
    pub fn close_focused_pane(&mut self) {
        let idx = self.active_slot;
        if self.slots[idx].view_mode == SlotViewMode::Browser {
            if let Some(broot) = self.slots[idx].browser_root.take() {
                let target_id = self.slots[idx]
                    .browser_focused_pane
                    .unwrap_or_else(|| broot.first_pane_id());
                let (next_root, _) = remove_node(broot, target_id);
                self.slots[idx].browser_root = next_root;
                if let Some(r) = &self.slots[idx].browser_root {
                    self.slots[idx].browser_focused_pane = Some(r.first_pane_id());
                } else {
                    self.slots[idx].browser_focused_pane = None;
                    self.slots[idx].view_mode = SlotViewMode::Panes;
                }
            } else {
                self.slots[idx].view_mode = SlotViewMode::Panes;
            }
            return;
        }
        let Some(target_id) = self.slots[idx].focused_pane else {
            return;
        };
        let is_terminal = self.slots[idx]
            .root
            .as_ref()
            .and_then(|r| r.terminal_pane(target_id))
            .is_some();
        if is_terminal {
            self.daemon.kill_pane(target_id);
        }
        self.handle_pane_exited(target_id);
    }

    pub fn handle_pane_exited(&mut self, pane_id: PaneId) {
        let (area, gap, cw, ch) = (self.main_area, self.pane_gap, self.cell_w, self.cell_h);
        for slot in &mut self.slots {
            let Some(root) = slot.root.take() else {
                continue;
            };
            let (next_root, removed) = remove_node(root, pane_id);
            slot.root = next_root;
            if let Some(info) = removed {
                if let Some(cwd) = info.cwd {
                    slot.last_cwd = cwd;
                }
                if let Some(r) = &slot.root {
                    if slot.focused_pane == Some(pane_id) {
                        slot.focused_pane = Some(r.first_pane_id());
                    }
                    slot.resize_to_area(area, gap, cw, ch);
                } else {
                    slot.focused_pane = None;
                    slot.saved_tree = None;
                }
                break;
            }
        }
    }

    /// アクティブスロットの分割境界線に対するヒットテスト
    pub fn hit_test_divider(
        &self,
        px: f32,
        py: f32,
        hit_half_thickness: f32,
    ) -> Option<SplitDivider> {
        let slot = &self.slots[self.active_slot];
        let tree = match slot.view_mode {
            SlotViewMode::Panes => slot.root.as_ref(),
            SlotViewMode::Browser => slot.browser_root.as_ref(),
        }?;
        tree.hit_test_divider(self.main_area, self.pane_gap, hit_half_thickness, px, py)
    }

    /// アクティブスロットの指定分割ノードの比率を更新し、ペインを再レイアウトする
    pub fn set_active_split_ratio(&mut self, path: &[bool], ratio: f32) -> bool {
        let (area, gap, cw, ch) = (self.main_area, self.pane_gap, self.cell_w, self.cell_h);
        let min_w = cw * 10.0;
        let min_h = ch * 4.0;
        let slot = &mut self.slots[self.active_slot];
        let changed = match slot.view_mode {
            SlotViewMode::Panes => slot
                .root
                .as_mut()
                .is_some_and(|r| r.set_split_ratio(path, ratio, area, gap, min_w, min_h)),
            SlotViewMode::Browser => slot
                .browser_root
                .as_mut()
                .is_some_and(|r| r.set_split_ratio(path, ratio, area, gap, min_w, min_h)),
        };
        if changed && slot.view_mode == SlotViewMode::Panes {
            slot.resize_to_area(area, gap, cw, ch);
        }
        changed
    }

    /// フォーカス中ペインに最も近い該当軸の分割境界をキーボード操作で調整する
    pub fn resize_focused_pane_dir(&mut self, dir: FocusDir, step: f32) -> bool {
        let (axis, delta) = match dir {
            FocusDir::Left => (SplitAxis::Vertical, -step),
            FocusDir::Right => (SplitAxis::Vertical, step),
            FocusDir::Up => (SplitAxis::Horizontal, -step),
            FocusDir::Down => (SplitAxis::Horizontal, step),
            FocusDir::Prev | FocusDir::Next => return false,
        };
        let (area, gap, cw, ch) = (self.main_area, self.pane_gap, self.cell_w, self.cell_h);
        let min_w = cw * 10.0;
        let min_h = ch * 4.0;
        let slot = &mut self.slots[self.active_slot];
        let Some(target_id) = slot.active_focused_pane() else {
            return false;
        };
        let changed = match slot.view_mode {
            SlotViewMode::Panes => slot.root.as_mut().is_some_and(|r| {
                r.resize_pane_axis(target_id, axis, delta, area, gap, min_w, min_h)
            }),
            SlotViewMode::Browser => slot.browser_root.as_mut().is_some_and(|r| {
                r.resize_pane_axis(target_id, axis, delta, area, gap, min_w, min_h)
            }),
        };
        if changed && slot.view_mode == SlotViewMode::Panes {
            slot.resize_to_area(area, gap, cw, ch);
        }
        changed
    }

    pub fn focus_pane_dir(&mut self, dir: FocusDir) {
        let slot = &mut self.slots[self.active_slot];
        let mut rects = slot.compute_pane_rects(self.main_area, self.pane_gap);
        if rects.len() <= 1 {
            return;
        }
        let Some(cur_id) = slot.active_focused_pane() else {
            return;
        };

        let next_id = select_next_pane(&mut rects, cur_id, dir);
        match slot.view_mode {
            SlotViewMode::Browser => slot.browser_focused_pane = Some(next_id),
            SlotViewMode::Panes => slot.focused_pane = Some(next_id),
        }
    }

    pub fn pane_at(&self, px: f32, py: f32) -> Option<(PaneId, Rect)> {
        let slot = &self.slots[self.active_slot];
        let rects = slot.compute_pane_rects(self.main_area, self.pane_gap);
        for (id, rect) in rects {
            if rect.contains(px, py) {
                return Some((id, rect));
            }
        }
        None
    }

    pub fn focus_pane_at(&mut self, px: f32, py: f32) -> Option<(PaneId, Rect)> {
        let slot = &mut self.slots[self.active_slot];
        let rects = slot.compute_pane_rects(self.main_area, self.pane_gap);
        for (id, rect) in rects {
            if rect.contains(px, py) {
                match slot.view_mode {
                    SlotViewMode::Browser => slot.browser_focused_pane = Some(id),
                    SlotViewMode::Panes => slot.focused_pane = Some(id),
                }
                return Some((id, rect));
            }
        }
        None
    }

    pub fn resize_all(&mut self, main_area: Rect, pane_gap: f32, cell_w: f32, cell_h: f32) {
        self.main_area = main_area;
        self.pane_gap = pane_gap;
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        for slot in &mut self.slots {
            slot.resize_to_area(main_area, pane_gap, cell_w, cell_h);
        }
    }

    pub fn feed_output(&mut self, pane_id: PaneId, bytes: &[u8], window_focused: bool) {
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            if let Some(root) = &mut slot.root
                && let Some(pane) = root.terminal_pane_mut(pane_id)
            {
                let is_active = idx == self.active_slot;
                let label = Slot::number_label(idx);
                pane.feed_pty_bytes(bytes, is_active, window_focused, label);
                slot.last_cwd = pane.cwd.clone();
                return;
            }
        }
    }

    /// 過負荷再同期などでデーモンから届いた最新スナップショットを該当ペインへ適用する
    pub fn apply_resync(&mut self, replayed: ReplayedPane) -> bool {
        let pane_id = replayed.pane_id;
        for slot in &mut self.slots {
            if let Some(root) = &mut slot.root
                && let Some(pane) = root.terminal_pane_mut(pane_id)
            {
                pane.apply_resync_snapshot(replayed);
                if slot.focused_pane == Some(pane_id) {
                    slot.last_cwd = pane.cwd.clone();
                }
                return true;
            }
        }
        false
    }

    pub fn apply_cwd_updates(&mut self, updates: Vec<(PaneId, PathBuf)>) -> bool {
        let mut changed = false;
        for (pane_id, cwd) in updates {
            for slot in &mut self.slots {
                if let Some(root) = &mut slot.root
                    && let Some(pane) = root.terminal_pane_mut(pane_id)
                {
                    if pane.update_cwd_from_daemon(cwd) {
                        changed = true;
                    }
                    if slot.focused_pane == Some(pane_id) {
                        slot.last_cwd = pane.cwd.clone();
                    }
                    break;
                }
            }
        }
        changed
    }

    pub fn has_any_active_output(&self, now: Instant) -> bool {
        self.slots.iter().any(|s| s.is_actively_outputting(now))
    }
}
