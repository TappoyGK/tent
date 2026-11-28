mod clipboard;
mod cursor_rects;
mod file_drop;
mod process;
mod system_status;

use std::path::PathBuf;
use std::process::Command;
use std::thread;

pub use clipboard::{
    read_clipboard_text, resolve_terminal_paste_payload, save_clipboard_image_to_temp,
    shell_escape_path, write_clipboard_text,
};
pub use cursor_rects::{AppCursorKind, subtract_cursor_rects};
pub use file_drop::install_file_drop_handler;
#[cfg(test)]
pub use process::detect_ai_tool_from_process;
pub use process::{detect_ai_tool_for_pid, format_memory_bytes, get_pid_cwd, get_pid_memory_bytes};
pub use system_status::{BatteryStatus, WifiStatus, read_battery_status, read_wifi_status};

/// macOS ネイティブ通知をバックグラウンドスレッドで非同期送信する
pub fn send_notification(title: String, body: String) {
    thread::spawn(move || {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            esc(&body),
            esc(&title)
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    });
}

/// macOS 標準のシステムベル音（Glass）を非同期再生する
pub fn play_system_bell() {
    thread::spawn(|| {
        let _ = Command::new("afplay")
            .arg("/System/Library/Sounds/Glass.aiff")
            .status();
    });
}

/// http:// または https:// の URL を macOS のデフォルトブラウザで非同期に開く
pub fn open_url(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    let url = url.to_string();
    thread::spawn(move || {
        let _ = Command::new("open").arg(&url).status();
    });
}

/// パニックログの出力先（~/Library/Logs/TenT/panic.log）
pub fn panic_log_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("TenT")
            .join("panic.log")
    } else {
        PathBuf::from("/tmp/tent-panic.log")
    }
}

/// macOS の Dock / Cmd+Tab に TenT のカスタムアイコンを設定する（bare binary 起動時用）
#[cfg(target_os = "macos")]
pub fn install_app_icon() {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSData;

    const ICON_PNG: &[u8] = include_bytes!("../../assets/tent-icon-256.png");
    let data = NSData::with_bytes(ICON_PNG);
    unsafe {
        let Some(app_cls) = AnyClass::get(c"NSApplication") else {
            return;
        };
        let Some(img_cls) = AnyClass::get(c"NSImage") else {
            return;
        };
        let app: *mut AnyObject = msg_send![app_cls, sharedApplication];
        if app.is_null() {
            return;
        }
        let alloc: *mut AnyObject = msg_send![img_cls, alloc];
        if alloc.is_null() {
            return;
        }
        let raw_img: *mut AnyObject = msg_send![alloc, initWithData: &*data];
        if let Some(img) = Retained::from_raw(raw_img) {
            let _: () = msg_send![app, setApplicationIconImage: &*img];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_app_icon() {}
