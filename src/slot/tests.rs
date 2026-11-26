use std::path::PathBuf;

use crate::daemon::{DaemonClient, ReplayedPane};
use crate::key_map::FocusDir;
use crate::layout::Rect;
use crate::state::SavedPaneTree;

use super::pane_tree::select_next_pane;
use super::{SplitAxis, Workspace};

#[test]
fn test_visual_pane_focus_order() {
    let mut rects = vec![
        (1, Rect::new(0.0, 0.0, 100.0, 100.0)),
        (2, Rect::new(0.0, 106.0, 100.0, 100.0)),
        (3, Rect::new(106.0, 0.0, 100.0, 100.0)),
        (4, Rect::new(106.0, 106.0, 100.0, 100.0)),
    ];
    assert_eq!(select_next_pane(&mut rects, 1, FocusDir::Next), 3);
    assert_eq!(select_next_pane(&mut rects, 3, FocusDir::Next), 2);
    assert_eq!(select_next_pane(&mut rects, 2, FocusDir::Next), 4);
    assert_eq!(select_next_pane(&mut rects, 4, FocusDir::Next), 1);
    assert_eq!(select_next_pane(&mut rects, 1, FocusDir::Prev), 4);

    assert_eq!(select_next_pane(&mut rects, 1, FocusDir::Right), 3);
    assert_eq!(select_next_pane(&mut rects, 1, FocusDir::Down), 2);
    assert_eq!(select_next_pane(&mut rects, 4, FocusDir::Up), 3);
    assert_eq!(select_next_pane(&mut rects, 4, FocusDir::Left), 2);
}

#[test]
fn test_session_restore_never_duplicates_browser_pane_ids() {
    let mut saved_slots = crate::state::SavedState::default_home().slots;
    // スロット0: 保存済み browser_tree (ID=3)
    saved_slots[0].is_browser_mode = true;
    saved_slots[0].browser_tree = Some(SavedPaneTree::Browser {
        pane_id: 3,
        url: "https://example.com/a".to_string(),
    });
    // スロット1: 通常ペイン木に 生存端末(ID=1) + ブラウザ(ID=2)
    saved_slots[1].tree = Some(SavedPaneTree::Split {
        axis: SplitAxis::Vertical,
        ratio: 0.5,
        first: Box::new(SavedPaneTree::Leaf {
            pane_id: 1,
            cwd: PathBuf::from("/tmp"),
        }),
        second: Box::new(SavedPaneTree::Browser {
            pane_id: 2,
            url: "https://example.com/b".to_string(),
        }),
    });

    let replayed = vec![ReplayedPane {
        pane_id: 1,
        cols: 80,
        rows: 24,
        cwd: PathBuf::from("/tmp"),
        replay_bytes: Vec::new(),
    }];

    let ws = Workspace::from_session(
        Rect::new(0.0, 0.0, 1200.0, 800.0),
        6.0,
        10.0,
        20.0,
        saved_slots,
        1,
        replayed,
        2, // デーモン側の次IDが2（ブラウザID=2,3を知らない状態）
        DaemonClient::dummy_for_test(),
    );

    let ids = ws.all_browser_ids();
    assert_eq!(ids.len(), 2);
    assert_ne!(
        ids[0], ids[1],
        "browser pane IDs must be unique across slots: {ids:?}"
    );
}
