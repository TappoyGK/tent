/// アプリ側で指定するカーソル種別（`WinitView::resetCursorRects` 差し替え用）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AppCursorKind {
    Default = 0,
    Pointer = 1,
    EwResize = 2,
    NsResize = 3,
}

#[cfg(target_os = "macos")]
static APP_CURSOR_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `WinitView::resetCursorRects` で使用するカーソル種別を更新する
pub fn set_app_cursor_kind(kind: AppCursorKind) {
    #[cfg(target_os = "macos")]
    APP_CURSOR_KIND.store(kind as u8, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_os = "macos"))]
    let _ = kind;
}

/// 2D 軸平行矩形 `bounds` から複数の除外矩形 `cuts` を差し引いた非重複矩形群を返す
/// （`WinitView::resetCursorRects` で表示中の `WKWebView` 領域を除外するため）
fn subtract_cursor_rects(
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

/// macOS の `WinitView::resetCursorRects` を差し替え、表示中の子 `WKWebView` 上には
/// 親ビューの `addCursorRect:` を張らないようにする（Web ページの CSS カーソル切替を阻害しないため）。
#[cfg(target_os = "macos")]
pub fn install_cursor_rect_passthrough(window: &winit::window::Window) {
    use std::ffi::c_char;

    use objc2::ffi::{class_replaceMethod, object_getClass};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_foundation::NSArray;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    unsafe extern "C" fn reset_cursor_rects_excluding_subviews(this: *mut AnyObject, _cmd: Sel) {
        if this.is_null() {
            return;
        }
        let bounds: CGRect = unsafe { msg_send![this, bounds] };
        let subviews: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![this, subviews] };
        let mut excluded = Vec::new();
        if let Some(subs) = subviews {
            for sub in subs.iter() {
                let hidden: Bool = unsafe { msg_send![&*sub, isHidden] };
                if hidden.as_bool() {
                    continue;
                }
                let frame: CGRect = unsafe { msg_send![&*sub, frame] };
                if frame.size.width > 0.0 && frame.size.height > 0.0 {
                    excluded.push((
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    ));
                }
            }
        }

        // 注: winit の resetCursorRects を直接差し替えるため、TenT が使用する
        // Default / Pointer / EwResize / NsResize を APP_CURSOR_KIND で反映している。
        // 今後ほかのカーソルアイコンや set_cursor_visible(false) を使う場合はここも合わせて拡張する。
        let Some(cursor_cls) = AnyClass::get(c"NSCursor") else {
            return;
        };
        let cursor: *mut AnyObject =
            match APP_CURSOR_KIND.load(std::sync::atomic::Ordering::Relaxed) {
                1 => unsafe { msg_send![cursor_cls, pointingHandCursor] },
                2 => unsafe { msg_send![cursor_cls, resizeLeftRightCursor] },
                3 => unsafe { msg_send![cursor_cls, resizeUpDownCursor] },
                _ => unsafe { msg_send![cursor_cls, arrowCursor] },
            };
        if cursor.is_null() {
            return;
        }

        let regions = subtract_cursor_rects(
            (
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
            ),
            &excluded,
        );
        for (x, y, w, h) in regions {
            let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
            let _: () = unsafe { msg_send![this, addCursorRect: rect, cursor: cursor] };
        }
    }

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    if view.is_null() {
        return;
    }

    unsafe {
        let view_cls = object_getClass(view.cast()).cast_mut();
        if !view_cls.is_null() {
            let imp: Imp = std::mem::transmute::<unsafe extern "C" fn(*mut AnyObject, Sel), Imp>(
                reset_cursor_rects_excluding_subviews,
            );
            let _ = class_replaceMethod(
                view_cls,
                sel!(resetCursorRects),
                imp,
                c"v@:".as_ptr().cast::<c_char>(),
            );
        }
        let ns_win: *mut AnyObject = msg_send![view, window];
        if !ns_win.is_null() {
            let _: () = msg_send![ns_win, invalidateCursorRectsForView: view];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_cursor_rect_passthrough(_window: &winit::window::Window) {}

/// 子 `WKWebView` の表示・位置変化後に `WinitView` のカーソル矩形を再計算させる
#[cfg(target_os = "macos")]
pub fn invalidate_cursor_rects(window: &winit::window::Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    if view.is_null() {
        return;
    }
    unsafe {
        let ns_win: *mut AnyObject = msg_send![view, window];
        if !ns_win.is_null() {
            let _: () = msg_send![ns_win, invalidateCursorRectsForView: view];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn invalidate_cursor_rects(_window: &winit::window::Window) {}

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
