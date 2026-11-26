use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::layout::SidebarState;
use crate::slot::{NUM_SLOTS, SplitAxis};
use crate::terminal_pane::PaneId;

/// 保存用のウィンドウ位置・サイズ（論理ピクセル）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 保存用のペイン分割木（各葉に pane_id と cwd または browser url を保持）
#[derive(Clone, Debug, PartialEq)]
pub enum SavedPaneTree {
    Leaf {
        pane_id: PaneId,
        cwd: PathBuf,
    },
    Browser {
        pane_id: PaneId,
        url: String,
    },
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<SavedPaneTree>,
        second: Box<SavedPaneTree>,
    },
}

impl SavedPaneTree {
    /// S式風の1行テキストへシリアライズ（外部クレート不要）
    pub fn encode(&self) -> String {
        match self {
            SavedPaneTree::Leaf { pane_id, cwd } => {
                format!("L({pane_id},{})", encode_token(&cwd.to_string_lossy()))
            }
            SavedPaneTree::Browser { pane_id, url } => {
                format!("B({pane_id},{})", encode_token(url))
            }
            SavedPaneTree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let a = match axis {
                    SplitAxis::Vertical => 'V',
                    SplitAxis::Horizontal => 'H',
                };
                format!(
                    "S({a},{:.4},{},{})",
                    ratio.clamp(0.1, 0.9),
                    first.encode(),
                    second.encode()
                )
            }
        }
    }

    pub fn decode(input: &str) -> Option<Self> {
        let mut cursor = input.trim();
        let tree = Self::parse_node(&mut cursor)?;
        if cursor.trim().is_empty() {
            Some(tree)
        } else {
            None
        }
    }

    fn parse_node(cur: &mut &str) -> Option<Self> {
        if let Some(rest) = cur.strip_prefix("L(") {
            *cur = rest;
            let comma = cur.find(',')?;
            let id_str = &cur[..comma];
            let pane_id = id_str.parse::<PaneId>().ok()?;
            *cur = &cur[comma + 1..];
            let close = cur.find(')')?;
            let token = &cur[..close];
            *cur = &cur[close + 1..];
            let cwd = PathBuf::from(decode_token(token));
            Some(SavedPaneTree::Leaf { pane_id, cwd })
        } else if let Some(rest) = cur.strip_prefix("B(") {
            *cur = rest;
            let comma = cur.find(',')?;
            let id_str = &cur[..comma];
            let pane_id = id_str.parse::<PaneId>().ok()?;
            *cur = &cur[comma + 1..];
            let close = cur.find(')')?;
            let token = &cur[..close];
            *cur = &cur[close + 1..];
            let url = decode_token(token);
            Some(SavedPaneTree::Browser { pane_id, url })
        } else if let Some(rest) = cur.strip_prefix("S(") {
            *cur = rest;
            let axis = if let Some(r) = cur.strip_prefix("V,") {
                *cur = r;
                SplitAxis::Vertical
            } else {
                let r = cur.strip_prefix("H,")?;
                *cur = r;
                SplitAxis::Horizontal
            };
            let comma = cur.find(',')?;
            let ratio = cur[..comma].parse::<f32>().ok()?.clamp(0.1, 0.9);
            *cur = &cur[comma + 1..];
            let first = Self::parse_node(cur)?;
            *cur = cur.strip_prefix(',')?;
            let second = Self::parse_node(cur)?;
            *cur = cur.strip_prefix(')')?;
            Some(SavedPaneTree::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            })
        } else {
            None
        }
    }
}

/// 1スロット分の保存状態
#[derive(Clone, Debug, PartialEq)]
pub struct SavedSlot {
    pub last_cwd: PathBuf,
    pub focused_pane: Option<PaneId>,
    pub tree: Option<SavedPaneTree>,
    pub is_browser_mode: bool,
    pub browser_url: Option<String>,
    pub browser_tree: Option<SavedPaneTree>,
}

/// ~/.config/tent/state.conf に保存・復元するアプリ状態
#[derive(Clone, Debug, PartialEq)]
pub struct SavedState {
    pub sidebar_state: SidebarState,
    pub active_slot: usize,
    pub window_geometry: Option<WindowGeometry>,
    pub slots: [SavedSlot; NUM_SLOTS],
}

impl SavedState {
    pub fn default_home() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self {
            sidebar_state: SidebarState::default(),
            active_slot: 0,
            window_geometry: None,
            slots: std::array::from_fn(|_| SavedSlot {
                last_cwd: home.clone(),
                focused_pane: None,
                tree: None,
                is_browser_mode: false,
                browser_url: None,
                browser_tree: None,
            }),
        }
    }

    fn default_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("tent")
                .join("state.conf"),
        )
    }

    pub fn load() -> Self {
        match Self::default_path() {
            Some(path) => Self::load_from(&path),
            None => Self::default_home(),
        }
    }

    pub fn load_from(path: &Path) -> Self {
        let mut state = Self::default_home();
        let Ok(content) = fs::read_to_string(path) else {
            return state;
        };

        let mut win_x: Option<i32> = None;
        let mut win_y: Option<i32> = None;
        let mut win_w: Option<u32> = None;
        let mut win_h: Option<u32> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "col1_collapsed" => state.sidebar_state.col1_collapsed = val.trim() == "true",
                "col2_collapsed" => state.sidebar_state.col2_collapsed = val.trim() == "true",
                "active_slot" => {
                    if let Ok(idx) = val.trim().parse::<usize>()
                        && idx < NUM_SLOTS
                    {
                        state.active_slot = idx;
                    }
                }
                "window_x" => win_x = val.trim().parse::<i32>().ok(),
                "window_y" => win_y = val.trim().parse::<i32>().ok(),
                "window_w" => win_w = val.trim().parse::<u32>().ok(),
                "window_h" => win_h = val.trim().parse::<u32>().ok(),
                other => {
                    if let Some(rest) = other.strip_prefix("slot.") {
                        if let Some(idx_str) = rest.strip_suffix(".cwd")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && idx < NUM_SLOTS
                        {
                            let decoded = decode_token(val);
                            let p = PathBuf::from(decoded);
                            if p.is_dir() {
                                state.slots[idx].last_cwd = p;
                            }
                        } else if let Some(idx_str) = rest.strip_suffix(".focused")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && idx < NUM_SLOTS
                        {
                            state.slots[idx].focused_pane = val.trim().parse::<PaneId>().ok();
                        } else if let Some(idx_str) = rest.strip_suffix(".tree")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && idx < NUM_SLOTS
                        {
                            state.slots[idx].tree = SavedPaneTree::decode(val);
                        } else if let Some(idx_str) = rest.strip_suffix(".browser_mode")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && idx < NUM_SLOTS
                        {
                            state.slots[idx].is_browser_mode = val.trim() == "true";
                        } else if let Some(idx_str) = rest.strip_suffix(".browser_url")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && idx < NUM_SLOTS
                        {
                            let decoded = decode_token(val);
                            if !decoded.is_empty() {
                                state.slots[idx].browser_url = Some(decoded);
                            }
                        } else if let Some(idx_str) = rest.strip_suffix(".browser_tree")
                            && let Ok(idx) = idx_str.parse::<usize>()
                            && idx < NUM_SLOTS
                        {
                            state.slots[idx].browser_tree = SavedPaneTree::decode(val);
                        }
                    }
                }
            }
        }

        if let (Some(x), Some(y), Some(width), Some(height)) = (win_x, win_y, win_w, win_h)
            && width >= 400
            && height >= 300
        {
            state.window_geometry = Some(WindowGeometry {
                x,
                y,
                width,
                height,
            });
        }

        state
    }

    pub fn save(&self) {
        if let Some(path) = Self::default_path()
            && let Err(err) = self.save_to(&path)
        {
            eprintln!("TenT: failed to save state: {err}");
        }
    }

    /// 一時ファイル経由でアトミックに保存する
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = String::new();
        out.push_str(&format!(
            "col1_collapsed={}\n",
            self.sidebar_state.col1_collapsed
        ));
        out.push_str(&format!(
            "col2_collapsed={}\n",
            self.sidebar_state.col2_collapsed
        ));
        out.push_str(&format!("active_slot={}\n", self.active_slot));
        if let Some(g) = self.window_geometry {
            out.push_str(&format!("window_x={}\n", g.x));
            out.push_str(&format!("window_y={}\n", g.y));
            out.push_str(&format!("window_w={}\n", g.width));
            out.push_str(&format!("window_h={}\n", g.height));
        }
        for (i, slot) in self.slots.iter().enumerate() {
            let encoded_cwd = encode_token(&slot.last_cwd.to_string_lossy());
            out.push_str(&format!("slot.{i}.cwd={encoded_cwd}\n"));
            if let Some(fid) = slot.focused_pane {
                out.push_str(&format!("slot.{i}.focused={fid}\n"));
            }
            if let Some(tree) = &slot.tree {
                out.push_str(&format!("slot.{i}.tree={}\n", tree.encode()));
            }
            if slot.is_browser_mode {
                out.push_str(&format!("slot.{i}.browser_mode=true\n"));
            }
            if let Some(url) = &slot.browser_url {
                out.push_str(&format!("slot.{i}.browser_url={}\n", encode_token(url)));
            }
            if let Some(btree) = &slot.browser_tree {
                out.push_str(&format!("slot.{i}.browser_tree={}\n", btree.encode()));
            }
        }

        let tmp_path = path.with_extension(format!("conf.tmp.{}", std::process::id()));
        let write_res = (|| -> io::Result<()> {
            let mut file = File::create(&tmp_path)?;
            file.write_all(out.as_bytes())?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(err) = write_res {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        if let Err(err) = fs::rename(&tmp_path, path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }
}

/// 改行・カンマ・括弧・パーセントを含む任意パスを安全にパーセントエンコード
fn encode_token(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let safe =
            matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'-' | b'_' | b'~');
        if safe {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn decode_token(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_load_roundtrip_with_pane_tree() {
        let dir = std::env::temp_dir().join(format!("tent-state-test-{}", std::process::id()));
        let spaced_dir = dir.join(" dir, (with) spaces ");
        fs::create_dir_all(&spaced_dir).unwrap();
        let state_file = dir.join("state.conf");

        let mut state = SavedState::default_home();
        state.sidebar_state.col1_collapsed = true;
        state.sidebar_state.col2_collapsed = false;
        state.active_slot = 3;
        state.window_geometry = Some(WindowGeometry {
            x: 120,
            y: 80,
            width: 1600,
            height: 960,
        });
        state.slots[0].last_cwd = dir.clone();
        state.slots[0].is_browser_mode = true;
        state.slots[0].browser_url = Some("https://example.com/a?b=1&c=2".to_string());
        state.slots[3].last_cwd = spaced_dir.clone();
        state.slots[3].focused_pane = Some(42);
        state.slots[3].tree = Some(SavedPaneTree::Split {
            axis: SplitAxis::Vertical,
            ratio: 0.5,
            first: Box::new(SavedPaneTree::Leaf {
                pane_id: 41,
                cwd: dir.clone(),
            }),
            second: Box::new(SavedPaneTree::Split {
                axis: SplitAxis::Horizontal,
                ratio: 0.5,
                first: Box::new(SavedPaneTree::Leaf {
                    pane_id: 42,
                    cwd: spaced_dir.clone(),
                }),
                second: Box::new(SavedPaneTree::Browser {
                    pane_id: 43,
                    url: "http://localhost:3000/app".to_string(),
                }),
            }),
        });

        state.save_to(&state_file).unwrap();
        let loaded = SavedState::load_from(&state_file);

        assert!(loaded.sidebar_state.col1_collapsed);
        assert!(!loaded.sidebar_state.col2_collapsed);
        assert_eq!(loaded.active_slot, 3);
        assert_eq!(loaded.window_geometry, state.window_geometry);
        assert_eq!(loaded.slots[0].last_cwd, dir);
        assert!(loaded.slots[0].is_browser_mode);
        assert_eq!(
            loaded.slots[0].browser_url.as_deref(),
            Some("https://example.com/a?b=1&c=2")
        );
        assert_eq!(loaded.slots[3].last_cwd, spaced_dir);
        assert_eq!(loaded.slots[3].focused_pane, Some(42));
        assert_eq!(loaded.slots[3].tree, state.slots[3].tree);

        let _ = fs::remove_dir_all(&dir);
    }
}
