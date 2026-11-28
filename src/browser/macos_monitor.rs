#[cfg(target_os = "macos")]
use std::ptr::NonNull;
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::Ordering;

use winit::event_loop::EventLoopProxy;

#[cfg(target_os = "macos")]
use crate::key_map::{AppShortcut, FocusDir};
use crate::terminal_pane::AppEvent;

/// WKWebView (WryWebView) にフォーカスがある間も TenT のグローバルショートカット
/// （Cmd+1..0, Cmd+B, Cmd+Shift+B, Cmd+L, Cmd+R, Cmd+D, Cmd+W 等）を受け取り、
/// 分割内のブラウザクリックでもペインフォーカスを同期するための macOS AppKit モニタ
#[cfg(target_os = "macos")]
pub fn install_macos_shortcut_monitor(proxy: EventLoopProxy<AppEvent>) {
    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::runtime::NSObject;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
    use std::sync::atomic::AtomicUsize;

    let last_hovered_view = Arc::new(AtomicUsize::new(0));
    let last_hovered_clone = Arc::clone(&last_hovered_view);

    let block = RcBlock::new(move |event_ptr: NonNull<NSEvent>| -> *mut NSEvent {
        let event = unsafe { event_ptr.as_ref() };
        let ev_type = event.r#type();

        if ev_type == NSEventType::MouseMoved {
            if let Some(mtm) = MainThreadMarker::new()
                && let Some(win) = event.window(mtm)
                && win.isKeyWindow()
                && let Some(content) = win.contentView()
            {
                let loc = event.locationInWindow();
                if let Some(hit_view) = content.hitTest(loc) {
                    let obj: &NSObject = &hit_view;
                    let is_winit = obj.class().name().to_string_lossy().contains("Winit");
                    if is_winit {
                        last_hovered_clone.store(0, Ordering::Relaxed);
                    } else {
                        let ptr = std::ptr::from_ref(obj) as usize;
                        if last_hovered_clone.swap(ptr, Ordering::Relaxed) != ptr {
                            let scale = win.backingScaleFactor();
                            let content_h = content.frame().size.height;
                            let px = (loc.x * scale) as f32;
                            let py = ((content_h - loc.y) * scale) as f32;
                            let _ = proxy.send_event(AppEvent::WebViewHovered { px, py });
                        }
                    }
                }
            }
            return event_ptr.as_ptr();
        }

        if ev_type == NSEventType::LeftMouseDown {
            if let Some(mtm) = MainThreadMarker::new()
                && let Some(win) = event.window(mtm)
                && let Some(content) = win.contentView()
            {
                let loc = event.locationInWindow();
                let hit_is_winit = content
                    .hitTest(loc)
                    .map(|v| {
                        let obj: &NSObject = &v;
                        obj.class().name().to_string_lossy().contains("Winit")
                    })
                    .unwrap_or(true);
                if !hit_is_winit {
                    let scale = win.backingScaleFactor();
                    let content_h = content.frame().size.height;
                    let px = (loc.x * scale) as f32;
                    let py = ((content_h - loc.y) * scale) as f32;
                    let _ = proxy.send_event(AppEvent::WebViewClicked { px, py });
                }
            }
            return event_ptr.as_ptr();
        }

        if ev_type == NSEventType::ScrollWheel {
            if event.hasPreciseScrollingDeltas()
                && let Some(mtm) = MainThreadMarker::new()
                && let Some(win) = event.window(mtm)
                && let Some(content) = win.contentView()
            {
                let loc = event.locationInWindow();
                let hit_is_winit = content
                    .hitTest(loc)
                    .map(|v| {
                        let obj: &NSObject = &v;
                        obj.class().name().to_string_lossy().contains("Winit")
                    })
                    .unwrap_or(true);
                if !hit_is_winit {
                    use objc2_app_kit::NSEventPhase;
                    use std::cell::Cell;
                    use std::time::Instant;
                    const SWIPE_THRESHOLD: f64 = 85.0;
                    thread_local! {
                        // 0: 未判定, 1: 横スワイプにロック, 2: 縦スクロールにロック
                        static SWIPE_LOCK: Cell<u8> = const { Cell::new(0) };
                        static SWIPE_DX: Cell<f64> = const { Cell::new(0.0) };
                        static SWIPE_DY: Cell<f64> = const { Cell::new(0.0) };
                        static SWALLOW_MOMENTUM_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
                    }
                    let phase = event.phase();
                    let mom = event.momentumPhase();

                    // 横スワイプ直後の純粋な横慣性のみ短時間吸収し、縦成分が少しでもあれば即解除して素通し
                    if mom != NSEventPhase::empty() {
                        let active = SWALLOW_MOMENTUM_UNTIL
                            .with(Cell::get)
                            .is_some_and(|until| Instant::now() < until);
                        if active {
                            if mom == NSEventPhase::Ended || mom == NSEventPhase::Cancelled {
                                SWALLOW_MOMENTUM_UNTIL.with(|c| c.set(None));
                            }
                            if event.scrollingDeltaY().abs() < 1.0
                                && event.scrollingDeltaX().abs() > 0.0
                            {
                                return std::ptr::null_mut();
                            }
                            SWALLOW_MOMENTUM_UNTIL.with(|c| c.set(None));
                        } else {
                            SWALLOW_MOMENTUM_UNTIL.with(|c| c.set(None));
                        }
                        return event_ptr.as_ptr();
                    }

                    if phase == NSEventPhase::Began || phase == NSEventPhase::MayBegin {
                        SWIPE_LOCK.with(|c| c.set(0));
                        SWIPE_DX.with(|c| c.set(0.0));
                        SWIPE_DY.with(|c| c.set(0.0));
                        SWALLOW_MOMENTUM_UNTIL.with(|c| c.set(None));
                    } else if phase == NSEventPhase::Changed {
                        let mut lock = SWIPE_LOCK.with(Cell::get);
                        // すでに縦スクロールと確定しているジェスチャーは一切触らずそのまま通す
                        if lock == 2 {
                            return event_ptr.as_ptr();
                        }
                        let dx = SWIPE_DX.with(|c| {
                            let v = c.get() + event.scrollingDeltaX();
                            c.set(v);
                            v
                        });
                        let dy = SWIPE_DY.with(|c| {
                            let v = c.get() + event.scrollingDeltaY();
                            c.set(v);
                            v
                        });
                        if lock == 0 {
                            if dy.abs() >= 2.0 && (dy.abs() >= 8.0 || dy.abs() >= dx.abs() * 0.35) {
                                // 縦スクロールを最優先でロック（以降、横スワイプ判定しない）
                                lock = 2;
                                SWIPE_LOCK.with(|c| c.set(2));
                            } else if dx.abs() >= 28.0
                                && dy.abs() < 8.0
                                && dx.abs() > dy.abs() * 4.0
                            {
                                // ほぼ真横にしっかり動かしたときだけ横スワイプへ移行
                                lock = 1;
                                SWIPE_LOCK.with(|c| c.set(1));
                            }
                        }
                        if lock == 1 {
                            let dir: i8 = if dx > 0.0 { -1 } else { 1 };
                            let progress = (dx.abs() / SWIPE_THRESHOLD).clamp(0.0, 1.0) as f32;
                            let ready = dx.abs() >= SWIPE_THRESHOLD;
                            let _ = proxy.send_event(AppEvent::BrowserSwipePreview {
                                dir,
                                progress,
                                ready,
                            });
                            // 横スワイプ中は WebView への横スクロールを完全に止める
                            return std::ptr::null_mut();
                        }
                    } else if phase == NSEventPhase::Ended || phase == NSEventPhase::Cancelled {
                        let lock = SWIPE_LOCK.with(Cell::get);
                        let dx = SWIPE_DX.with(Cell::get);
                        SWIPE_LOCK.with(|c| c.set(0));
                        SWIPE_DX.with(|c| c.set(0.0));
                        SWIPE_DY.with(|c| c.set(0.0));
                        if lock == 1 {
                            SWALLOW_MOMENTUM_UNTIL.with(|c| {
                                c.set(Some(Instant::now() + std::time::Duration::from_millis(180)))
                            });
                            let _ = proxy.send_event(AppEvent::BrowserSwipePreview {
                                dir: 0,
                                progress: 0.0,
                                ready: false,
                            });
                            if phase == NSEventPhase::Ended && dx.abs() >= SWIPE_THRESHOLD {
                                let shortcut = if dx > 0.0 {
                                    AppShortcut::BrowserBack
                                } else {
                                    AppShortcut::BrowserForward
                                };
                                let _ = proxy.send_event(AppEvent::Shortcut(shortcut));
                            }
                            return std::ptr::null_mut();
                        }
                    }
                }
            }
            return event_ptr.as_ptr();
        }

        if ev_type == NSEventType::Swipe {
            if let Some(mtm) = MainThreadMarker::new()
                && let Some(win) = event.window(mtm)
                && let Some(content) = win.contentView()
            {
                let loc = event.locationInWindow();
                let hit_is_winit = content
                    .hitTest(loc)
                    .map(|v| {
                        let obj: &NSObject = &v;
                        obj.class().name().to_string_lossy().contains("Winit")
                    })
                    .unwrap_or(true);
                if !hit_is_winit {
                    let dx = event.deltaX();
                    if dx > 0.0 {
                        let _ = proxy.send_event(AppEvent::Shortcut(AppShortcut::BrowserBack));
                        return std::ptr::null_mut();
                    } else if dx < 0.0 {
                        let _ = proxy.send_event(AppEvent::Shortcut(AppShortcut::BrowserForward));
                        return std::ptr::null_mut();
                    }
                }
            }
            return event_ptr.as_ptr();
        }

        if ev_type == NSEventType::KeyDown {
            let flags = event.modifierFlags();
            let has_cmd = flags.contains(NSEventModifierFlags::Command);
            let has_ctrl = flags.contains(NSEventModifierFlags::Control);

            if has_cmd && has_ctrl {
                let shift = flags.contains(NSEventModifierFlags::Shift);
                let opt = flags.contains(NSEventModifierFlags::Option);
                if !shift && !opt {
                    let dir = match event.keyCode() {
                        123 => Some(FocusDir::Left),
                        124 => Some(FocusDir::Right),
                        125 => Some(FocusDir::Down),
                        126 => Some(FocusDir::Up),
                        _ => None,
                    };
                    if let Some(dir) = dir {
                        let _ = proxy.send_event(AppEvent::Shortcut(AppShortcut::ResizePane(dir)));
                        return std::ptr::null_mut();
                    }
                }
            } else if has_cmd && !has_ctrl {
                // WinitView / WryWebView / IME / performKeyEquivalent の差異に左右されず
                // TenT の全グローバルショートカットを確実に1箇所で捕捉し、null 返却で二重発火を防ぐ
                let shift = flags.contains(NSEventModifierFlags::Shift);
                let opt = flags.contains(NSEventModifierFlags::Option);
                let key_code = event.keyCode();

                if let Some(shortcut) = match_macos_keycode_shortcut(key_code, shift, opt) {
                    let _ = proxy.send_event(AppEvent::Shortcut(shortcut));
                    return std::ptr::null_mut();
                }

                // winit 標準アプリには Edit メニューが存在せず WryWebView 子ビューの
                // performKeyEquivalent も抑止されるため、WKWebView フォーカス中の基本編集
                // ショートカット（Cmd+C/V/X/A/Z/Shift+Z/Shift+V）を First Responder へ直接配送する
                if let Some(action) = match_webview_edit_action(key_code, shift, opt)
                    && let Some(mtm) = MainThreadMarker::new()
                    && let Some(win) = event.window(mtm)
                    && unsafe { is_webview_first_responder(&win) }
                {
                    let _ = unsafe { dispatch_webview_edit_action(action) };
                    return std::ptr::null_mut();
                }
            }
        }
        event_ptr.as_ptr()
    });

    // アプリ終了まで常駐させる
    unsafe {
        let mask = NSEventMask::KeyDown
            | NSEventMask::LeftMouseDown
            | NSEventMask::MouseMoved
            | NSEventMask::ScrollWheel
            | NSEventMask::Swipe;
        let _monitor = NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block);
        std::mem::forget(_monitor);
        std::mem::forget(block);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_macos_shortcut_monitor(_proxy: EventLoopProxy<AppEvent>) {}

/// macOS の現在の物理修飾キー状態をライブ取得する（WKWebView ⇔ WinitView フォーカス切替時の修飾キーずれ防止）
#[cfg(target_os = "macos")]
pub fn current_macos_modifiers() -> winit::keyboard::ModifiersState {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags};
    use winit::keyboard::ModifiersState;

    let flags = NSEvent::modifierFlags_class();
    let mut state = ModifiersState::empty();
    if flags.contains(NSEventModifierFlags::Shift) {
        state |= ModifiersState::SHIFT;
    }
    if flags.contains(NSEventModifierFlags::Control) {
        state |= ModifiersState::CONTROL;
    }
    if flags.contains(NSEventModifierFlags::Option) {
        state |= ModifiersState::ALT;
    }
    if flags.contains(NSEventModifierFlags::Command) {
        state |= ModifiersState::SUPER;
    }
    state
}

#[cfg(not(target_os = "macos"))]
pub fn current_macos_modifiers() -> winit::keyboard::ModifiersState {
    winit::keyboard::ModifiersState::empty()
}

/// WKWebView フォーカス中に First Responder へ配送する基本編集アクション
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebViewEditAction {
    Copy,
    Paste,
    PasteAsPlainText,
    Cut,
    SelectAll,
    Undo,
    Redo,
}

#[cfg(target_os = "macos")]
fn match_webview_edit_action(key_code: u16, shift: bool, opt: bool) -> Option<WebViewEditAction> {
    if opt {
        return None;
    }
    if shift {
        return match key_code {
            0x06 => Some(WebViewEditAction::Redo),             // Shift+Z
            0x09 => Some(WebViewEditAction::PasteAsPlainText), // Shift+V
            _ => None,
        };
    }
    match key_code {
        0x08 => Some(WebViewEditAction::Copy),      // C
        0x09 => Some(WebViewEditAction::Paste),     // V
        0x07 => Some(WebViewEditAction::Cut),       // X
        0x00 => Some(WebViewEditAction::SelectAll), // A
        0x06 => Some(WebViewEditAction::Undo),      // Z
        _ => None,
    }
}

#[cfg(target_os = "macos")]
unsafe fn is_webview_first_responder(win: &objc2_app_kit::NSWindow) -> bool {
    use objc2::ffi::object_getClass;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let resp: *mut AnyObject = unsafe { msg_send![win, firstResponder] };
    if resp.is_null() {
        return false;
    }
    let cls = unsafe { object_getClass(resp.cast()) };
    if cls.is_null() {
        return false;
    }
    let name = unsafe { (*cls).name().to_string_lossy() };
    !name.contains("Winit") && name != "NSWindow"
}

#[cfg(target_os = "macos")]
unsafe fn dispatch_webview_edit_action(action: WebViewEditAction) -> bool {
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{msg_send, sel};

    let Some(app_cls) = AnyClass::get(c"NSApplication") else {
        return false;
    };
    let app: *mut AnyObject = unsafe { msg_send![app_cls, sharedApplication] };
    if app.is_null() {
        return false;
    }
    let nil: *const AnyObject = std::ptr::null();
    let send = |sel_val: objc2::runtime::Sel| -> bool {
        let ok: Bool = unsafe { msg_send![app, sendAction: sel_val, to: nil, from: nil] };
        ok.as_bool()
    };
    match action {
        WebViewEditAction::Copy => send(sel!(copy:)),
        WebViewEditAction::Paste => send(sel!(paste:)),
        WebViewEditAction::PasteAsPlainText => send(sel!(pasteAsPlainText:)) || send(sel!(paste:)),
        WebViewEditAction::Cut => send(sel!(cut:)),
        WebViewEditAction::SelectAll => send(sel!(selectAll:)),
        WebViewEditAction::Undo => send(sel!(undo:)),
        WebViewEditAction::Redo => send(sel!(redo:)),
    }
}

/// macOS Carbon 仮想キーコードから TenT ショートカットへ変換
#[cfg(target_os = "macos")]
fn match_macos_keycode_shortcut(key_code: u16, shift: bool, opt: bool) -> Option<AppShortcut> {
    if shift && !opt {
        return match key_code {
            0x02 => Some(AppShortcut::SplitHorizontal), // D
            0x0B => Some(AppShortcut::SplitBrowser),    // B
            0x2C => Some(AppShortcut::ToggleHelp),      // ? (Shift+/)
            _ => None,
        };
    }
    if opt && !shift {
        return match key_code {
            0x12 => Some(AppShortcut::ToggleCol(0)),               // 1
            0x13 => Some(AppShortcut::ToggleCol(1)),               // 2
            0x0F => Some(AppShortcut::RestartApp),                 // R
            0x7B => Some(AppShortcut::FocusPane(FocusDir::Left)),  // Left
            0x7C => Some(AppShortcut::FocusPane(FocusDir::Right)), // Right
            0x7E => Some(AppShortcut::FocusPane(FocusDir::Up)),    // Up
            0x7D => Some(AppShortcut::FocusPane(FocusDir::Down)),  // Down
            _ => None,
        };
    }
    if shift || opt {
        return None;
    }
    match key_code {
        0x12 => Some(AppShortcut::SelectSlot(0)),             // 1
        0x13 => Some(AppShortcut::SelectSlot(1)),             // 2
        0x14 => Some(AppShortcut::SelectSlot(2)),             // 3
        0x15 => Some(AppShortcut::SelectSlot(3)),             // 4
        0x17 => Some(AppShortcut::SelectSlot(4)),             // 5
        0x16 => Some(AppShortcut::SelectSlot(5)),             // 6
        0x1A => Some(AppShortcut::SelectSlot(6)),             // 7
        0x1C => Some(AppShortcut::SelectSlot(7)),             // 8
        0x19 => Some(AppShortcut::SelectSlot(8)),             // 9
        0x1D => Some(AppShortcut::SelectSlot(9)),             // 0
        0x2A => Some(AppShortcut::ToggleAllCols),             // \
        0x02 => Some(AppShortcut::SplitVertical),             // D
        0x0D => Some(AppShortcut::ClosePane),                 // W
        0x0B => Some(AppShortcut::ToggleBrowser),             // B
        0x25 => Some(AppShortcut::FocusUrlBar),               // L
        0x0F => Some(AppShortcut::ReloadBrowser),             // R
        0x7B => Some(AppShortcut::BrowserBack),               // Left
        0x7C => Some(AppShortcut::BrowserForward),            // Right
        0x2C => Some(AppShortcut::ToggleHelp),                // /
        0x21 => Some(AppShortcut::FocusPane(FocusDir::Prev)), // [
        0x1E => Some(AppShortcut::FocusPane(FocusDir::Next)), // ]
        0x03 => Some(AppShortcut::Find),                      // F
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::escape_js_string;

    #[test]
    fn test_escape_js_string_and_find_shortcut() {
        assert_eq!(escape_js_string(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_js_string("line1\nline2"), r"line1\nline2");
        #[cfg(target_os = "macos")]
        assert_eq!(
            match_macos_keycode_shortcut(0x03, false, false),
            Some(AppShortcut::Find)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_match_webview_edit_action() {
        assert_eq!(
            match_webview_edit_action(0x08, false, false),
            Some(WebViewEditAction::Copy)
        );
        assert_eq!(
            match_webview_edit_action(0x09, false, false),
            Some(WebViewEditAction::Paste)
        );
        assert_eq!(
            match_webview_edit_action(0x09, true, false),
            Some(WebViewEditAction::PasteAsPlainText)
        );
        assert_eq!(
            match_webview_edit_action(0x07, false, false),
            Some(WebViewEditAction::Cut)
        );
        assert_eq!(
            match_webview_edit_action(0x00, false, false),
            Some(WebViewEditAction::SelectAll)
        );
        assert_eq!(
            match_webview_edit_action(0x06, false, false),
            Some(WebViewEditAction::Undo)
        );
        assert_eq!(
            match_webview_edit_action(0x06, true, false),
            Some(WebViewEditAction::Redo)
        );
        assert_eq!(match_webview_edit_action(0x08, false, true), None);
    }
}
