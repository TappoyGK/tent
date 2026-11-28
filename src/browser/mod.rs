mod macos_monitor;
mod scripts;
mod url;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use winit::event_loop::EventLoopProxy;
use winit::window::Window;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, Rect as WryRect, WebView, WebViewBuilder};

pub use macos_monitor::{current_macos_modifiers, install_macos_shortcut_monitor};
use scripts::IN_TAB_FULLSCREEN_JS;
pub use url::{BrowserPaneMeta, DEFAULT_BROWSER_URL, escape_js_string, normalize_url_input};

use crate::layout::Rect;
use crate::terminal_pane::{AppEvent, PaneId};

struct ManagedWebView {
    webview: WebView,
    last_bounds: (f64, f64, f64, f64),
    visible: bool,
    in_flight: Arc<AtomicBool>,
}

/// winit Window 上の子 WKWebView 群のライフサイクル・位置・可視性を管理する
pub struct BrowserManager {
    views: HashMap<PaneId, ManagedWebView>,
    proxy: EventLoopProxy<AppEvent>,
}

impl BrowserManager {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        Self {
            views: HashMap::new(),
            proxy,
        }
    }

    /// 現在表示すべきブラウザ一覧に合わせて WKWebView を生成・リサイズ・表示切替する
    pub fn sync(
        &mut self,
        window: &Window,
        visible_browsers: &[(PaneId, Rect, &str)],
        scale_factor: f64,
    ) {
        let scale = scale_factor.max(1.0);
        let mut cursor_rects_dirty = false;

        // 1. 今回表示しない WebView を非表示にする
        // macOS では isHidden = YES でも frame がウィンドウ内にあると CoreDrag が
        // 非表示 WKWebView へ D&D を吸い込むため、サイズを保ったままウィンドウ外へ退避する
        for (&id, managed) in &mut self.views {
            let should_show = visible_browsers.iter().any(|(vid, _, _)| *vid == id);
            if !should_show && managed.visible {
                let _ = managed.webview.set_visible(false);
                let (_, _, lw, lh) = managed.last_bounds;
                let offscreen = WryRect {
                    position: LogicalPosition::new(-10000.0, -10000.0).into(),
                    size: LogicalSize::new(lw, lh).into(),
                };
                let _ = managed.webview.set_bounds(offscreen);
                managed.last_bounds = (-10000.0, -10000.0, lw, lh);
                managed.visible = false;
                cursor_rects_dirty = true;
            }
        }

        // 2. 表示すべき WebView を生成または位置更新して表示する
        for &(id, phys_rect, url) in visible_browsers {
            let lx = (phys_rect.x as f64) / scale;
            let ly = (phys_rect.y as f64) / scale;
            let lw = ((phys_rect.w as f64) / scale).max(1.0);
            let lh = ((phys_rect.h as f64) / scale).max(1.0);
            let bounds_tuple = (lx, ly, lw, lh);
            let wry_rect = WryRect {
                position: LogicalPosition::new(lx, ly).into(),
                size: LogicalSize::new(lw, lh).into(),
            };

            if let Some(managed) = self.views.get_mut(&id) {
                if (managed.last_bounds.0 - lx).abs() > 0.5
                    || (managed.last_bounds.1 - ly).abs() > 0.5
                    || (managed.last_bounds.2 - lw).abs() > 0.5
                    || (managed.last_bounds.3 - lh).abs() > 0.5
                {
                    let _ = managed.webview.set_bounds(wry_rect);
                    managed.last_bounds = bounds_tuple;
                    cursor_rects_dirty = true;
                }
                if !managed.visible {
                    let _ = managed.webview.set_visible(true);
                    managed.visible = true;
                    cursor_rects_dirty = true;
                }
            } else {
                let proxy_nav = self.proxy.clone();
                let proxy_title = self.proxy.clone();
                let proxy_popup = self.proxy.clone();
                let proxy_ipc = self.proxy.clone();
                let nav_url = normalize_url_input(url);
                let mut builder = WebViewBuilder::new()
                    .with_url(&nav_url)
                    .with_bounds(wry_rect)
                    .with_devtools(true)
                    .with_back_forward_navigation_gestures(false)
                    .with_accept_first_mouse(true)
                    .with_initialization_script(IN_TAB_FULLSCREEN_JS)
                    .with_ipc_handler(move |req| {
                        let body = req.body().as_str();
                        if body == "audio:1" || body == "audio:0" {
                            let _ = proxy_ipc.send_event(AppEvent::BrowserAudioState {
                                pane_id: id,
                                is_playing: body == "audio:1",
                            });
                        }
                    })
                    .with_new_window_req_handler(move |popup_url, _features| {
                        if popup_url.starts_with("http://") || popup_url.starts_with("https://") {
                            let _ = proxy_popup.send_event(AppEvent::BrowserOpenNewWindow {
                                pane_id: id,
                                url: popup_url,
                            });
                        }
                        NewWindowResponse::Deny
                    })
                    .with_navigation_handler(move |new_url| {
                        // 広告・解析用 iframe の about:blank / data: 等でメイン URL を上書きしない
                        if new_url.starts_with("http://") || new_url.starts_with("https://") {
                            let _ = proxy_nav.send_event(AppEvent::BrowserUrlChanged {
                                pane_id: id,
                                url: new_url,
                            });
                        }
                        true
                    })
                    .with_document_title_changed_handler(move |title| {
                        let _ = proxy_title
                            .send_event(AppEvent::BrowserTitleChanged { pane_id: id, title });
                    });
                #[cfg(target_os = "macos")]
                {
                    use wry::WebViewBuilderExtDarwin;
                    // bare binary 起動でも Cookie・ログイン状態を永続化するための固定 DataStore UUID
                    builder = builder.with_data_store_identifier(*b"TenTBrowserStore");
                }
                match builder.build_as_child(window) {
                    Ok(webview) => {
                        #[cfg(target_os = "macos")]
                        {
                            use wry::WebViewExtMacOS;
                            let wk = webview.webview();
                            unsafe {
                                wk.setAllowsMagnification(true);
                                wk.setAllowsBackForwardNavigationGestures(false);
                            }
                        }
                        self.views.insert(
                            id,
                            ManagedWebView {
                                webview,
                                last_bounds: bounds_tuple,
                                visible: true,
                                in_flight: Arc::new(AtomicBool::new(false)),
                            },
                        );
                        cursor_rects_dirty = true;
                    }
                    Err(err) => {
                        eprintln!("TenT: failed to create WKWebView for pane {id}: {err}");
                    }
                }
            }
        }
        if cursor_rects_dirty {
            crate::macos::invalidate_cursor_rects(window);
        }
    }

    /// 非アクティブスロットのブラウザを非表示（フォーカスを奪わない）のまま裏で順次生成する
    pub fn warm_up_hidden(
        &mut self,
        window: &Window,
        id: PaneId,
        phys_rect: Rect,
        url: &str,
        scale_factor: f64,
    ) {
        if self.views.contains_key(&id) {
            return;
        }
        let scale = scale_factor.max(1.0);
        let lw = ((phys_rect.w as f64) / scale).max(1.0);
        let lh = ((phys_rect.h as f64) / scale).max(1.0);
        // macOS の非表示 WKWebView がターミナルへの D&D を吸い込まないようウィンドウ外へ配置
        let bounds_tuple = (-10000.0, -10000.0, lw, lh);
        let wry_rect = WryRect {
            position: LogicalPosition::new(-10000.0, -10000.0).into(),
            size: LogicalSize::new(lw, lh).into(),
        };
        let proxy_nav = self.proxy.clone();
        let proxy_title = self.proxy.clone();
        let proxy_popup = self.proxy.clone();
        let proxy_ipc = self.proxy.clone();
        let nav_url = normalize_url_input(url);
        let mut builder = WebViewBuilder::new()
            .with_url(&nav_url)
            .with_bounds(wry_rect)
            .with_visible(false)
            .with_devtools(true)
            .with_back_forward_navigation_gestures(false)
            .with_accept_first_mouse(true)
            .with_initialization_script(IN_TAB_FULLSCREEN_JS)
            .with_ipc_handler(move |req| {
                let body = req.body().as_str();
                if body == "audio:1" || body == "audio:0" {
                    let _ = proxy_ipc.send_event(AppEvent::BrowserAudioState {
                        pane_id: id,
                        is_playing: body == "audio:1",
                    });
                }
            })
            .with_new_window_req_handler(move |popup_url, _features| {
                if popup_url.starts_with("http://") || popup_url.starts_with("https://") {
                    let _ = proxy_popup.send_event(AppEvent::BrowserOpenNewWindow {
                        pane_id: id,
                        url: popup_url,
                    });
                }
                NewWindowResponse::Deny
            })
            .with_navigation_handler(move |new_url| {
                if new_url.starts_with("http://") || new_url.starts_with("https://") {
                    let _ = proxy_nav.send_event(AppEvent::BrowserUrlChanged {
                        pane_id: id,
                        url: new_url,
                    });
                }
                true
            })
            .with_document_title_changed_handler(move |title| {
                let _ =
                    proxy_title.send_event(AppEvent::BrowserTitleChanged { pane_id: id, title });
            });
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewBuilderExtDarwin;
            builder = builder.with_data_store_identifier(*b"TenTBrowserStore");
        }
        match builder.build_as_child(window) {
            Ok(webview) => {
                #[cfg(target_os = "macos")]
                {
                    use wry::WebViewExtMacOS;
                    let wk = webview.webview();
                    unsafe {
                        wk.setAllowsMagnification(true);
                        wk.setAllowsBackForwardNavigationGestures(false);
                    }
                }
                self.views.insert(
                    id,
                    ManagedWebView {
                        webview,
                        last_bounds: bounds_tuple,
                        visible: false,
                        in_flight: Arc::new(AtomicBool::new(false)),
                    },
                );
            }
            Err(err) => {
                eprintln!("TenT: failed to warm up WKWebView for pane {id}: {err}");
            }
        }
    }

    /// 閉じられたブラウザペインの WebView を破棄する。
    /// 1 つ以上破棄した場合は `true` を返す（親ビューのカーソル矩形再計算トリガー用）。
    pub fn retain_ids(&mut self, live_ids: &[PaneId]) -> bool {
        let before = self.views.len();
        self.views.retain(|id, _| live_ids.contains(id));
        self.views.len() != before
    }

    pub fn navigate(&self, pane_id: PaneId, url: &str) {
        if let Some(managed) = self.views.get(&pane_id) {
            let normalized = normalize_url_input(url);
            let _ = managed.webview.load_url(&normalized);
        }
    }

    pub fn go_back(&self, pane_id: PaneId) {
        if let Some(managed) = self.views.get(&pane_id) {
            #[cfg(target_os = "macos")]
            {
                use wry::WebViewExtMacOS;
                let wk = managed.webview.webview();
                unsafe {
                    let _ = wk.goBack();
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = managed.webview.evaluate_script("window.history.back()");
            }
        }
    }

    pub fn go_forward(&self, pane_id: PaneId) {
        if let Some(managed) = self.views.get(&pane_id) {
            #[cfg(target_os = "macos")]
            {
                use wry::WebViewExtMacOS;
                let wk = managed.webview.webview();
                unsafe {
                    let _ = wk.goForward();
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = managed.webview.evaluate_script("window.history.forward()");
            }
        }
    }

    /// ブラウザペイン内でテキストを検索して該当箇所へスクロール・選択する
    pub fn find_in_page(
        &self,
        pane_id: PaneId,
        query: &str,
        backwards: bool,
        reset_selection: bool,
    ) {
        let Some(managed) = self.views.get(&pane_id) else {
            return;
        };
        if query.is_empty() {
            self.clear_find_highlight(pane_id);
            return;
        }
        let escaped = escape_js_string(query);
        let reset_js = if reset_selection {
            "try { const s = window.getSelection(); if (s) s.removeAllRanges(); } catch (_) {}"
        } else {
            ""
        };
        let back = if backwards { "true" } else { "false" };
        let script = format!(
            "(() => {{ {reset_js} window.find(\"{escaped}\", false, {back}, true, false, false, false); }})();"
        );
        let _ = managed.webview.evaluate_script(&script);
    }

    /// ブラウザペイン内の検索選択ハイライトを解除する
    pub fn clear_find_highlight(&self, pane_id: PaneId) {
        if let Some(managed) = self.views.get(&pane_id) {
            let _ = managed.webview.evaluate_script(
                "try { const s = window.getSelection(); if (s) s.removeAllRanges(); } catch (_) {}",
            );
        }
    }

    pub fn reload(&self, pane_id: PaneId) {
        if let Some(managed) = self.views.get(&pane_id) {
            let _ = managed.webview.reload();
        }
    }

    /// 2本指スワイプの予備動作インジケータ（画面左右端の矢印ピル）を更新する
    pub fn set_swipe_preview(&self, pane_id: PaneId, dir: i8, progress: f32, ready: bool) {
        let Some(managed) = self.views.get(&pane_id) else {
            return;
        };
        #[cfg(target_os = "macos")]
        if dir != 0 {
            use wry::WebViewExtMacOS;
            let wk = managed.webview.webview();
            let can_nav = unsafe {
                if dir < 0 {
                    wk.canGoBack()
                } else {
                    wk.canGoForward()
                }
            };
            if !can_nav {
                let _ = managed.webview.evaluate_script(
                    "window.__tentSwipePreview&&window.__tentSwipePreview(0,0,false)",
                );
                return;
            }
        }
        let js = format!(
            "window.__tentSwipePreview&&window.__tentSwipePreview({dir},{progress:.3},{ready})"
        );
        let _ = managed.webview.evaluate_script(&js);
    }

    pub fn focus_webview(&self, pane_id: PaneId) {
        if let Some(managed) = self.views.get(&pane_id) {
            let _ = managed.webview.focus();
        }
    }

    /// 親の WinitView へフォーカスを戻す（ターミナルや URL バー入力へ切り替えるとき用）
    pub fn focus_parent(&self) {
        if let Some(managed) = self.views.values().next() {
            let _ = managed.webview.focus_parent();
        }
    }

    pub fn has_any_views(&self) -> bool {
        !self.views.is_empty()
    }

    /// 全 WebView の現在の実 URL（SPA の history.pushState 遷移含む）を取得する
    pub fn live_urls(&self) -> Vec<(PaneId, String)> {
        let mut out = Vec::with_capacity(self.views.len());
        for (&id, managed) in &self.views {
            if let Ok(url) = managed.webview.url()
                && !url.is_empty()
            {
                out.push((id, url));
            }
        }
        out
    }

    /// 全 WebView で実際に音声が出ているか（ミュート自動再生を除外）を非同期照会する
    pub fn poll_audio_states(&self) {
        const CHECK_AUDIBLE_JS: &str = r#"(() => {
            let audible = false;
            for (const el of document.querySelectorAll('video, audio')) {
                if (!el.paused && !el.ended && !el.muted && el.volume > 0 && el.readyState >= 2) {
                    audible = true;
                    break;
                }
            }
            if (window.ipc) window.ipc.postMessage(audible ? 'audio:1' : 'audio:0');
        })();"#;

        for managed in self.views.values() {
            let _ = managed.webview.evaluate_script(CHECK_AUDIBLE_JS);
        }
    }

    /// 指定された WKWebView 群（アクティブ＆バックグラウンドスロット含む）から
    /// サイドバー用の縮小サムネイルを非同期キャプチャする
    #[cfg(target_os = "macos")]
    pub fn capture_snapshots(&self, target_ids: &[PaneId]) {
        use block2::RcBlock;
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSImage;
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImage};
        use objc2_foundation::{NSError, NSNumber};
        use objc2_web_kit::WKSnapshotConfiguration;
        use wry::WebViewExtMacOS;

        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };

        for &pane_id in target_ids {
            let Some(managed) = self.views.get(&pane_id) else {
                continue;
            };
            if managed
                .in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            let wk = managed.webview.webview();
            let config = unsafe { WKSnapshotConfiguration::new(mtm) };
            let width_num = NSNumber::new_f64(160.0);
            unsafe {
                config.setSnapshotWidth(Some(&width_num));
                config.setAfterScreenUpdates(false);
            }
            let proxy = self.proxy.clone();
            let in_flight = Arc::clone(&managed.in_flight);
            let block = RcBlock::new(move |img_ptr: *mut NSImage, err_ptr: *mut NSError| {
                in_flight.store(false, Ordering::Release);
                if img_ptr.is_null() || !err_ptr.is_null() {
                    return;
                }
                objc2::rc::autoreleasepool(|_| {
                    let img = unsafe { &*img_ptr };
                    let Some(cg) = (unsafe {
                        img.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None)
                    }) else {
                        return;
                    };
                    let w = CGImage::width(Some(&cg));
                    let h = CGImage::height(Some(&cg));
                    if w == 0 || h == 0 || w > 1024 || h > 1024 {
                        return;
                    }
                    let Some(cs) = CGColorSpace::new_device_rgb() else {
                        return;
                    };
                    let mut rgba = vec![0u8; w * h * 4];
                    // kCGImageAlphaPremultipliedLast (1) | kCGBitmapByteOrder32Big (4 << 12)
                    let bitmap_info: u32 = 1 | (4 << 12);
                    let ctx = unsafe {
                        CGBitmapContextCreate(
                            rgba.as_mut_ptr().cast(),
                            w,
                            h,
                            8,
                            w * 4,
                            Some(&cs),
                            bitmap_info,
                        )
                    };
                    if let Some(ctx) = ctx {
                        let rect =
                            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
                        CGContext::draw_image(Some(&ctx), rect, Some(&cg));
                        let _ = proxy.send_event(AppEvent::BrowserSnapshot {
                            pane_id,
                            width: w as u32,
                            height: h as u32,
                            rgba,
                        });
                    }
                });
            });
            unsafe {
                wk.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn capture_snapshots(&self, _target_ids: &[PaneId]) {}
}
