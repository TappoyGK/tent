/// YouTube 等の Fullscreen API を OS 全画面に飛ばさず、ブラウザタブ（ビューポート）内だけの
/// 全画面表示へ差し替える初期化スクリプト
pub(super) const IN_TAB_FULLSCREEN_JS: &str = r#"
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
