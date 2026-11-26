use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use winit::event_loop::EventLoopProxy;
use winit::window::Window;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, Rect as WryRect, WebView, WebViewBuilder};

use crate::key_map::{AppShortcut, FocusDir};
use crate::layout::Rect;
use crate::terminal_pane::{AppEvent, PaneId};

pub const DEFAULT_BROWSER_URL: &str = "https://www.google.com";

/// YouTube 等の Fullscreen API を OS 全画面に飛ばさず、ブラウザタブ（ビューポート）内だけの
/// 全画面表示へ差し替える初期化スクリプト
const IN_TAB_FULLSCREEN_JS: &str = r#"
(() => {
  if (window.__tentInTabFsInstalled) return;
  window.__tentInTabFsInstalled = true;

  let fsEl = null;
  let fsAncestors = [];
  let fsDisconnectObserver = null;
  let savedWinScroll = null;
  let savedAncestorScrolls = [];

  function ensureStyle() {
    if (document.getElementById('__tent-fs-style')) return;
    const style = document.createElement('style');
    style.id = '__tent-fs-style';
    style.textContent = `
      html.__tent-fs-lock, body.__tent-fs-lock {
        overflow: hidden !important;
      }
      .__tent-fs-ancestor {
        filter: none !important;
        perspective: none !important;
        contain: none !important;
        clip-path: none !important;
        z-index: auto !important;
        overflow: visible !important;
      }
      .__tent-fs-ancestor-untransform {
        transform: none !important;
      }
      .__tent-fs-target {
        position: fixed !important;
        top: 0 !important;
        left: 0 !important;
        right: 0 !important;
        bottom: 0 !important;
        width: 100vw !important;
        height: 100vh !important;
        max-width: 100vw !important;
        max-height: 100vh !important;
        margin: 0 !important;
        padding: 0 !important;
        z-index: 2147483647 !important;
        background: #000 !important;
        overflow: hidden !important;
      }
      .__tent-fs-target .html5-video-player,
      .__tent-fs-target .html5-video-container {
        position: fixed !important;
        top: 0 !important;
        left: 0 !important;
        width: 100vw !important;
        height: 100vh !important;
        margin: 0 !important;
        padding: 0 !important;
      }
      .__tent-fs-target video {
        position: fixed !important;
        top: 0 !important;
        left: 0 !important;
        right: 0 !important;
        bottom: 0 !important;
        width: 100vw !important;
        height: 100vh !important;
        max-width: 100vw !important;
        max-height: 100vh !important;
        margin: 0 !important;
        padding: 0 !important;
        transform: none !important;
        object-fit: contain !important;
      }
      .__tent-fs-target .ytp-chrome-bottom {
        left: 12px !important;
        width: calc(100vw - 24px) !important;
      }
    `;
    (document.head || document.documentElement).appendChild(style);
  }

  function fireEvents(target) {
    const ev1 = new Event('fullscreenchange', { bubbles: true });
    const ev2 = new Event('webkitfullscreenchange', { bubbles: true });
    document.dispatchEvent(ev1);
    document.dispatchEvent(ev2);
    if (target) {
      target.dispatchEvent(new Event('fullscreenchange', { bubbles: false }));
      target.dispatchEvent(new Event('webkitfullscreenchange', { bubbles: false }));
    }
    window.dispatchEvent(new Event('resize'));
    setTimeout(() => window.dispatchEvent(new Event('resize')), 60);
  }

  function restoreSavedScroll(winScroll, ancScrolls) {
    if (winScroll) {
      window.scrollTo(winScroll.x, winScroll.y);
    }
    for (const item of ancScrolls) {
      if (item.el && item.el.isConnected) {
        item.el.scrollLeft = item.x;
        item.el.scrollTop = item.y;
      }
    }
  }

  function exitInTabFullscreen() {
    if (!fsEl) return Promise.resolve();
    const prev = fsEl;
    const winScroll = savedWinScroll;
    const ancScrolls = savedAncestorScrolls;
    fsEl = null;
    savedWinScroll = null;
    savedAncestorScrolls = [];
    if (fsDisconnectObserver) {
      fsDisconnectObserver.disconnect();
      fsDisconnectObserver = null;
    }
    prev.classList.remove('__tent-fs-target');
    prev.style.removeProperty('transform');
    for (const a of fsAncestors) {
      a.classList.remove('__tent-fs-ancestor');
      a.classList.remove('__tent-fs-ancestor-untransform');
    }
    fsAncestors = [];
    document.documentElement.classList.remove('__tent-fs-lock');
    if (document.body) document.body.classList.remove('__tent-fs-lock');
    restoreSavedScroll(winScroll, ancScrolls);
    fireEvents(prev);
    // 仮想スクロール等が fullscreenchange / resize 直後に先頭へ戻すのを防ぐため再復元
    requestAnimationFrame(() => restoreSavedScroll(winScroll, ancScrolls));
    setTimeout(() => restoreSavedScroll(winScroll, ancScrolls), 80);
    return Promise.resolve();
  }

  function enterInTabFullscreen(el) {
    if (!el) return Promise.resolve();
    ensureStyle();
    if (fsEl && fsEl !== el) {
      exitInTabFullscreen();
    }
    savedWinScroll = { x: window.scrollX || 0, y: window.scrollY || 0 };
    savedAncestorScrolls = [];
    fsEl = el;
    fsAncestors = [];
    let p = el.parentElement;
    while (p && p !== document.documentElement) {
      if (p.scrollTop || p.scrollLeft) {
        savedAncestorScrolls.push({ el: p, x: p.scrollLeft, y: p.scrollTop });
      }
      p.classList.add('__tent-fs-ancestor');
      // X の仮想スクロールセル (cellInnerDiv) の translateY を壊すと unmount やスクロール位置崩壊を招くため温存する
      const isVirtualCell = p.getAttribute && p.getAttribute('data-testid') === 'cellInnerDiv';
      if (!isVirtualCell) {
        p.classList.add('__tent-fs-ancestor-untransform');
      }
      fsAncestors.push(p);
      p = p.parentElement;
    }
    el.classList.add('__tent-fs-target');
    el.style.setProperty('transform', 'none', 'important');
    document.documentElement.classList.add('__tent-fs-lock');
    if (document.body) document.body.classList.add('__tent-fs-lock');

    // 温存した仮想スクロールセルの transform によるズレを el 自身の逆平行移動で吸収し (0,0) に固定
    const rect = el.getBoundingClientRect();
    if (Math.abs(rect.left) > 0.5 || Math.abs(rect.top) > 0.5) {
      el.style.setProperty(
        'transform',
        `translate3d(${(-rect.left).toFixed(1)}px, ${(-rect.top).toFixed(1)}px, 0)`,
        'important'
      );
    }

    // 万一フルスクリーン中に要素が DOM から切り離されても、スクロールロックが残留しないよう自動解除する
    if (document.body) {
      fsDisconnectObserver = new MutationObserver(() => {
        if (fsEl && !fsEl.isConnected) {
          exitInTabFullscreen();
        }
      });
      fsDisconnectObserver.observe(document.body, { childList: true, subtree: true });
    }

    fireEvents(el);
    return Promise.resolve();
  }

  // X (x.com / twitter.com) では仮想スクロール・モーダルと競合するためタブ内フルスクリーン乗っ取りを無効化する
  const fsHost = (location.hostname || '').toLowerCase();
  const skipInTabFs =
    fsHost === 'x.com' ||
    fsHost.endsWith('.x.com') ||
    fsHost === 'twitter.com' ||
    fsHost.endsWith('.twitter.com');

  if (!skipInTabFs) {
    try {
      Object.defineProperty(Document.prototype, 'fullscreenEnabled', { get: () => true, configurable: true });
      Object.defineProperty(Document.prototype, 'webkitFullscreenEnabled', { get: () => true, configurable: true });
      Object.defineProperty(Document.prototype, 'fullscreenElement', { get: () => fsEl, configurable: true });
      Object.defineProperty(Document.prototype, 'webkitFullscreenElement', { get: () => fsEl, configurable: true });
      Object.defineProperty(Document.prototype, 'webkitIsFullScreen', { get: () => fsEl !== null, configurable: true });
    } catch (_) {}

    Element.prototype.requestFullscreen = function() { return enterInTabFullscreen(this); };
    Element.prototype.webkitRequestFullscreen = function() { return enterInTabFullscreen(this); };
    Element.prototype.webkitRequestFullScreen = function() { return enterInTabFullscreen(this); };
    if (typeof HTMLVideoElement !== 'undefined') {
      HTMLVideoElement.prototype.webkitEnterFullscreen = function() { return enterInTabFullscreen(this); };
      HTMLVideoElement.prototype.webkitEnterFullScreen = function() { return enterInTabFullscreen(this); };
      HTMLVideoElement.prototype.webkitExitFullscreen = function() { return exitInTabFullscreen(); };
    }
    Document.prototype.exitFullscreen = function() { return exitInTabFullscreen(); };
    Document.prototype.webkitExitFullscreen = function() { return exitInTabFullscreen(); };
    Document.prototype.webkitCancelFullScreen = function() { return exitInTabFullscreen(); };

    window.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && fsEl) {
        e.preventDefault();
        exitInTabFullscreen();
      }
    }, true);
  }

  // 2本指スワイプの戻る／進む予備動作インジケータ（画面左右端から矢印ピルがせり出す）
  let swipeEl = null;
  function ensureSwipePill() {
    if (swipeEl && swipeEl.isConnected) return swipeEl;
    const el = document.createElement('div');
    el.id = '__tent-swipe-pill';
    el.style.cssText = [
      'position:fixed',
      'top:50%',
      'width:44px',
      'height:44px',
      'margin-top:-22px',
      'border-radius:22px',
      'display:flex',
      'align-items:center',
      'justify-content:center',
      'font:700 20px/1 -apple-system,BlinkMacSystemFont,sans-serif',
      'pointer-events:none',
      'z-index:2147483647',
      'box-shadow:0 4px 16px rgba(0,0,0,0.45)',
      'backdrop-filter:blur(8px)',
      '-webkit-backdrop-filter:blur(8px)',
      'opacity:0',
      'will-change:transform,opacity',
    ].join(';');
    (document.body || document.documentElement).appendChild(el);
    swipeEl = el;
    return el;
  }

  window.__tentSwipePreview = function(dir, progress, ready) {
    const el = ensureSwipePill();
    if (!dir || progress <= 0.001) {
      el.style.transition = 'transform 160ms ease-out, opacity 160ms ease-out';
      el.style.opacity = '0';
      if (el.dataset.dir === '-1') {
        el.style.transform = 'translate3d(-56px,0,0) scale(0.85)';
      } else {
        el.style.transform = 'translate3d(56px,0,0) scale(0.85)';
      }
      return;
    }
    el.dataset.dir = String(dir);
    el.style.transition = 'background-color 90ms ease, border-color 90ms ease, color 90ms ease';
    const p = Math.max(0, Math.min(1, progress));
    const scale = ready ? 1.12 : (0.85 + 0.15 * p);
    const offset = -48 + 70 * p;
    if (dir < 0) {
      el.style.left = '0px';
      el.style.right = 'auto';
      el.textContent = '←';
      el.style.transform = `translate3d(${offset.toFixed(1)}px,0,0) scale(${scale.toFixed(3)})`;
    } else {
      el.style.left = 'auto';
      el.style.right = '0px';
      el.textContent = '→';
      el.style.transform = `translate3d(${(-offset).toFixed(1)}px,0,0) scale(${scale.toFixed(3)})`;
    }
    el.style.opacity = String(Math.min(1, 0.25 + 0.75 * p));
    if (ready) {
      el.style.backgroundColor = 'rgba(52, 152, 240, 0.94)';
      el.style.border = '1.5px solid rgba(130, 210, 255, 0.95)';
      el.style.color = '#ffffff';
    } else {
      el.style.backgroundColor = 'rgba(22, 26, 36, 0.86)';
      el.style.border = '1.5px solid rgba(140, 160, 195, 0.45)';
      el.style.color = '#d8dee9';
    }
  };

  // X (x.com / twitter.com) のプロモーション広告投稿をまるごと非表示にする
  const host = (location.hostname || '').toLowerCase();
  if (host === 'x.com' || host.endsWith('.x.com') || host === 'twitter.com' || host.endsWith('.twitter.com')) {
    const AD_LABELS = new Set(['広告', 'プロモーション', 'Ad', 'Promoted', 'Sponsored']);

    function ensureAdStyle() {
      if (document.getElementById('__tent-x-ad-style')) return;
      const st = document.createElement('style');
      st.id = '__tent-x-ad-style';
      st.textContent = `
        article.__tent-x-ad-hidden {
          position: relative !important;
          pointer-events: none !important;
        }
        article.__tent-x-ad-hidden > * {
          visibility: hidden !important;
          pointer-events: none !important;
        }
        article.__tent-x-ad-hidden::after {
          content: '広告';
          position: absolute;
          inset: 6px 12px;
          display: flex;
          align-items: center;
          justify-content: center;
          border-radius: 8px;
          border: 1px dashed rgba(120, 134, 158, 0.28);
          background: rgba(255, 255, 255, 0.02);
          color: rgba(130, 144, 168, 0.55);
          font: 600 12px/1 -apple-system, BlinkMacSystemFont, sans-serif;
          letter-spacing: 0.08em;
          pointer-events: none;
        }
      `;
      (document.head || document.documentElement).appendChild(st);
    }

    function isPromotedArticle(article) {
      // 1. 右上の「...」メニュー ([data-testid="caret"]) のすぐ隣にある広告ラベルのみを見る
      //    （placementTracking は通常ポストの動画プレイヤーにも付くため使わない）
      const caret = article.querySelector('[data-testid="caret"]');
      const headerBox = caret && caret.parentElement && caret.parentElement.parentElement;
      if (headerBox) {
        const spans = headerBox.querySelectorAll('span');
        for (let i = 0; i < spans.length; i++) {
          const raw = (spans[i].textContent || '').replace(/\s+/g, '');
          if (AD_LABELS.has(raw)) return true;
        }
      }

      // 2. 広告専用リンク（/i/ads, twclid=）を含む場合
      if (article.querySelector('a[href*="/i/ads"], a[href*="twclid="]')) return true;

      return false;
    }

    function hideXAds() {
      ensureAdStyle();
      // MutationObserver のマイクロタスク内で即座に判定・隠蔽することで、1フレーム目のチラ見えを防ぐ
      const articles = document.querySelectorAll('article[data-testid="tweet"]');
      for (let i = 0; i < articles.length; i++) {
        const art = articles[i];
        if (isPromotedArticle(art)) {
          if (!art.classList.contains('__tent-x-ad-hidden')) {
            art.classList.add('__tent-x-ad-hidden');
            const videos = art.querySelectorAll('video');
            for (let j = 0; j < videos.length; j++) {
              try { videos[j].pause(); } catch (_) {}
            }
          }
        } else if (art.classList.contains('__tent-x-ad-hidden')) {
          art.classList.remove('__tent-x-ad-hidden');
        }
      }
    }

    const startObserver = () => {
      hideXAds();
      const root = document.body || document.documentElement;
      if (!root) return;
      new MutationObserver(hideXAds).observe(root, { childList: true, subtree: true });
    };
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', startObserver, { once: true });
    } else {
      startObserver();
    }
  }
})();
"#;

/// ブラウザペインのメタデータ（Workspace / Slot 側で保持し、UI なしでもテスト可能にする）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserPaneMeta {
    pub id: PaneId,
    pub url: String,
    pub title: String,
    pub is_playing_audio: bool,
}

impl BrowserPaneMeta {
    pub fn new(id: PaneId, url: impl Into<String>) -> Self {
        let url = normalize_url_input(&url.into());
        Self {
            id,
            url,
            title: String::new(),
            is_playing_audio: false,
        }
    }

    /// サイドバーやステータスバー向けの短いラベル
    pub fn display_label(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else {
            extract_host_label(&self.url)
        }
    }
}

/// ユーザーが URL バーに入力した文字列をナビゲーション可能な URL へ正規化する
pub fn normalize_url_input(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_BROWSER_URL.to_string();
    }
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
        || trimmed.starts_with("about:")
    {
        return trimmed.to_string();
    }
    if trimmed == "localhost"
        || trimmed.starts_with("localhost:")
        || trimmed.starts_with("localhost/")
        || trimmed.starts_with("127.0.0.1")
        || trimmed.starts_with("[::1]")
    {
        return format!("http://{trimmed}");
    }
    if !trimmed.contains(char::is_whitespace) && trimmed.contains('.') {
        return format!("https://{trimmed}");
    }
    // 検索クエリとして扱う
    let encoded = percent_encode_query(trimmed);
    format!("https://www.google.com/search?q={encoded}")
}

fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push_str(&format!("%{other:02X}"));
            }
        }
    }
    out
}

/// URL からホスト名（ポート付き）を抽出する
pub fn extract_host_label(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() {
        "Web".to_string()
    } else {
        host.to_string()
    }
}

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
                }
                if !managed.visible {
                    let _ = managed.webview.set_visible(true);
                    managed.visible = true;
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
                    }
                    Err(err) => {
                        eprintln!("TenT: failed to create WKWebView for pane {id}: {err}");
                    }
                }
            }
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

    /// 閉じられたブラウザペインの WebView を破棄する
    pub fn retain_ids(&mut self, live_ids: &[PaneId]) {
        self.views.retain(|id, _| live_ids.contains(id));
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
                    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
                    CGContext::draw_image(Some(&ctx), rect, Some(&cg));
                    let _ = proxy.send_event(AppEvent::BrowserSnapshot {
                        pane_id,
                        width: w as u32,
                        height: h as u32,
                        rgba,
                    });
                }
            });
            unsafe {
                wk.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn capture_snapshots(&self, _target_ids: &[PaneId]) {}
}

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

            if has_cmd && !has_ctrl {
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
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url_input() {
        assert_eq!(normalize_url_input(""), DEFAULT_BROWSER_URL);
        assert_eq!(
            normalize_url_input("https://example.com"),
            "https://example.com"
        );
        assert_eq!(
            normalize_url_input("localhost:3000/app"),
            "http://localhost:3000/app"
        );
        assert_eq!(
            normalize_url_input("github.com/rust-lang"),
            "https://github.com/rust-lang"
        );
        assert_eq!(
            normalize_url_input("rust wry webview"),
            "https://www.google.com/search?q=rust+wry+webview"
        );
    }

    #[test]
    fn test_extract_host_label() {
        assert_eq!(
            extract_host_label("https://www.github.com/user/repo"),
            "github.com"
        );
        assert_eq!(
            extract_host_label("http://localhost:3000/dashboard"),
            "localhost:3000"
        );
        assert_eq!(extract_host_label("about:blank"), "about:blank");
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
