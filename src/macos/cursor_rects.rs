/// アプリ側で指定するカーソル種別（`TentMetalView::resetCursorRects` 用）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AppCursorKind {
    Default = 0,
    Pointer = 1,
    EwResize = 2,
    NsResize = 3,
}

/// 2D 軸平行矩形 `bounds` から複数の除外矩形 `cuts` を差し引いた非重複矩形群を返す
/// （`TentMetalView::resetCursorRects` で表示中の兄弟 `WKWebView` 領域を除外するため）
pub fn subtract_cursor_rects(
    bounds: (f64, f64, f64, f64),
    cuts: &[(f64, f64, f64, f64)],
) -> Vec<(f64, f64, f64, f64)> {
    let mut regions = vec![bounds];
    for &(cx, cy, cw, ch) in cuts {
        if cw <= 0.0 || ch <= 0.0 {
            continue;
        }
        let cx1 = cx + cw;
        let cy1 = cy + ch;
        let mut next = Vec::with_capacity(regions.len() + 3);
        for (rx, ry, rw, rh) in regions {
            let rx1 = rx + rw;
            let ry1 = ry + rh;
            let ix0 = rx.max(cx);
            let iy0 = ry.max(cy);
            let ix1 = rx1.min(cx1);
            let iy1 = ry1.min(cy1);
            if ix0 >= ix1 || iy0 >= iy1 {
                next.push((rx, ry, rw, rh));
                continue;
            }
            if ry1 > iy1 {
                next.push((rx, iy1, rw, ry1 - iy1));
            }
            if iy0 > ry {
                next.push((rx, ry, rw, iy0 - ry));
            }
            if ix0 > rx {
                next.push((rx, iy0, ix0 - rx, iy1 - iy0));
            }
            if rx1 > ix1 {
                next.push((ix1, iy0, rx1 - ix1, iy1 - iy0));
            }
        }
        regions = next;
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subtract_cursor_rects() {
        let bounds = (0.0, 0.0, 1000.0, 800.0);
        // ブラウザペイン（右半分）を除外したとき、左半分の 1 矩形だけが残ること
        let res = subtract_cursor_rects(bounds, &[(500.0, 0.0, 500.0, 800.0)]);
        assert_eq!(res, vec![(0.0, 0.0, 500.0, 800.0)]);

        // 中央の矩形を除外すると上下左右の 4 矩形へ分割され、総面積が一致すること
        let res = subtract_cursor_rects(bounds, &[(200.0, 100.0, 600.0, 500.0)]);
        assert_eq!(res.len(), 4);
        let area: f64 = res.iter().map(|(_, _, w, h)| w * h).sum();
        assert!((area - (1000.0 * 800.0 - 600.0 * 500.0)).abs() < 1e-6);

        // 除外なしなら元の矩形のまま
        assert_eq!(subtract_cursor_rects(bounds, &[]), vec![bounds]);
    }
}
