#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use super::clipboard::extract_pasteboard_image_to_temp;

/// macOS の `WinitView` にファイル／スクショ画像 D&D ハンドラを登録する。
/// `winit` 標準は非推奨の `NSFilenamesPboardType` のみ対応のため、
/// `public.file-url` およびスクショ右下サムネイル等の `public.png` / `public.tiff` も受け取れるようにする。
#[cfg(target_os = "macos")]
pub fn install_file_drop_handler(
    window: &winit::window::Window,
    proxy: winit::event_loop::EventLoopProxy<crate::terminal_pane::AppEvent>,
) {
    use std::ffi::c_char;
    use std::ptr::NonNull;
    use std::sync::OnceLock;
    use std::time::{SystemTime, UNIX_EPOCH};

    use block2::RcBlock;
    use objc2::ffi::{class_replaceMethod, object_getClass};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_core_foundation::{CGPoint, CGRect};
    use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    static DROP_PROXY: OnceLock<winit::event_loop::EventLoopProxy<crate::terminal_pane::AppEvent>> =
        OnceLock::new();
    let _ = DROP_PROXY.set(proxy);

    unsafe fn drag_location_px(sender: *mut AnyObject) -> Option<(f32, f32)> {
        if sender.is_null() {
            return None;
        }
        let win: *mut AnyObject = unsafe { msg_send![sender, draggingDestinationWindow] };
        if win.is_null() {
            return None;
        }
        let content: *mut AnyObject = unsafe { msg_send![win, contentView] };
        if content.is_null() {
            return None;
        }
        let loc: CGPoint = unsafe { msg_send![sender, draggingLocation] };
        let frame: CGRect = unsafe { msg_send![content, frame] };
        let scale: f64 = unsafe { msg_send![win, backingScaleFactor] };
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let px = (loc.x * scale) as f32;
        let py = ((frame.size.height - loc.y) * scale) as f32;
        Some((px, py))
    }

    fn notify_drag_hover(sender: *mut AnyObject) {
        if let Some((px, py)) = unsafe { drag_location_px(sender) }
            && let Some(proxy) = DROP_PROXY.get()
        {
            let _ = proxy.send_event(crate::terminal_pane::AppEvent::DragHover { px, py });
        }
    }

    /// macOS スクショ右下サムネイルの一時領域（`TemporaryItems/NSIRD_screencaptureui_*`）は
    /// サムネイル消滅後に削除されたりサンドボックスから読めないため、`/tmp/tent-drops/` へ退避する。
    fn materialize_ephemeral_drop(path: PathBuf) -> PathBuf {
        let s = path.to_string_lossy();
        if !s.contains("TemporaryItems/NSIRD_screencaptureui_") {
            return path;
        }
        let drop_dir = PathBuf::from("/tmp/tent-drops");
        if std::fs::create_dir_all(&drop_dir).is_err() {
            return path;
        }
        let file_name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| {
                let ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                std::ffi::OsString::from(format!("screenshot-{ms}.png"))
            });
        let dest = drop_dir.join(file_name);
        if std::fs::copy(&path, &dest).is_ok() {
            dest
        } else {
            path
        }
    }

    unsafe extern "C" fn dragging_entered(
        _this: *mut AnyObject,
        _cmd: Sel,
        sender: *mut AnyObject,
    ) -> usize {
        notify_drag_hover(sender);
        1 // NSDragOperationCopy
    }

    unsafe extern "C" fn dragging_updated(
        _this: *mut AnyObject,
        _cmd: Sel,
        sender: *mut AnyObject,
    ) -> usize {
        notify_drag_hover(sender);
        1 // NSDragOperationCopy
    }

    unsafe extern "C" fn prepare_for_drag(
        _this: *mut AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) -> Bool {
        Bool::YES
    }

    unsafe extern "C" fn perform_drag(
        _this: *mut AnyObject,
        _cmd: Sel,
        sender: *mut AnyObject,
    ) -> Bool {
        if sender.is_null() {
            return Bool::NO;
        }
        let pb: *mut AnyObject = unsafe { msg_send![sender, draggingPasteboard] };
        if pb.is_null() {
            return Bool::NO;
        }

        let drop_pos = unsafe { drag_location_px(sender) };
        let mut paths: Vec<PathBuf> = Vec::new();

        // 1. Ghostty と同じ方式: item.propertyListForType("public.file-url") -> NSURL(pasteboardPropertyList:ofType:)
        let file_url_type = NSString::from_str("public.file-url");
        let items: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![pb, pasteboardItems] };
        if let Some(items) = items {
            let nsurl_cls = AnyClass::get(c"NSURL");
            for item in items.iter() {
                let mut resolved_url: Option<Retained<NSURL>> = None;
                if let Some(cls) = nsurl_cls {
                    let plist: Option<Retained<AnyObject>> =
                        unsafe { msg_send![&*item, propertyListForType: &*file_url_type] };
                    if let Some(plist) = plist {
                        let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
                        if !alloc.is_null() {
                            let raw: *mut NSURL = unsafe {
                                msg_send![
                                    alloc,
                                    initWithPasteboardPropertyList: &*plist,
                                    ofType: &*file_url_type
                                ]
                            };
                            resolved_url = unsafe { Retained::from_raw(raw) };
                        }
                    }
                }
                if resolved_url.is_none() {
                    let url_str: Option<Retained<NSString>> =
                        unsafe { msg_send![&*item, stringForType: &*file_url_type] };
                    if let Some(url_str) = url_str {
                        resolved_url = NSURL::URLWithString(&url_str);
                    }
                }
                if let Some(nsurl) = resolved_url {
                    let resolved = nsurl.filePathURL().unwrap_or(nsurl);
                    if let Some(path_str) = resolved.path() {
                        let p = PathBuf::from(path_str.to_string());
                        if !p.as_os_str().is_empty() {
                            paths.push(p);
                        }
                    }
                }
            }
        }

        // 2. 旧 NSFilenamesPboardType フォールバック
        if paths.is_empty() {
            let legacy_type = NSString::from_str("NSFilenamesPboardType");
            let plist: Option<Retained<AnyObject>> =
                unsafe { msg_send![pb, propertyListForType: &*legacy_type] };
            if let Some(plist) = plist {
                let count: usize = unsafe { msg_send![&*plist, count] };
                for i in 0..count {
                    let s: Option<Retained<NSString>> =
                        unsafe { msg_send![&*plist, objectAtIndex: i] };
                    if let Some(s) = s {
                        let p = PathBuf::from(s.to_string());
                        if !p.as_os_str().is_empty() {
                            paths.push(p);
                        }
                    }
                }
            }
        }

        // 3. macOS スクショ直後の右下サムネイル等（NSFilePromiseReceiver）
        if paths.is_empty()
            && let Some(promise_cls) = AnyClass::get(c"NSFilePromiseReceiver")
            && let Some(queue_cls) = AnyClass::get(c"NSOperationQueue")
        {
            let cls_obj: &AnyObject = unsafe { &*(promise_cls as *const AnyClass).cast() };
            let classes = NSArray::from_slice(&[cls_obj]);
            let empty_opts: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
            let receivers: Option<Retained<NSArray<AnyObject>>> =
                unsafe { msg_send![pb, readObjectsForClasses: &*classes, options: &*empty_opts] };
            if let Some(receivers) = receivers
                && !receivers.is_empty()
            {
                let drop_dir = PathBuf::from("/tmp/tent-drops");
                let _ = std::fs::create_dir_all(&drop_dir);
                let dir_str = NSString::from_str(&drop_dir.to_string_lossy());
                let dest_url = NSURL::fileURLWithPath_isDirectory(&dir_str, true);
                // Apple 推奨: mainQueue ではなく単一のバックグラウンド NSOperationQueue を再利用（毎回リークさせない）
                static PROMISE_QUEUE: OnceLock<usize> = OnceLock::new();
                let queue_ptr = *PROMISE_QUEUE.get_or_init(|| {
                    let q: Option<Retained<AnyObject>> = unsafe { msg_send![queue_cls, new] };
                    q.map_or(0, |r| Retained::into_raw(r) as usize)
                });
                if queue_ptr != 0 {
                    let queue = queue_ptr as *const AnyObject;
                    for receiver in receivers.iter() {
                        let block =
                            RcBlock::new(move |file_url: NonNull<NSURL>, err: *mut AnyObject| {
                                if !err.is_null() {
                                    return;
                                }
                                let url_ref = unsafe { file_url.as_ref() };
                                let resolved = url_ref.filePathURL();
                                let path_opt = resolved
                                    .as_deref()
                                    .and_then(NSURL::path)
                                    .or_else(|| url_ref.path());
                                if let Some(path_str) = path_opt {
                                    let p = PathBuf::from(path_str.to_string());
                                    if !p.as_os_str().is_empty()
                                        && let Some(proxy) = DROP_PROXY.get()
                                    {
                                        let _ = proxy.send_event(
                                            crate::terminal_pane::AppEvent::DroppedFiles {
                                                paths: vec![p],
                                                pos: drop_pos,
                                            },
                                        );
                                    }
                                }
                            });
                        unsafe {
                            let _: () = msg_send![
                                &*receiver,
                                receivePromisedFilesAtDestination: &*dest_url,
                                options: &*empty_opts,
                                operationQueue: queue,
                                reader: &*block
                            ];
                        }
                    }
                    return Bool::YES;
                }
            }
        }

        // 4. スクショ直後の右下サムネイルや他アプリからの生画像（PNG / TIFF）
        if paths.is_empty()
            && let Some(path) = unsafe { extract_pasteboard_image_to_temp(pb, "tent-drop") }
        {
            paths.push(path);
        }

        if !paths.is_empty()
            && let Some(proxy) = DROP_PROXY.get()
        {
            let materialized: Vec<PathBuf> =
                paths.into_iter().map(materialize_ephemeral_drop).collect();
            let _ = proxy.send_event(crate::terminal_pane::AppEvent::DroppedFiles {
                paths: materialized,
                pos: drop_pos,
            });
            return Bool::YES;
        }
        Bool::NO
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
        let imp_enter: Imp = std::mem::transmute::<
            unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(dragging_entered);
        let imp_update: Imp = std::mem::transmute::<
            unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(dragging_updated);
        let imp_prep: Imp = std::mem::transmute::<
            unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool,
            Imp,
        >(prepare_for_drag);
        let imp_perf: Imp = std::mem::transmute::<
            unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool,
            Imp,
        >(perform_drag);

        // WinitView と NSWindow の delegate（WinitWindowDelegate）の両方へ確実にメソッドを登録する
        let mut target_classes: Vec<*mut AnyClass> = Vec::new();
        let view_cls = object_getClass(view.cast()).cast_mut();
        if !view_cls.is_null() {
            target_classes.push(view_cls);
        }
        let ns_win: *mut AnyObject = msg_send![view, window];
        if !ns_win.is_null() {
            let delegate: *mut AnyObject = msg_send![ns_win, delegate];
            if !delegate.is_null() {
                let del_cls = object_getClass(delegate.cast()).cast_mut();
                if !del_cls.is_null() {
                    target_classes.push(del_cls);
                }
            }
        }

        for cls in target_classes {
            let _ = class_replaceMethod(
                cls,
                sel!(draggingEntered:),
                imp_enter,
                c"Q@:@".as_ptr().cast::<c_char>(),
            );
            let _ = class_replaceMethod(
                cls,
                sel!(draggingUpdated:),
                imp_update,
                c"Q@:@".as_ptr().cast::<c_char>(),
            );
            let _ = class_replaceMethod(
                cls,
                sel!(prepareForDragOperation:),
                imp_prep,
                c"B@:@".as_ptr().cast::<c_char>(),
            );
            let _ = class_replaceMethod(
                cls,
                sel!(performDragOperation:),
                imp_perf,
                c"B@:@".as_ptr().cast::<c_char>(),
            );
        }

        let t1 = NSString::from_str("public.file-url");
        let t2 = NSString::from_str("public.utf8-plain-text");
        let t3 = NSString::from_str("NSFilenamesPboardType");
        let t4 = NSString::from_str("public.png");
        let t5 = NSString::from_str("public.tiff");
        let mut type_objs: Vec<Retained<NSString>> = vec![t1, t2, t3, t4, t5];
        if let Some(promise_cls) = AnyClass::get(c"NSFilePromiseReceiver") {
            let promise_types: Option<Retained<NSArray<NSString>>> =
                msg_send![promise_cls, readableDraggedTypes];
            if let Some(ptypes) = promise_types {
                for t in ptypes.iter() {
                    type_objs.push(t);
                }
            }
        }
        let type_refs: Vec<&NSString> = type_objs.iter().map(|s| &**s).collect();
        let types = NSArray::from_slice(&type_refs);
        let _: () = msg_send![view, registerForDraggedTypes: &*types];
        if !ns_win.is_null() {
            let _: () = msg_send![ns_win, registerForDraggedTypes: &*types];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_file_drop_handler(
    _window: &winit::window::Window,
    _proxy: winit::event_loop::EventLoopProxy<crate::terminal_pane::AppEvent>,
) {
}
