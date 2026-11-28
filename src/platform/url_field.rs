use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControl, NSFocusRingType, NSFont, NSTextField, NSTextView, NSView, NSWindow,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSString};

use crate::layout::Rect;
use crate::platform::AppProxy;
use crate::terminal_pane::AppEvent;

static ACTIVE_URL_FIELD_PTR: AtomicUsize = AtomicUsize::new(0);

/// 指定オブジェクトが TenT のネイティブ URL バー（`NSTextField` またはその子孫 / Field Editor）かをポインタで判定する
pub fn is_url_field_or_editor(win: &NSWindow, obj: &AnyObject) -> bool {
    let field_addr = ACTIVE_URL_FIELD_PTR.load(Ordering::Relaxed);
    if field_addr == 0 {
        return false;
    }
    let field_ptr = field_addr as *const AnyObject;
    let obj_ptr: *const AnyObject = obj;
    if std::ptr::eq(obj_ptr, field_ptr) {
        return true;
    }
    unsafe {
        let editor: *mut AnyObject = msg_send![win, fieldEditor: Bool::NO, forObject: field_ptr];
        if !editor.is_null() && std::ptr::eq(obj_ptr, editor) {
            return true;
        }
        // NSWindow 等の NSView 以外のレスポンダへ isDescendantOf: を送らないようクラスを確認する
        if let Some(view_cls) = objc2::runtime::AnyClass::get(c"NSView") {
            let is_view: Bool = msg_send![obj_ptr, isKindOfClass: view_cls];
            if is_view.as_bool() {
                let is_descendant: Bool = msg_send![obj_ptr, isDescendantOf: field_ptr];
                return is_descendant.as_bool();
            }
        }
        false
    }
}

struct TentUrlFieldDelegateIvars {
    proxy: AppProxy,
    suppress_end_editing: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TentUrlFieldDelegateIvars]
    struct TentUrlFieldDelegate;

    unsafe impl NSObjectProtocol for TentUrlFieldDelegate {}

    impl TentUrlFieldDelegate {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn control_text_view_do_command(
            &self,
            control: &NSControl,
            _text_view: &NSTextView,
            command_selector: Sel,
        ) -> Bool {
            if command_selector == sel!(insertNewline:) {
                self.ivars().suppress_end_editing.set(true);
                let text = control.stringValue().to_string();
                let _ = self.ivars().proxy.send_event(AppEvent::UrlBarSubmit(text));
                return Bool::YES;
            }
            if command_selector == sel!(cancelOperation:) {
                self.ivars().suppress_end_editing.set(true);
                let _ = self.ivars().proxy.send_event(AppEvent::UrlBarCancel);
                return Bool::YES;
            }
            Bool::NO
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _notification: &NSNotification) {
            if !self.ivars().suppress_end_editing.replace(false) {
                let _ = self.ivars().proxy.send_event(AppEvent::UrlBarCancel);
            }
        }
    }
);

impl TentUrlFieldDelegate {
    fn new(mtm: MainThreadMarker, proxy: AppProxy) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(TentUrlFieldDelegateIvars {
            proxy,
            suppress_end_editing: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// macOS ネイティブ `NSTextField` による URL バー編集コントロール
pub(crate) struct NativeUrlField {
    field: Retained<NSTextField>,
    delegate: Retained<TentUrlFieldDelegate>,
}

impl NativeUrlField {
    pub(crate) fn new(mtm: MainThreadMarker, root_view: &NSView, proxy: AppProxy) -> Self {
        let delegate = TentUrlFieldDelegate::new(mtm, proxy);
        let field = NSTextField::initWithFrame(
            mtm.alloc(),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(100.0, 20.0)),
        );
        field.setBordered(false);
        field.setBezeled(false);
        field.setDrawsBackground(true);
        field.setEditable(true);
        field.setSelectable(true);
        field.setFocusRingType(NSFocusRingType::None);
        field.setUsesSingleLineMode(true);

        let bg = NSColor::colorWithSRGBRed_green_blue_alpha(0.07, 0.09, 0.13, 1.0);
        field.setBackgroundColor(Some(&bg));
        field.setTextColor(Some(&NSColor::whiteColor()));

        let font = NSFont::fontWithName_size(&NSString::from_str("Menlo"), 11.5)
            .unwrap_or_else(|| NSFont::systemFontOfSize(11.5));
        field.setFont(Some(&font));

        if let Some(cell) = field.cell() {
            cell.setScrollable(true);
            cell.setWraps(false);
        }

        unsafe {
            let _: () = msg_send![&field, setDelegate: &*delegate];
        }
        field.setHidden(true);
        root_view.addSubview(&field);
        ACTIVE_URL_FIELD_PTR.store((&*field as *const NSTextField) as usize, Ordering::Relaxed);

        Self { field, delegate }
    }

    fn compute_frame(root_view: &NSView, phys_rect: Rect, scale: f64) -> CGRect {
        let s = scale.max(1.0);
        let root_h = root_view.bounds().size.height;
        let pad_x = 5.0;
        let pad_y = 2.0;
        let lx = (f64::from(phys_rect.x) / s) + pad_x;
        let lw = ((f64::from(phys_rect.w) / s) - pad_x * 2.0).max(20.0);
        let lh = ((f64::from(phys_rect.h) / s) - pad_y * 2.0).max(16.0);
        let top_y = (f64::from(phys_rect.y) / s) + pad_y;
        let ly = root_h - top_y - lh;
        CGRect::new(CGPoint::new(lx, ly), CGSize::new(lw, lh))
    }

    pub(crate) fn show(
        &self,
        ns_window: &NSWindow,
        root_view: &NSView,
        phys_rect: Rect,
        scale: f64,
        text: &str,
    ) {
        self.delegate.ivars().suppress_end_editing.set(true);
        let frame = Self::compute_frame(root_view, phys_rect, scale);
        self.field.setFrame(frame);
        self.field.setStringValue(&NSString::from_str(text));
        self.field.setHidden(false);
        // 兄弟ビュー（WKWebView 等）より確実に前面へ出す
        root_view.addSubview(&self.field);
        self.delegate.ivars().suppress_end_editing.set(false);
        let _ = ns_window.makeFirstResponder(Some(&self.field));
        // Field Editor の挿入カーソル色を白にしてダーク背景で視認性を確保する
        unsafe {
            let editor: *mut AnyObject =
                msg_send![ns_window, fieldEditor: Bool::NO, forObject: &*self.field];
            if !editor.is_null() {
                let white = NSColor::whiteColor();
                let _: () = msg_send![editor, setInsertionPointColor: &*white];
            }
        }
    }

    pub(crate) fn reposition(&self, root_view: &NSView, phys_rect: Rect, scale: f64) {
        if !self.field.isHidden() {
            let frame = Self::compute_frame(root_view, phys_rect, scale);
            self.field.setFrame(frame);
        }
    }

    pub(crate) fn hide(&self) {
        if !self.field.isHidden() {
            self.delegate.ivars().suppress_end_editing.set(true);
            self.field.setHidden(true);
        }
    }
}
