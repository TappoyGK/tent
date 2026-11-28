mod menu;
mod proxy;
pub mod types;
mod url_field;
mod window;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSTimer};

pub use proxy::AppProxy;
pub use types::{
    ControlFlow, ElementState, Ime, Key, KeyCode, KeyEvent, LogicalPosition, LogicalSize,
    ModifiersState, MouseButton, MouseScrollDelta, NamedKey, PhysicalKey, PhysicalPosition,
    PhysicalSize, WindowEvent,
};
pub use window::{NativeWindow, WindowAttributes};

use crate::terminal_pane::AppEvent;

/// アプリケーション本体のライフサイクル・イベント受信トレイト
pub trait ApplicationHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop);
    fn exiting(&mut self, event_loop: &ActiveEventLoop);
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent);
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop);
    fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent);
}

/// アクティブな macOS イベントループ制御ハンドル
pub struct ActiveEventLoop {
    mtm: MainThreadMarker,
    window_delegate: Retained<TentWindowDelegate>,
    proxy: AppProxy,
    control_flow: Cell<ControlFlow>,
    exit_requested: Cell<bool>,
    window: RefCell<Option<Arc<NativeWindow>>>,
}

impl ActiveEventLoop {
    pub fn create_window(&self, attrs: WindowAttributes) -> Arc<NativeWindow> {
        let win = Arc::new(NativeWindow::new(
            self.mtm,
            attrs,
            &self.window_delegate,
            self.proxy.clone(),
        ));
        *self.window.borrow_mut() = Some(Arc::clone(&win));
        win
    }

    pub fn set_control_flow(&self, flow: ControlFlow) {
        self.control_flow.set(flow);
    }

    pub fn exit(&self) {
        self.exit_requested.set(true);
        let app = NSApplication::sharedApplication(self.mtm);
        app.terminate(None);
    }
}

enum PendingDispatch {
    Window(WindowEvent),
    DrainProxy,
    Redraw,
    TimerTick,
}

struct MainRuntime {
    handler: Box<dyn ApplicationHandler>,
    event_loop: ActiveEventLoop,
    proxy: AppProxy,
    wake_timer: Option<Retained<NSTimer>>,
    timer_target: Retained<TentTimerTarget>,
    in_dispatch: bool,
    pending: VecDeque<PendingDispatch>,
}

thread_local! {
    static RUNTIME: RefCell<Option<MainRuntime>> = const { RefCell::new(None) };
}

pub(crate) fn dispatch_window_event(event: WindowEvent) {
    dispatch_or_enqueue(PendingDispatch::Window(event));
}

pub(crate) fn schedule_redraw_on_main() {
    extern "C" fn on_redraw(_ctx: *mut c_void) {
        dispatch_or_enqueue(PendingDispatch::Redraw);
    }
    unsafe extern "C" {
        #[link_name = "_dispatch_main_q"]
        static DISPATCH_MAIN_Q: c_void;
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }
    unsafe {
        dispatch_async_f(
            &DISPATCH_MAIN_Q as *const c_void,
            std::ptr::null_mut(),
            on_redraw,
        );
    }
}

extern "C" fn on_proxy_wake(_ctx: *mut c_void) {
    dispatch_or_enqueue(PendingDispatch::DrainProxy);
}

fn dispatch_or_enqueue(item: PendingDispatch) {
    RUNTIME.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(rt) = borrow.as_mut() else {
            return;
        };
        rt.pending.push_back(item);
        if rt.in_dispatch {
            return;
        }
        rt.in_dispatch = true;

        while let Some(next) = rt.pending.pop_front() {
            if rt.event_loop.exit_requested.get() {
                break;
            }
            match next {
                PendingDispatch::Window(ev) => {
                    rt.handler.window_event(&rt.event_loop, ev);
                }
                PendingDispatch::DrainProxy => {
                    let events = rt.proxy.drain_events();
                    for ev in events {
                        if rt.event_loop.exit_requested.get() {
                            break;
                        }
                        rt.handler.user_event(&rt.event_loop, ev);
                    }
                }
                PendingDispatch::Redraw => {
                    let should_draw = rt
                        .event_loop
                        .window
                        .borrow()
                        .as_ref()
                        .is_some_and(|w| w.take_redraw_flag());
                    if should_draw {
                        rt.handler
                            .window_event(&rt.event_loop, WindowEvent::RedrawRequested);
                    }
                }
                PendingDispatch::TimerTick => {
                    rt.handler.about_to_wait(&rt.event_loop);
                }
            }
        }

        if !rt.event_loop.exit_requested.get() {
            rt.handler.about_to_wait(&rt.event_loop);
            reschedule_timer(rt);
        }
        rt.in_dispatch = false;
    });
}

fn reschedule_timer(rt: &mut MainRuntime) {
    if let Some(old) = rt.wake_timer.take() {
        old.invalidate();
    }
    if let ControlFlow::WaitUntil(deadline) = rt.event_loop.control_flow.get() {
        let now = Instant::now();
        let delay = deadline
            .checked_duration_since(now)
            .unwrap_or(Duration::from_millis(1))
            .max(Duration::from_millis(1));
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                delay.as_secs_f64(),
                &rt.timer_target,
                sel!(onTimer:),
                None,
                false,
            )
        };
        rt.wake_timer = Some(timer);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct TentTimerTarget;

    unsafe impl NSObjectProtocol for TentTimerTarget {}

    impl TentTimerTarget {
        #[unsafe(method(onTimer:))]
        fn on_timer(&self, _timer: Option<&NSTimer>) {
            dispatch_or_enqueue(PendingDispatch::TimerTick);
        }
    }
);

impl TentTimerTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct TentWindowDelegate;

    unsafe impl NSObjectProtocol for TentWindowDelegate {}

    impl TentWindowDelegate {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            dispatch_window_event(WindowEvent::Resized);
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn window_did_change_backing_properties(&self, _notification: &NSNotification) {
            dispatch_window_event(WindowEvent::ScaleFactorChanged);
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            dispatch_window_event(WindowEvent::Focused(true));
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            dispatch_window_event(WindowEvent::Focused(false));
        }

        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &AnyObject) -> Bool {
            dispatch_window_event(WindowEvent::CloseRequested);
            Bool::NO
        }
    }
);

impl TentWindowDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct TentAppDelegate;

    unsafe impl NSObjectProtocol for TentAppDelegate {}

    impl TentAppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            menu::install_main_menu(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);

            RUNTIME.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if let Some(rt) = borrow.as_mut() {
                    rt.in_dispatch = true;
                    rt.handler.resumed(&rt.event_loop);
                    rt.in_dispatch = false;
                }
            });
            dispatch_or_enqueue(PendingDispatch::TimerTick);
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: &NSNotification) {
            RUNTIME.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if let Some(rt) = borrow.as_mut() {
                    if let Some(timer) = rt.wake_timer.take() {
                        timer.invalidate();
                    }
                    rt.handler.exiting(&rt.event_loop);
                }
            });
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &NSApplication,
        ) -> Bool {
            Bool::YES
        }
    }
);

impl TentAppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// macOS ネイティブ AppKit イベントループを起動する
pub fn run_app<H: ApplicationHandler + 'static>(proxy: AppProxy, handler: H) {
    let mtm = MainThreadMarker::new().expect("TenT must run on the main thread");
    proxy.set_wake_callback(on_proxy_wake);

    let app = NSApplication::sharedApplication(mtm);
    let app_delegate = TentAppDelegate::new(mtm);
    let window_delegate = TentWindowDelegate::new(mtm);
    let timer_target = TentTimerTarget::new(mtm);

    let event_loop = ActiveEventLoop {
        mtm,
        window_delegate,
        proxy: proxy.clone(),
        control_flow: Cell::new(ControlFlow::Wait),
        exit_requested: Cell::new(false),
        window: RefCell::new(None),
    };

    RUNTIME.with(|cell| {
        *cell.borrow_mut() = Some(MainRuntime {
            handler: Box::new(handler),
            event_loop,
            proxy,
            wake_timer: None,
            timer_target,
            in_dispatch: false,
            pending: VecDeque::new(),
        });
    });

    unsafe {
        let _: () = msg_send![&app, setDelegate: &*app_delegate];
    }
    app.run();
}
