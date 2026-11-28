mod menu;
mod proxy;
pub mod types;
mod url_field;
mod window;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
};
use objc2_core_foundation::CGPoint;
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSRunLoop, NSRunLoopCommonModes, NSTimer,
};

pub use proxy::AppProxy;
pub use types::{
    ControlFlow, ElementState, Ime, Key, KeyCode, KeyEvent, LogicalPosition, LogicalSize,
    ModifiersState, MouseButton, MouseScrollDelta, NamedKey, PhysicalKey, PhysicalPosition,
    PhysicalSize, WindowEvent,
};
pub use url_field::is_url_field_or_editor;
pub use window::{NativeWindow, SurfaceTarget, WindowAttributes};

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
    window: RefCell<Option<Rc<NativeWindow>>>,
}

impl ActiveEventLoop {
    pub fn create_window(&self, attrs: WindowAttributes) -> Rc<NativeWindow> {
        let win = Rc::new(NativeWindow::new(
            self.mtm,
            attrs,
            &self.window_delegate,
            self.proxy.clone(),
        ));
        *self.window.borrow_mut() = Some(Rc::clone(&win));
        win
    }

    pub fn set_control_flow(&self, flow: ControlFlow) {
        self.control_flow.set(flow);
    }

    /// 終了フラグのみを立てる（実際の `exiting()` と `NSApp.terminate` は借用解放後に実行する）
    pub fn exit(&self) {
        self.exit_requested.set(true);
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
}

// 再入ガード・保留キュー・終了状態は RUNTIME の外へ分離し、
// AppKit 同期コールバック再入時に RUNTIME.borrow_mut() が二重借用でパニックしないようにする。
thread_local! {
    static RUNTIME: RefCell<Option<MainRuntime>> = const { RefCell::new(None) };
    static IN_DISPATCH: Cell<bool> = const { Cell::new(false) };
    static PENDING: RefCell<VecDeque<PendingDispatch>> = const { RefCell::new(VecDeque::new()) };
    static EXITED: Cell<bool> = const { Cell::new(false) };
    static PANIC_PAYLOAD: RefCell<Option<Box<dyn Any + Send>>> = const { RefCell::new(None) };
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

fn run_exiting_once() {
    if EXITED.get() {
        return;
    }
    RUNTIME.with(|cell| {
        if let Ok(mut borrow) = cell.try_borrow_mut()
            && let Some(rt) = borrow.as_mut()
        {
            EXITED.set(true);
            if let Some(timer) = rt.wake_timer.take() {
                timer.invalidate();
            }
            let prev = IN_DISPATCH.replace(true);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                rt.handler.exiting(&rt.event_loop);
            }));
            IN_DISPATCH.set(prev);
        }
    });
}

fn schedule_stop_on_main() {
    extern "C" fn on_stop(_ctx: *mut c_void) {
        if let Some(mtm) = MainThreadMarker::new() {
            stop_app_run_loop(mtm);
        }
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
            on_stop,
        );
    }
}

fn stop_app_run_loop(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    // app.stop() は次のイベント処理後に app.run() を抜けるため、ダミーイベントを 1 件積んで即座に復帰させる
    if let Some(ev) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        CGPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    ) {
        app.postEvent_atStart(&ev, true);
    }
}

fn finish_if_exiting_or_panicked(defer_stop: bool) {
    let has_panic = PANIC_PAYLOAD.with(|p| p.borrow().is_some());
    let should_exit = RUNTIME.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|b| b.as_ref().map(|rt| rt.event_loop.exit_requested.get()))
            .unwrap_or(false)
    });

    if has_panic {
        // パニック時は中途半端な状態で state.conf を上書きしないため exiting() は呼ばずに即停止する
        if defer_stop {
            schedule_stop_on_main();
        } else if let Some(mtm) = MainThreadMarker::new() {
            stop_app_run_loop(mtm);
        }
    } else if should_exit {
        run_exiting_once();
        if defer_stop {
            schedule_stop_on_main();
        } else if let Some(mtm) = MainThreadMarker::new() {
            stop_app_run_loop(mtm);
        }
    }
}

fn dispatch_or_enqueue(item: PendingDispatch) {
    if PANIC_PAYLOAD.with(|p| p.borrow().is_some()) {
        return;
    }
    PENDING.with(|q| q.borrow_mut().push_back(item));
    if IN_DISPATCH.get() {
        return;
    }

    IN_DISPATCH.set(true);
    let result = catch_unwind(AssertUnwindSafe(|| {
        RUNTIME.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(rt) = borrow.as_mut() else {
                return;
            };

            loop {
                if rt.event_loop.exit_requested.get() {
                    break;
                }
                let next = PENDING.with(|q| q.borrow_mut().pop_front());
                let Some(next) = next else {
                    break;
                };
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
        });
    }));
    IN_DISPATCH.set(false);

    if let Err(payload) = result {
        PANIC_PAYLOAD.with(|p| {
            let mut slot = p.borrow_mut();
            if slot.is_none() {
                *slot = Some(payload);
            }
        });
    }
    finish_if_exiting_or_panicked(false);
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
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                delay.as_secs_f64(),
                &rt.timer_target,
                sel!(onTimer:),
                None,
                false,
            )
        };
        // メニュー展開中やライブリサイズ中もタイマーが止まらないよう CommonModes へ登録する
        let run_loop = NSRunLoop::mainRunLoop();
        unsafe {
            run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
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
            dispatch_window_event(WindowEvent::ModifiersChanged(
                crate::browser::current_macos_modifiers(),
            ));
            dispatch_window_event(WindowEvent::Focused(true));
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            dispatch_window_event(WindowEvent::ModifiersChanged(ModifiersState::empty()));
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

            IN_DISPATCH.set(true);
            let result = catch_unwind(AssertUnwindSafe(|| {
                RUNTIME.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    if let Some(rt) = borrow.as_mut() {
                        rt.handler.resumed(&rt.event_loop);
                    }
                });
            }));
            IN_DISPATCH.set(false);

            if let Err(payload) = result {
                PANIC_PAYLOAD.with(|p| {
                    let mut slot = p.borrow_mut();
                    if slot.is_none() {
                        *slot = Some(payload);
                    }
                });
                // applicationDidFinishLaunching: 時点では app.run() のメインループ開始前のため
                // 次の RunLoop 周回で stop + ダミーイベントを実行する
                finish_if_exiting_or_panicked(true);
                return;
            }
            dispatch_or_enqueue(PendingDispatch::TimerTick);
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: &NSNotification) {
            run_exiting_once();
            // NSApp.terminate: が C の exit(0) を呼ぶ前に RUNTIME（wgpu::Queue 等）を明示破棄し、
            // TLS 解体順による wgpu_hal の panic_access_error (SIGABRT) を防ぐ
            RUNTIME.with(|cell| {
                if let Ok(mut borrow) = cell.try_borrow_mut() {
                    *borrow = None;
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

    EXITED.set(false);
    RUNTIME.with(|cell| {
        *cell.borrow_mut() = Some(MainRuntime {
            handler: Box::new(handler),
            event_loop,
            proxy,
            wake_timer: None,
            timer_target,
        });
    });

    unsafe {
        let _: () = msg_send![&app, setDelegate: &*app_delegate];
    }
    app.run();

    // app.run() 復帰後に RUNTIME（wgpu / NSWindow 等）を全 TLS が生存しているうちに明示破棄する
    let has_panic = PANIC_PAYLOAD.with(|p| p.borrow().is_some());
    if !has_panic {
        run_exiting_once();
    }
    RUNTIME.with(|cell| {
        if let Ok(mut borrow) = cell.try_borrow_mut() {
            *borrow = None;
        }
    });

    if let Some(payload) = PANIC_PAYLOAD.with(|p| p.borrow_mut().take()) {
        resume_unwind(payload);
    }
}
