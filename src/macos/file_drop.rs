#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::ptr::NonNull;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
use block2::RcBlock;
#[cfg(target_os = "macos")]
use objc2::msg_send;
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyClass, AnyObject, Bool};
#[cfg(target_os = "macos")]
use objc2_core_foundation::{CGPoint, CGRect};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};

#[cfg(target_os = "macos")]
use super::clipboard::extract_pasteboard_image_to_temp;
#[cfg(target_os = "macos")]
use crate::platform::AppProxy;

#[cfg(target_os = "macos")]
unsafe fn drag_location_px(sender: &AnyObject) -> Option<(f32, f32)> {
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

/// macOS スクショ右下サムネイルの一時領域（`TemporaryItems/NSIRD_screencaptureui_*`）は
/// サムネイル消滅後に削除されたりサンドボックスから読めないため、`/tmp/tent-drops/` へ退避する。
#[cfg(target_os = "macos")]
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

/// `TentMetalView` へドラッグ受付タイプ（`public.file-url` / `public.png` / `public.tiff` / `NSFilePromiseReceiver` 等）を登録する
#[cfg(target_os = "macos")]
pub fn register_dragged_types(view: &AnyObject) {
    let t1 = NSString::from_str("public.file-url");
    let t2 = NSString::from_str("public.utf8-plain-text");
    let t3 = NSString::from_str("NSFilenamesPboardType");
    let t4 = NSString::from_str("public.png");
    let t5 = NSString::from_str("public.tiff");
    let mut type_objs: Vec<Retained<NSString>> = vec![t1, t2, t3, t4, t5];
    unsafe {
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
    }
}

#[cfg(target_os = "macos")]
pub fn notify_drag_hover(sender: &AnyObject, proxy: &AppProxy) {
    if let Some((px, py)) = unsafe { drag_location_px(sender) } {
        let _ = proxy.send_event(crate::terminal_pane::AppEvent::DragHover { px, py });
    }
}

#[cfg(target_os = "macos")]
pub fn handle_perform_drag(sender: &AnyObject, proxy: &AppProxy) -> Bool {
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
                let s: Option<Retained<NSString>> = unsafe { msg_send![&*plist, objectAtIndex: i] };
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
            static PROMISE_QUEUE: OnceLock<usize> = OnceLock::new();
            let queue_ptr = *PROMISE_QUEUE.get_or_init(|| {
                let q: Option<Retained<AnyObject>> = unsafe { msg_send![queue_cls, new] };
                q.map_or(0, |r| Retained::into_raw(r) as usize)
            });
            if queue_ptr != 0 {
                let queue = queue_ptr as *const AnyObject;
                for receiver in receivers.iter() {
                    let proxy_clone = proxy.clone();
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
                                if !p.as_os_str().is_empty() {
                                    let _ = proxy_clone.send_event(
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

    if !paths.is_empty() {
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
