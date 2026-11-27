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

#[test]
fn test_pane_divider_hit_test_drag_resize_keyboard_resize_and_persistence() {
    use crate::browser::BrowserPaneMeta;
    use crate::slot::{Pane, PaneNode, SplitAxis, ratio_from_pointer};
    use crate::state::SavedPaneTree;

    // 左 (id=1) と右 (上 id=2, 下 id=3) の入れ子分割木を構築
    let mut tree = PaneNode::Split {
        axis: SplitAxis::Vertical,
        ratio: 0.5,
        first: Box::new(PaneNode::Leaf(Pane::Browser(Box::new(BrowserPaneMeta {
            id: 1,
            url: "https://a.example".to_string(),
            title: String::new(),
            is_playing_audio: false,
        })))),
        second: Box::new(PaneNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(Pane::Browser(Box::new(BrowserPaneMeta {
                id: 2,
                url: "https://b.example".to_string(),
                title: String::new(),
                is_playing_audio: false,
            })))),
            second: Box::new(PaneNode::Leaf(Pane::Browser(Box::new(BrowserPaneMeta {
                id: 3,
                url: "https://c.example".to_string(),
                title: String::new(),
                is_playing_audio: false,
            })))),
        }),
    };

    let area = Rect::new(0.0, 0.0, 1006.0, 606.0);
    let gap = 6.0;

    // 1. ルート垂直境界線（x = 500..506, 中心 503）へのヒットテスト
    let v_div = tree
        .hit_test_divider(area, gap, 5.0, 503.0, 300.0)
        .expect("vertical divider hit");
    assert_eq!(v_div.axis, SplitAxis::Vertical);
    assert!(v_div.path.is_empty());

    // 2. 右側内の水平境界線（y = 300..306, 中心 303）へのヒットテスト
    let h_div = tree
        .hit_test_divider(area, gap, 5.0, 750.0, 303.0)
        .expect("horizontal divider hit");
    assert_eq!(h_div.axis, SplitAxis::Horizontal);
    assert_eq!(h_div.path, vec![true]);

    // 3. ドラッグによる比率算出＆更新（最小ペインサイズ 160x80 を守りつつクランプ）
    let min_w = 160.0;
    let min_h = 80.0;
    let new_v = ratio_from_pointer(v_div.axis, v_div.parent_rect, gap, 303.0, 300.0);
    assert!((new_v - 0.3).abs() < 0.01);
    assert!(tree.set_split_ratio(&v_div.path, new_v, area, gap, min_w, min_h));
    assert_eq!(
        ratio_from_pointer(v_div.axis, v_div.parent_rect, gap, -50.0, 300.0),
        0.1
    );
    assert_eq!(
        ratio_from_pointer(v_div.axis, v_div.parent_rect, gap, 2000.0, 300.0),
        0.9
    );

    // 極端に小さい比率 0.1 を指定しても最小幅 160px 未満にならないこと
    assert!(tree.set_split_ratio(&v_div.path, 0.1, area, gap, min_w, min_h));
    let mut clamped_rects = Vec::new();
    tree.compute_rects(area, gap, &mut clamped_rects);
    assert!(clamped_rects.iter().all(|(_, r)| r.w >= min_w - 0.5));
    // テスト用に 0.3 へ戻す
    assert!(tree.set_split_ratio(&v_div.path, 0.3, area, gap, min_w, min_h));

    // 4. キーボード操作による比率調整（id=2 に最も近い Horizontal 分割を +0.1 調整）
    assert!(tree.resize_pane_axis(2, SplitAxis::Horizontal, 0.1, area, gap, min_w, min_h));
    // id=2 に最も近い Vertical 分割（ルート）を +0.05 調整
    assert!(tree.resize_pane_axis(2, SplitAxis::Vertical, 0.05, area, gap, min_w, min_h));

    // 5. state.conf 形式（SavedPaneTree::encode / decode）で比率が保存・復元され、NaN は 0.5 になること
    let nan_decoded = SavedPaneTree::decode("S(V,NaN,B(1,u),B(2,u))").expect("decode NaN");
    assert!(matches!(
        nan_decoded,
        SavedPaneTree::Split { ratio, .. } if (ratio - 0.5).abs() < 1e-4
    ));
    let encoded = tree.to_saved_tree().encode();
    let decoded = SavedPaneTree::decode(&encoded).expect("decode saved tree");
    match decoded {
        SavedPaneTree::Split {
            axis: SplitAxis::Vertical,
            ratio: r_v,
            second,
            ..
        } => {
            assert!((r_v - 0.35).abs() < 0.01);
            match *second {
                SavedPaneTree::Split {
                    axis: SplitAxis::Horizontal,
                    ratio: r_h,
                    ..
                } => assert!((r_h - 0.6).abs() < 0.01),
                other => panic!("expected horizontal split, got {other:?}"),
            }
        }
        other => panic!("expected vertical split, got {other:?}"),
    }
}
