use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSCursor, NSDragOperation,
    NSDraggingDestination, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSResponder, NSScreen,
    NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSObject, NSRange, NSString,
};
use raw_window_handle::{
    AppKitDisplayHandle, AppKitWindowHandle, DisplayHandle, HandleError, HasDisplayHandle,
    HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};

use crate::key_map::{AppShortcut, match_app_shortcut};
use crate::layout::Rect;
use crate::macos::{
    AppCursorKind, handle_perform_drag, notify_drag_hover, register_dragged_types,
    subtract_cursor_rects,
};
use crate::platform::types::{
    ElementState, Ime, Key, KeyCode, KeyEvent, LogicalPosition, LogicalSize, ModifiersState,
    MouseButton, MouseScrollDelta, NamedKey, PhysicalKey, PhysicalPosition, PhysicalSize,
    WindowEvent, fallback_unshifted_char, macos_keycode_to_physical, physical_to_named_key,
};
use crate::platform::url_field::NativeUrlField;
use crate::platform::{AppProxy, dispatch_window_event};
use crate::terminal_pane::AppEvent;

/// ウィンドウ生成時の初期属性
pub struct WindowAttributes {
    pub title: String,
    pub inner_size: LogicalSize<f64>,
    pub position: Option<LogicalPosition<i32>>,
}

impl Default for WindowAttributes {
    fn default() -> Self {
        Self {
            title: "TenT".to_string(),
            inner_size: LogicalSize::new(1440.0, 880.0),
            position: None,
        }
    }
}

// `TentMetalView` と `WKWebView` / `NativeUrlField` を兄弟配置するための名前付きコンテナビュー
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    pub struct TentRootView;

    unsafe impl NSObjectProtocol for TentRootView {}
);

impl TentRootView {
    pub fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

pub struct TentMetalViewIvars {
    proxy: AppProxy,
    marked_text: RefCell<String>,
    ime_rect_phys: Cell<(i32, i32, u32, u32)>,
    in_key_down: Cell<bool>,
    ime_consumed_keydown: Cell<bool>,
    command_by_selector: Cell<bool>,
    cursor_kind: AtomicU8,
    tracking_area: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TentMetalViewIvars]
    pub struct TentMetalView;

    unsafe impl NSObjectProtocol for TentMetalView {}

    impl TentMetalView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> Bool {
            Bool::YES
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> Bool {
            Bool::NO
        }

        #[unsafe(method(copy:))]
        fn menu_copy(&self, _sender: Option<&AnyObject>) {
            let _ = self
                .ivars()
                .proxy
                .send_event(AppEvent::Shortcut(AppShortcut::Copy));
        }

        #[unsafe(method(paste:))]
        fn menu_paste(&self, _sender: Option<&AnyObject>) {
            let _ = self
                .ivars()
                .proxy
                .send_event(AppEvent::Shortcut(AppShortcut::Paste));
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            unsafe {
                let _: () = msg_send![super(self), updateTrackingAreas];
            }
            if let Some(old) = self.ivars().tracking_area.borrow_mut().take() {
                self.removeTrackingArea(&old);
            }
            let opts = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    self.mtm().alloc(),
                    self.bounds(),
                    opts,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            *self.ivars().tracking_area.borrow_mut() = Some(area);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let bounds = self.bounds();
            let mut excluded = Vec::new();
            // 親コンテナ（TentRootView）上の兄弟ビュー（表示中の WKWebView 等）領域を除外する
            if let Some(superview) = unsafe { self.superview() } {
                let self_ptr: *const NSView = (self as *const Self).cast();
                for sub in superview.subviews().iter() {
                    let sub_ptr: *const NSView = &*sub;
                    if std::ptr::eq(sub_ptr, self_ptr) || sub.isHidden() {
                        continue;
                    }
                    let frame = sub.frame();
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

            #[allow(deprecated)]
            let cursor = match self.ivars().cursor_kind.load(Ordering::Relaxed) {
                1 => NSCursor::pointingHandCursor(),
                2 => NSCursor::resizeLeftRightCursor(),
                3 => NSCursor::resizeUpDownCursor(),
                _ => NSCursor::arrowCursor(),
            };

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
                self.addCursorRect_cursor(rect, &cursor);
            }
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            // 自分が First Responder（ターミナル / 検索バー）のときだけ横取りする。
            // WKWebView / NSTextField が First Responder のときは NO を返し、AppKit 標準の Edit メニュー等へ流す。
            let is_first = self
                .window()
                .and_then(|w| w.firstResponder())
                .is_some_and(|fr| {
                    let fr_ptr: *const AnyObject = (&*fr as *const NSResponder).cast();
                    let self_ptr: *const AnyObject = (self as *const Self).cast();
                    std::ptr::eq(fr_ptr, self_ptr)
                });
            if !is_first {
                return Bool::NO;
            }

            let mods = ModifiersState::from_macos_flags(event.modifierFlags().0);
            if !mods.super_key() {
                return Bool::NO;
            }

            let key_event = nsevent_to_key_event(event, ElementState::Pressed);

            // TenT のアプリショートカット、および検索バーで使う Cmd+A / Cmd+G のみを捕捉し、
            // Cmd+Q / Cmd+H / Ctrl+Cmd+Space（絵文字ピッカー）等は macOS メニューへそのまま流す。
            let is_search_cmd = !mods.control_key()
                && !mods.alt_key()
                && matches!(
                    &key_event.logical_key,
                    Key::Character(c) if c.eq_ignore_ascii_case("a") || c.eq_ignore_ascii_case("g")
                );
            if match_app_shortcut(&key_event, mods).is_none() && !is_search_cmd {
                return Bool::NO;
            }

            dispatch_window_event(WindowEvent::KeyboardInput { event: key_event });
            Bool::YES
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let mods = ModifiersState::from_macos_flags(event.modifierFlags().0);
            let key_event = nsevent_to_key_event(event, ElementState::Pressed);
            let had_marked = !self.ivars().marked_text.borrow().is_empty();

            // IME 変換中でないときの Cmd / Ctrl / Option 併用キーは、macOS の Option 記号合成（ƒ 等）を
            // 避けてそのまま ghostty-vt / ショートカット判定へ渡す
            if !had_marked && (mods.super_key() || mods.control_key() || mods.alt_key()) {
                dispatch_window_event(WindowEvent::KeyboardInput { event: key_event });
                return;
            }

            self.ivars().in_key_down.set(true);
            self.ivars().ime_consumed_keydown.set(false);
            self.ivars().command_by_selector.set(false);

            let events = NSArray::from_slice(&[event]);
            self.interpretKeyEvents(&events);

            self.ivars().in_key_down.set(false);

            let has_marked_now = !self.ivars().marked_text.borrow().is_empty();
            let forward_command = self.ivars().command_by_selector.get() && !has_marked_now;
            if forward_command
                || (!self.ivars().ime_consumed_keydown.get() && !had_marked && !has_marked_now)
            {
                dispatch_window_event(WindowEvent::KeyboardInput { event: key_event });
            }
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let key_event = nsevent_to_key_event(event, ElementState::Released);
            dispatch_window_event(WindowEvent::KeyboardInput { event: key_event });
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let raw_flags = event.modifierFlags().0;
            let mods = ModifiersState::from_macos_flags(raw_flags);
            dispatch_window_event(WindowEvent::ModifiersChanged(mods));

            // macOS device-dependent ビットで左右の修飾キーを正確に区別して押下/解放を送出する。
            // 合成イベント等で device-dependent ビットがすべて 0 の場合は汎用フラグへフォールバックする。
            let physical_key = macos_keycode_to_physical(event.keyCode());
            if let PhysicalKey::Code(code) = physical_key {
                if code == KeyCode::CapsLock {
                    // macOS の Caps Lock はトグル通知のみで押下/解放の組が来ないため、
                    // Kitty プロトコルで押しっぱなし扱いにならないよう Pressed → Released を続けて送出する
                    for state in [ElementState::Pressed, ElementState::Released] {
                        dispatch_window_event(WindowEvent::KeyboardInput {
                            event: KeyEvent {
                                physical_key,
                                logical_key: Key::Unidentified,
                                unshifted_char: None,
                                state,
                                repeat: false,
                                text: None,
                            },
                        });
                    }
                    return;
                }

                const DEVICE_DEPENDENT_ALL: usize = 0x207f;
                let has_device_bits = (raw_flags & DEVICE_DEPENDENT_ALL) != 0;
                let pressed = if has_device_bits {
                    let mask: usize = match code {
                        KeyCode::ControlLeft => 0x0001,
                        KeyCode::ShiftLeft => 0x0002,
                        KeyCode::ShiftRight => 0x0004,
                        KeyCode::SuperLeft => 0x0008,
                        KeyCode::SuperRight => 0x0010,
                        KeyCode::AltLeft => 0x0020,
                        KeyCode::AltRight => 0x0040,
                        KeyCode::ControlRight => 0x2000,
                        _ => return,
                    };
                    (raw_flags & mask) != 0
                } else {
                    match code {
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => mods.shift_key(),
                        KeyCode::ControlLeft | KeyCode::ControlRight => mods.control_key(),
                        KeyCode::AltLeft | KeyCode::AltRight => mods.alt_key(),
                        KeyCode::SuperLeft | KeyCode::SuperRight => mods.super_key(),
                        _ => return,
                    }
                };
                let state = if pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                };
                dispatch_window_event(WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        physical_key,
                        logical_key: Key::Unidentified,
                        unshifted_char: None,
                        state,
                        repeat: false,
                        text: None,
                    },
                });
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.emit_cursor_moved(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.emit_cursor_moved(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            // ターミナル領域クリック時は確実に自分を First Responder へ戻す
            if let Some(win) = self.window() {
                let _ = win.makeFirstResponder(Some(self));
            }
            self.emit_cursor_moved(event);
            dispatch_window_event(WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.emit_cursor_moved(event);
            dispatch_window_event(WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
            });
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.emit_cursor_moved(event);
            dispatch_window_event(WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
            });
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.emit_cursor_moved(event);
            dispatch_window_event(WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
            });
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            self.emit_cursor_moved(event);
            let dx = event.scrollingDeltaX();
            let dy = event.scrollingDeltaY();
            let delta = if event.hasPreciseScrollingDeltas() {
                let scale = self
                    .window()
                    .map(|w| w.backingScaleFactor())
                    .unwrap_or(1.0)
                    .max(1.0);
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(dx * scale, dy * scale))
            } else {
                MouseScrollDelta::LineDelta(dx as f32, dy as f32)
            };
            dispatch_window_event(WindowEvent::MouseWheel { delta });
        }
    }

    unsafe impl NSDraggingDestination for TentMetalView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            let obj: &AnyObject = unsafe { &*(sender as *const _ as *const AnyObject) };
            notify_drag_hover(obj, &self.ivars().proxy);
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            let obj: &AnyObject = unsafe { &*(sender as *const _ as *const AnyObject) };
            notify_drag_hover(obj, &self.ivars().proxy);
            NSDragOperation::Copy
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(
            &self,
            _sender: &ProtocolObject<dyn NSDraggingInfo>,
        ) -> Bool {
            Bool::YES
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> Bool {
            let obj: &AnyObject = unsafe { &*(sender as *const _ as *const AnyObject) };
            handle_perform_drag(obj, &self.ivars().proxy)
        }
    }

    unsafe impl NSTextInputClient for TentMetalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(
            &self,
            string: &AnyObject,
            _replacement_range: NSRange,
        ) {
            let text = extract_text_from_any(string);
            let was_marked = !self.ivars().marked_text.borrow().is_empty();
            if was_marked {
                self.ivars().marked_text.borrow_mut().clear();
            }

            let in_kd = self.ivars().in_key_down.get();
            // 変換確定（marked あり）または keyDown 外からの挿入（絵文字ピッカー等）のみ Ime::Commit を使う。
            // 変換中でない通常キー入力は KeyboardInput 経路へ通し、Kitty プロトコル・JIS ¥・選択解除と一貫させる。
            if was_marked || !in_kd {
                self.ivars().ime_consumed_keydown.set(true);
                if !text.is_empty() {
                    dispatch_window_event(WindowEvent::Ime(Ime::Commit(text)));
                } else if was_marked {
                    dispatch_window_event(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                }
            }
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, _selector: Sel) {
            // 制御キー・矢印・Enter・Backspace 等は keyDown 側の KeyboardInput へそのまま流す
            self.ivars().command_by_selector.set(true);
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            _selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            let text = extract_text_from_any(string);
            *self.ivars().marked_text.borrow_mut() = text.clone();
            self.ivars().ime_consumed_keydown.set(true);
            dispatch_window_event(WindowEvent::Ime(Ime::Preedit(text, None)));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            if !self.ivars().marked_text.borrow().is_empty() {
                self.ivars().marked_text.borrow_mut().clear();
                dispatch_window_event(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
            }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(0, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let len = self.ivars().marked_text.borrow().encode_utf16().count();
            if len == 0 {
                NSRange::new(objc2_foundation::NSNotFound as usize, 0)
            } else {
                NSRange::new(0, len)
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> Bool {
            Bool::new(!self.ivars().marked_text.borrow().is_empty())
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring_for_proposed_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSString>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> CGRect {
            let Some(win) = self.window() else {
                return CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
            };
            let scale = win.backingScaleFactor().max(1.0);
            let bounds = self.bounds();
            let (px, py, pw, ph) = self.ivars().ime_rect_phys.get();
            let lx = f64::from(px) / scale;
            let lw = (f64::from(pw) / scale).max(1.0);
            let lh = (f64::from(ph) / scale).max(1.0);
            let ly = bounds.size.height - (f64::from(py) / scale) - lh;
            let view_rect = CGRect::new(CGPoint::new(lx, ly), CGSize::new(lw, lh));
            let win_rect = self.convertRect_toView(view_rect, None);
            win.convertRectToScreen(win_rect)
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: CGPoint) -> usize {
            0
        }
    }
);

impl TentMetalView {
    pub fn new(mtm: MainThreadMarker, frame: CGRect, proxy: AppProxy) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(TentMetalViewIvars {
            proxy,
            marked_text: RefCell::new(String::new()),
            ime_rect_phys: Cell::new((0, 0, 1, 1)),
            in_key_down: Cell::new(false),
            ime_consumed_keydown: Cell::new(false),
            command_by_selector: Cell::new(false),
            cursor_kind: AtomicU8::new(AppCursorKind::Default as u8),
            tracking_area: RefCell::new(None),
        });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let view_obj: &AnyObject = unsafe { &*(&*view as *const Self).cast() };
        register_dragged_types(view_obj);
        view
    }

    fn emit_cursor_moved(&self, event: &NSEvent) {
        let Some(win) = self.window() else {
            return;
        };
        let scale = win.backingScaleFactor().max(1.0);
        let loc = self.convertPoint_fromView(event.locationInWindow(), None);
        let bounds = self.bounds();
        let px = loc.x * scale;
        let py = (bounds.size.height - loc.y) * scale;
        dispatch_window_event(WindowEvent::CursorMoved {
            position: PhysicalPosition::new(px, py),
        });
    }

    pub fn set_ime_cursor_area(&self, pos: PhysicalPosition<i32>, size: PhysicalSize<u32>) {
        self.ivars()
            .ime_rect_phys
            .set((pos.x, pos.y, size.width, size.height));
        unsafe {
            let ctx: *mut AnyObject = msg_send![self, inputContext];
            if !ctx.is_null() {
                let _: () = msg_send![ctx, invalidateCharacterCoordinates];
            }
        }
    }

    pub fn set_cursor_kind(&self, kind: AppCursorKind) {
        self.ivars()
            .cursor_kind
            .store(kind as u8, Ordering::Relaxed);
        if let Some(win) = self.window() {
            win.invalidateCursorRectsForView(self);
        }
    }
}

fn extract_text_from_any(obj: &AnyObject) -> String {
    unsafe {
        if let Some(attr_cls) = AnyClass::get(c"NSAttributedString") {
            let is_attr: Bool = msg_send![obj, isKindOfClass: attr_cls];
            if is_attr.as_bool() {
                let s: Option<Retained<NSString>> = msg_send![obj, string];
                return s.map(|ns| ns.to_string()).unwrap_or_default();
            }
        }
        if let Some(str_cls) = AnyClass::get(c"NSString") {
            let is_str: Bool = msg_send![obj, isKindOfClass: str_cls];
            if is_str.as_bool() {
                let ns: &NSString = &*(obj as *const AnyObject).cast::<NSString>();
                return ns.to_string();
            }
        }
    }
    String::new()
}

fn nsevent_to_key_event(event: &NSEvent, state: ElementState) -> KeyEvent {
    let keycode = event.keyCode();
    let physical_key = macos_keycode_to_physical(keycode);
    let repeat = if state == ElementState::Pressed {
        event.isARepeat()
    } else {
        false
    };

    let chars = event.characters().map(|s| s.to_string());
    let chars_ignoring = event.charactersIgnoringModifiers().map(|s| s.to_string());

    let logical_key = if let PhysicalKey::Code(code) = physical_key
        && let Some(named) = physical_to_named_key(code)
    {
        Key::Named(named)
    } else if let Some(ref s) = chars_ignoring
        && !s.is_empty()
    {
        Key::Character(s.clone())
    } else {
        Key::Unidentified
    };

    // macOS 10.15+ の charactersByApplyingModifiers(empty) で現在のキーボード配列（JIS/US等）における
    // Shift/Option/Ctrl/CapsLock 無効化後の基底文字を取得する
    let unshifted_char = event
        .charactersByApplyingModifiers(NSEventModifierFlags::empty())
        .map(|s| s.to_string())
        .as_deref()
        .and_then(|s| s.chars().next())
        .filter(|&c| {
            let cp = c as u32;
            cp >= 0x20 && cp != 0x7f && !(0xF700..=0xF8FF).contains(&cp)
        })
        .or_else(|| match physical_key {
            PhysicalKey::Code(code) => fallback_unshifted_char(code),
            PhysicalKey::Unidentified => None,
        });

    let text = match &logical_key {
        Key::Named(n) if *n != NamedKey::Space => None,
        _ => chars.and_then(|s| {
            let cleaned: String = s
                .chars()
                .filter(|&c| {
                    let cp = c as u32;
                    cp >= 0x20 && cp != 0x7f && !(0xF700..=0xF8FF).contains(&cp)
                })
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        }),
    };

    KeyEvent {
        physical_key,
        logical_key,
        unshifted_char,
        state,
        repeat,
        text,
    }
}

/// `wgpu::Instance::create_surface` へ渡す `Send + Sync` なサーフェスハンドル。
/// `NativeWindow` 本体ではなく raw view handle のみを `Send + Sync` に閉じ込める。
pub struct SurfaceTarget {
    metal_view: NonNull<std::ffi::c_void>,
}

unsafe impl Send for SurfaceTarget {}
unsafe impl Sync for SurfaceTarget {}

impl HasWindowHandle for SurfaceTarget {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = AppKitWindowHandle::new(self.metal_view);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(handle)) })
    }
}

impl HasDisplayHandle for SurfaceTarget {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::AppKit(AppKitDisplayHandle::new()))
        })
    }
}

/// `wry::WebViewBuilder::build_as_child` へ渡すコンテナビューハンドル。
/// `TentMetalView` ではなくプレーンな `TentRootView` を返すことで、
/// `WKWebView` を `TentMetalView` の「子」ではなく「兄弟」として配置する。
pub struct BrowserContainerHandle {
    root_view: NonNull<std::ffi::c_void>,
}

impl HasWindowHandle for BrowserContainerHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = AppKitWindowHandle::new(self.root_view);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(handle)) })
    }
}

/// TenT の macOS ネイティブウィンドウ（メインスレッド専用: `!Send`, `!Sync`）
pub struct NativeWindow {
    mtm: MainThreadMarker,
    ns_window: Retained<NSWindow>,
    root_view: Retained<TentRootView>,
    metal_view: Retained<TentMetalView>,
    url_field: NativeUrlField,
    redraw_queued: AtomicBool,
}

impl NativeWindow {
    pub fn new(
        mtm: MainThreadMarker,
        attrs: WindowAttributes,
        delegate: &NSObject,
        proxy: AppProxy,
    ) -> Self {
        let content_rect = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(attrs.inner_size.width, attrs.inner_size.height),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;

        let ns_window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content_rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        ns_window.setTitle(&NSString::from_str(&attrs.title));
        ns_window.setAcceptsMouseMovedEvents(true);
        unsafe {
            ns_window.setReleasedWhenClosed(false);
            let _: () = msg_send![&ns_window, setDelegate: delegate];
        }

        // 名前付きコンテナ TentRootView の下に TentMetalView・NativeUrlField・WKWebView を兄弟配置する
        let root_view = TentRootView::new(mtm, content_rect);
        let metal_view = TentMetalView::new(mtm, content_rect, proxy.clone());
        root_view.addSubview(&metal_view);
        let url_field = NativeUrlField::new(mtm, &root_view, proxy);
        ns_window.setContentView(Some(&root_view));
        let _ = ns_window.makeFirstResponder(Some(&metal_view));

        if let Some(pos) = attrs.position {
            let primary_h = primary_screen_height(mtm);
            let frame_rect = ns_window.frameRectForContentRect(content_rect);
            let origin_y = primary_h - f64::from(pos.y) - frame_rect.size.height;
            ns_window.setFrameOrigin(CGPoint::new(f64::from(pos.x), origin_y));
        } else {
            ns_window.center();
        }

        ns_window.makeKeyAndOrderFront(None);

        Self {
            mtm,
            ns_window,
            root_view,
            metal_view,
            url_field,
            redraw_queued: AtomicBool::new(false),
        }
    }

    pub fn surface_target(&self) -> SurfaceTarget {
        let ptr = NonNull::new(
            (&*self.metal_view as *const TentMetalView)
                .cast_mut()
                .cast(),
        )
        .expect("metal_view pointer is non-null");
        SurfaceTarget { metal_view: ptr }
    }

    pub fn scale_factor(&self) -> f64 {
        self.ns_window.backingScaleFactor().max(1.0)
    }

    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let bounds = self.root_view.bounds();
        let scale = self.scale_factor();
        PhysicalSize::new(
            (bounds.size.width * scale).round().max(0.0) as u32,
            (bounds.size.height * scale).round().max(0.0) as u32,
        )
    }

    pub fn outer_position(&self) -> Option<PhysicalPosition<i32>> {
        let primary_h = primary_screen_height(self.mtm);
        let frame = self.ns_window.frame();
        let scale = self.scale_factor();
        let top_left_y = primary_h - (frame.origin.y + frame.size.height);
        Some(PhysicalPosition::new(
            (frame.origin.x * scale).round() as i32,
            (top_left_y * scale).round() as i32,
        ))
    }

    /// `wry::WebViewBuilder::build_as_child` 用のコンテナハンドル（`TentRootView`）
    pub fn browser_container_handle(&self) -> BrowserContainerHandle {
        let ptr = NonNull::new((&*self.root_view as *const TentRootView).cast_mut().cast())
            .expect("root_view pointer is non-null");
        BrowserContainerHandle { root_view: ptr }
    }

    /// ターミナル側の `TentMetalView` へ First Responder を戻す
    pub fn focus_terminal(&self) {
        let _ = self.ns_window.makeFirstResponder(Some(&self.metal_view));
    }

    pub fn show_url_bar(&self, phys_rect: Rect, text: &str) {
        self.url_field.show(
            &self.ns_window,
            &self.root_view,
            phys_rect,
            self.scale_factor(),
            text,
        );
    }

    pub fn reposition_url_bar(&self, phys_rect: Rect) {
        self.url_field
            .reposition(&self.root_view, phys_rect, self.scale_factor());
    }

    pub fn hide_url_bar(&self) {
        self.url_field.hide();
    }

    pub fn set_ime_cursor_area(&self, pos: PhysicalPosition<i32>, size: PhysicalSize<u32>) {
        self.metal_view.set_ime_cursor_area(pos, size);
    }

    pub fn set_cursor_kind(&self, kind: AppCursorKind) {
        self.metal_view.set_cursor_kind(kind);
    }

    pub fn invalidate_cursor_rects(&self) {
        self.ns_window
            .invalidateCursorRectsForView(&self.metal_view);
    }

    pub fn request_redraw(&self) {
        if self
            .redraw_queued
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            crate::platform::schedule_redraw_on_main();
        }
    }

    pub(crate) fn take_redraw_flag(&self) -> bool {
        self.redraw_queued.swap(false, Ordering::AcqRel)
    }
}

fn primary_screen_height(mtm: MainThreadMarker) -> f64 {
    let screens = NSScreen::screens(mtm);
    if !screens.is_empty() {
        screens.objectAtIndex(0).frame().size.height
    } else {
        NSScreen::mainScreen(mtm)
            .map(|s| s.frame().size.height)
            .unwrap_or(900.0)
    }
}
