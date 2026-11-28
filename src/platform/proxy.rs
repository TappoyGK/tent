use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::terminal_pane::AppEvent;

type WakeCallback = extern "C" fn(*mut c_void);

#[cfg(target_os = "macos")]
unsafe extern "C" {
    // libSystem (Grand Central Dispatch) のメインキュー。常にリンクされている。
    #[link_name = "_dispatch_main_q"]
    static DISPATCH_MAIN_Q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: WakeCallback);
}

struct SharedQueue {
    events: Mutex<VecDeque<AppEvent>>,
    wake_scheduled: AtomicBool,
    wake_fn: Mutex<Option<WakeCallback>>,
}

/// 任意スレッドからメインスレッドの AppKit イベントループへ `AppEvent` を配送するプロキシ
#[derive(Clone)]
pub struct AppProxy {
    inner: Arc<SharedQueue>,
}

impl AppProxy {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SharedQueue {
                events: Mutex::new(VecDeque::new()),
                wake_scheduled: AtomicBool::new(false),
                wake_fn: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn set_wake_callback(&self, cb: WakeCallback) {
        if let Ok(mut guard) = self.inner.wake_fn.lock() {
            *guard = Some(cb);
        }
    }

    pub fn send_event(&self, event: AppEvent) -> Result<(), AppEvent> {
        if let Ok(mut q) = self.inner.events.lock() {
            q.push_back(event);
        } else {
            return Err(event);
        }
        self.request_wake();
        Ok(())
    }

    pub(crate) fn request_wake(&self) {
        if self
            .inner
            .wake_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            #[cfg(target_os = "macos")]
            if let Ok(guard) = self.inner.wake_fn.lock()
                && let Some(cb) = *guard
            {
                unsafe {
                    dispatch_async_f(&DISPATCH_MAIN_Q as *const c_void, std::ptr::null_mut(), cb);
                }
            }
        }
    }

    pub(crate) fn drain_events(&self) -> Vec<AppEvent> {
        self.inner.wake_scheduled.store(false, Ordering::Release);
        let mut out = Vec::new();
        if let Ok(mut q) = self.inner.events.lock() {
            while let Some(ev) = q.pop_front() {
                out.push(ev);
            }
        }
        out
    }
}
