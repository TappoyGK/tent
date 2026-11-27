use std::path::PathBuf;

use crate::browser::BrowserPaneMeta;
use crate::key_map::FocusDir;
use crate::layout::Rect;
use crate::state::SavedPaneTree;
use crate::terminal_pane::{PaneId, TerminalPane};

/// ペイン分割方向
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitAxis {
    /// 左右分割 (Cmd+D)
    Vertical,
    /// 上下分割 (Cmd+Shift+D)
    Horizontal,
}

/// 分割ペイン間の境界線情報（マウスホバー・ドラッグリサイズ用）
#[derive(Clone, Debug)]
pub struct SplitDivider {
    /// ルートから対象 `PaneNode::Split` までの経路（false = first, true = second）
    pub path: Vec<bool>,
    pub axis: SplitAxis,
    /// 対象 `Split` ノードが占有する親領域矩形
    pub parent_rect: Rect,
}

/// ドラッグ中のポインタ座標から分割比率（`0.1..=0.9`）を算出する
pub fn ratio_from_pointer(axis: SplitAxis, parent_rect: Rect, gap: f32, px: f32, py: f32) -> f32 {
    match axis {
        SplitAxis::Vertical => {
            let avail = (parent_rect.w - gap).max(2.0);
            ((px - parent_rect.x - gap * 0.5) / avail).clamp(0.1, 0.9)
        }
        SplitAxis::Horizontal => {
            let avail = (parent_rect.h - gap).max(2.0);
            ((py - parent_rect.y - gap * 0.5) / avail).clamp(0.1, 0.9)
        }
    }
}

/// スロットの表示モード（ペイン分割ビュー ⇔ スロット専用フルブラウザ）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SlotViewMode {
    #[default]
    Panes,
    Browser,
}

/// 分割木内の1ペイン（ターミナルまたは組み込みブラウザ）
pub enum Pane {
    Terminal(Box<TerminalPane>),
    Browser(Box<BrowserPaneMeta>),
}

impl Pane {
    pub fn id(&self) -> PaneId {
        match self {
            Pane::Terminal(t) => t.id,
            Pane::Browser(b) => b.id,
        }
    }

    pub fn as_terminal(&self) -> Option<&TerminalPane> {
        match self {
            Pane::Terminal(t) => Some(t),
            Pane::Browser(_) => None,
        }
    }

    pub fn as_terminal_mut(&mut self) -> Option<&mut TerminalPane> {
        match self {
            Pane::Terminal(t) => Some(t),
            Pane::Browser(_) => None,
        }
    }

    pub fn as_browser(&self) -> Option<&BrowserPaneMeta> {
        match self {
            Pane::Browser(b) => Some(b),
            Pane::Terminal(_) => None,
        }
    }

    pub fn as_browser_mut(&mut self) -> Option<&mut BrowserPaneMeta> {
        match self {
            Pane::Browser(b) => Some(b),
            Pane::Terminal(_) => None,
        }
    }
}

/// ペイン分割の二分木
pub enum PaneNode {
    Leaf(Pane),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        match self {
            PaneNode::Leaf(p) => (p.id() == id).then_some(p),
            PaneNode::Split { first, second, .. } => first.pane(id).or_else(|| second.pane(id)),
        }
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            PaneNode::Leaf(p) => (p.id() == id).then_some(p),
            PaneNode::Split { first, second, .. } => {
                first.pane_mut(id).or_else(|| second.pane_mut(id))
            }
        }
    }

    pub fn terminal_pane(&self, id: PaneId) -> Option<&TerminalPane> {
        self.pane(id).and_then(Pane::as_terminal)
    }

    pub fn terminal_pane_mut(&mut self, id: PaneId) -> Option<&mut TerminalPane> {
        self.pane_mut(id).and_then(Pane::as_terminal_mut)
    }

    pub fn first_pane(&self) -> &Pane {
        match self {
            PaneNode::Leaf(p) => p,
            PaneNode::Split { first, .. } => first.first_pane(),
        }
    }

    pub fn first_pane_id(&self) -> PaneId {
        self.first_pane().id()
    }

    pub fn for_each_terminal(&self, f: &mut impl FnMut(&TerminalPane)) {
        match self {
            PaneNode::Leaf(Pane::Terminal(t)) => f(t),
            PaneNode::Leaf(Pane::Browser(_)) => {}
            PaneNode::Split { first, second, .. } => {
                first.for_each_terminal(f);
                second.for_each_terminal(f);
            }
        }
    }

    pub fn for_each_terminal_mut(&mut self, f: &mut impl FnMut(&mut TerminalPane)) {
        match self {
            PaneNode::Leaf(Pane::Terminal(t)) => f(t),
            PaneNode::Leaf(Pane::Browser(_)) => {}
            PaneNode::Split { first, second, .. } => {
                first.for_each_terminal_mut(f);
                second.for_each_terminal_mut(f);
            }
        }
    }

    pub fn for_each_browser(&self, f: &mut impl FnMut(&BrowserPaneMeta)) {
        match self {
            PaneNode::Leaf(Pane::Browser(b)) => f(b),
            PaneNode::Leaf(Pane::Terminal(_)) => {}
            PaneNode::Split { first, second, .. } => {
                first.for_each_browser(f);
                second.for_each_browser(f);
            }
        }
    }

    pub fn to_saved_tree(&self) -> SavedPaneTree {
        match self {
            PaneNode::Leaf(Pane::Terminal(p)) => SavedPaneTree::Leaf {
                pane_id: p.id,
                cwd: p.cwd.clone(),
            },
            PaneNode::Leaf(Pane::Browser(b)) => SavedPaneTree::Browser {
                pane_id: b.id,
                url: b.url.clone(),
            },
            PaneNode::Split {
                axis,
                ratio,
                first,
                second,
            } => SavedPaneTree::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(first.to_saved_tree()),
                second: Box::new(second.to_saved_tree()),
            },
        }
    }

    /// 指定領域を分割木に従って再帰的に割り当て、各ペインの矩形リストを返す
    pub fn compute_rects(&self, area: Rect, gap: f32, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            PaneNode::Leaf(p) => out.push((p.id(), area)),
            PaneNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (r1, r2) = match axis {
                    SplitAxis::Vertical => {
                        let avail = (area.w - gap).max(2.0);
                        let w1 = (avail * *ratio).max(1.0);
                        let w2 = (avail - w1).max(1.0);
                        (
                            Rect::new(area.x, area.y, w1, area.h),
                            Rect::new(area.x + w1 + gap, area.y, w2, area.h),
                        )
                    }
                    SplitAxis::Horizontal => {
                        let avail = (area.h - gap).max(2.0);
                        let h1 = (avail * *ratio).max(1.0);
                        let h2 = (avail - h1).max(1.0);
                        (
                            Rect::new(area.x, area.y, area.w, h1),
                            Rect::new(area.x, area.y + h1 + gap, area.w, h2),
                        )
                    }
                };
                first.compute_rects(r1, gap, out);
                second.compute_rects(r2, gap, out);
            }
        }
    }

    /// 指定座標 `(px, py)` が分割境界線（幅 `hit_half_thickness * 2`）上にあれば該当境界情報を返す
    pub fn hit_test_divider(
        &self,
        area: Rect,
        gap: f32,
        hit_half_thickness: f32,
        px: f32,
        py: f32,
    ) -> Option<SplitDivider> {
        let mut path = Vec::new();
        self.hit_test_divider_inner(area, gap, hit_half_thickness, px, py, &mut path)
    }

    fn hit_test_divider_inner(
        &self,
        area: Rect,
        gap: f32,
        hit_half_thickness: f32,
        px: f32,
        py: f32,
        path: &mut Vec<bool>,
    ) -> Option<SplitDivider> {
        let PaneNode::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return None;
        };

        let half = hit_half_thickness.max(gap * 0.5);
        let (r1, r2, hit_rect) = match axis {
            SplitAxis::Vertical => {
                let avail = (area.w - gap).max(2.0);
                let w1 = (avail * *ratio).max(1.0);
                let w2 = (avail - w1).max(1.0);
                let center_x = area.x + w1 + gap * 0.5;
                (
                    Rect::new(area.x, area.y, w1, area.h),
                    Rect::new(area.x + w1 + gap, area.y, w2, area.h),
                    Rect::new(center_x - half, area.y, half * 2.0, area.h),
                )
            }
            SplitAxis::Horizontal => {
                let avail = (area.h - gap).max(2.0);
                let h1 = (avail * *ratio).max(1.0);
                let h2 = (avail - h1).max(1.0);
                let center_y = area.y + h1 + gap * 0.5;
                (
                    Rect::new(area.x, area.y, area.w, h1),
                    Rect::new(area.x, area.y + h1 + gap, area.w, h2),
                    Rect::new(area.x, center_y - half, area.w, half * 2.0),
                )
            }
        };

        if hit_rect.contains(px, py) {
            return Some(SplitDivider {
                path: path.clone(),
                axis: *axis,
                parent_rect: area,
            });
        }

        path.push(false);
        if let Some(found) = first.hit_test_divider_inner(r1, gap, hit_half_thickness, px, py, path)
        {
            return Some(found);
        }
        path.pop();

        path.push(true);
        let found = second.hit_test_divider_inner(r2, gap, hit_half_thickness, px, py, path);
        path.pop();
        found
    }

    /// 経路 `path` で指定された `Split` ノードの比率を `ratio`（`0.1..=0.9`）へ更新する
    pub fn set_split_ratio(&mut self, path: &[bool], ratio: f32) -> bool {
        let clamped = ratio.clamp(0.1, 0.9);
        match (self, path.split_first()) {
            (PaneNode::Split { ratio: r, .. }, None) => {
                if (*r - clamped).abs() > 1e-4 {
                    *r = clamped;
                    true
                } else {
                    false
                }
            }
            (PaneNode::Split { first, second, .. }, Some((go_second, rest))) => {
                if *go_second {
                    second.set_split_ratio(rest, clamped)
                } else {
                    first.set_split_ratio(rest, clamped)
                }
            }
            _ => false,
        }
    }

    /// `target_id` を含む最も内側の指定軸 `Split` ノードの比率を `delta` 分だけ調整する
    pub fn resize_pane_axis(&mut self, target_id: PaneId, axis: SplitAxis, delta: f32) -> bool {
        let mut path = Vec::new();
        let Some(target_split_path) = self.find_nearest_split_path(target_id, axis, &mut path)
        else {
            return false;
        };
        let mut node: &mut PaneNode = self;
        for go_second in target_split_path {
            match node {
                PaneNode::Split { first, second, .. } => {
                    node = if go_second { second } else { first };
                }
                PaneNode::Leaf(_) => return false,
            }
        }
        if let PaneNode::Split { ratio, .. } = node {
            let next = (*ratio + delta).clamp(0.1, 0.9);
            if (*ratio - next).abs() > 1e-4 {
                *ratio = next;
                return true;
            }
        }
        false
    }

    fn find_nearest_split_path(
        &self,
        target_id: PaneId,
        target_axis: SplitAxis,
        cur_path: &mut Vec<bool>,
    ) -> Option<Vec<bool>> {
        match self {
            PaneNode::Leaf(_) => None,
            PaneNode::Split {
                axis,
                first,
                second,
                ..
            } => {
                let in_first = first.pane(target_id).is_some();
                let in_second = !in_first && second.pane(target_id).is_some();
                if !in_first && !in_second {
                    return None;
                }
                let child_res = if in_first {
                    cur_path.push(false);
                    let r = first.find_nearest_split_path(target_id, target_axis, cur_path);
                    cur_path.pop();
                    r
                } else {
                    cur_path.push(true);
                    let r = second.find_nearest_split_path(target_id, target_axis, cur_path);
                    cur_path.pop();
                    r
                };
                if child_res.is_some() {
                    child_res
                } else if *axis == target_axis {
                    Some(cur_path.clone())
                } else {
                    None
                }
            }
        }
    }
}

pub(super) fn split_node_mut(
    node: PaneNode,
    target_id: PaneId,
    axis: SplitAxis,
    new_pane: &mut Option<Pane>,
) -> (PaneNode, bool) {
    match node {
        PaneNode::Leaf(p) if p.id() == target_id => {
            let second = new_pane.take().expect("new_pane available");
            (
                PaneNode::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(PaneNode::Leaf(p)),
                    second: Box::new(PaneNode::Leaf(second)),
                },
                true,
            )
        }
        PaneNode::Leaf(p) => (PaneNode::Leaf(p), false),
        PaneNode::Split {
            axis: a,
            ratio,
            first,
            second,
        } => {
            let (first, ok) = split_node_mut(*first, target_id, axis, new_pane);
            if ok {
                return (
                    PaneNode::Split {
                        axis: a,
                        ratio,
                        first: Box::new(first),
                        second,
                    },
                    true,
                );
            }
            let (second, ok2) = split_node_mut(*second, target_id, axis, new_pane);
            (
                PaneNode::Split {
                    axis: a,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                },
                ok2,
            )
        }
    }
}

pub(super) struct RemovedInfo {
    pub(super) cwd: Option<PathBuf>,
}

pub(super) fn remove_node(
    node: PaneNode,
    target_id: PaneId,
) -> (Option<PaneNode>, Option<RemovedInfo>) {
    match node {
        PaneNode::Leaf(p) => {
            if p.id() == target_id {
                let cwd = match p {
                    Pane::Terminal(t) => Some(t.cwd),
                    Pane::Browser(_) => None,
                };
                (None, Some(RemovedInfo { cwd }))
            } else {
                (Some(PaneNode::Leaf(p)), None)
            }
        }
        PaneNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (f_opt, removed) = remove_node(*first, target_id);
            if removed.is_some() {
                return match f_opt {
                    Some(f) => (
                        Some(PaneNode::Split {
                            axis,
                            ratio,
                            first: Box::new(f),
                            second,
                        }),
                        removed,
                    ),
                    None => (Some(*second), removed),
                };
            }
            let first_kept = f_opt.expect("first not removed");
            let (s_opt, removed2) = remove_node(*second, target_id);
            if removed2.is_some() {
                return match s_opt {
                    Some(s) => (
                        Some(PaneNode::Split {
                            axis,
                            ratio,
                            first: Box::new(first_kept),
                            second: Box::new(s),
                        }),
                        removed2,
                    ),
                    None => (Some(first_kept), removed2),
                };
            }
            let second_kept = s_opt.expect("second not removed");
            (
                Some(PaneNode::Split {
                    axis,
                    ratio,
                    first: Box::new(first_kept),
                    second: Box::new(second_kept),
                }),
                None,
            )
        }
    }
}

pub(super) fn select_next_pane(
    rects: &mut [(PaneId, Rect)],
    cur_id: PaneId,
    dir: FocusDir,
) -> PaneId {
    match dir {
        FocusDir::Prev | FocusDir::Next => {
            rects.sort_by(|a, b| {
                let ay = (a.1.y / 8.0).round() as i32;
                let by = (b.1.y / 8.0).round() as i32;
                ay.cmp(&by).then_with(|| {
                    ((a.1.x / 8.0).round() as i32).cmp(&((b.1.x / 8.0).round() as i32))
                })
            });
            let Some(cur_idx) = rects.iter().position(|(id, _)| *id == cur_id) else {
                return cur_id;
            };
            let next_idx = if dir == FocusDir::Next {
                (cur_idx + 1) % rects.len()
            } else {
                (cur_idx + rects.len() - 1) % rects.len()
            };
            rects[next_idx].0
        }
        FocusDir::Left | FocusDir::Right | FocusDir::Up | FocusDir::Down => {
            let Some(&(_, cur_r)) = rects.iter().find(|(id, _)| *id == cur_id) else {
                return cur_id;
            };
            let cur_cx = cur_r.x + cur_r.w * 0.5;
            let cur_cy = cur_r.y + cur_r.h * 0.5;
            let mut best: Option<(PaneId, bool, f32)> = None;

            for &(id, r) in rects.iter() {
                if id == cur_id {
                    continue;
                }
                let cx = r.x + r.w * 0.5;
                let cy = r.y + r.h * 0.5;
                let (primary, secondary, overlaps) = match dir {
                    FocusDir::Left if r.right() <= cur_r.x + 2.0 => {
                        let ov = r.y < cur_r.bottom() && r.bottom() > cur_r.y;
                        (cur_r.x - r.right(), (cy - cur_cy).abs(), ov)
                    }
                    FocusDir::Right if r.x >= cur_r.right() - 2.0 => {
                        let ov = r.y < cur_r.bottom() && r.bottom() > cur_r.y;
                        (r.x - cur_r.right(), (cy - cur_cy).abs(), ov)
                    }
                    FocusDir::Up if r.bottom() <= cur_r.y + 2.0 => {
                        let ov = r.x < cur_r.right() && r.right() > cur_r.x;
                        (cur_r.y - r.bottom(), (cx - cur_cx).abs(), ov)
                    }
                    FocusDir::Down if r.y >= cur_r.bottom() - 2.0 => {
                        let ov = r.x < cur_r.right() && r.right() > cur_r.x;
                        (r.y - cur_r.bottom(), (cx - cur_cx).abs(), ov)
                    }
                    _ => continue,
                };
                let score = primary.max(0.0) * 4.0 + secondary;
                let better = match best {
                    None => true,
                    Some((_, best_ov, best_score)) => {
                        (overlaps && !best_ov) || (overlaps == best_ov && score < best_score)
                    }
                };
                if better {
                    best = Some((id, overlaps, score));
                }
            }
            best.map(|(id, _, _)| id).unwrap_or(cur_id)
        }
    }
}
