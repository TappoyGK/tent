use std::ffi::CStr;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

/// 指定 PID の物理メモリフットプリント（バイト）を macOS `proc_pid_rusage` で取得する。
/// アクティビティモニタの「メモリ」欄と同じ `ri_phys_footprint` を優先し、0 の場合は `ri_resident_size` を使う。
pub fn get_pid_memory_bytes(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let bytes = if info.ri_phys_footprint > 0 {
        info.ri_phys_footprint
    } else {
        info.ri_resident_size
    };
    (bytes > 0).then_some(bytes)
}

/// バイト数をステータスバー向けの短い表記（例: `185M`, `1.2G`）へ整形する
pub fn format_memory_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1}G", bytes as f64 / GIB)
    } else {
        format!("{}M", ((bytes as f64) / MIB).round() as u64)
    }
}

/// 指定 PID のカレントディレクトリを macOS libc::proc_pidinfo で高速取得する
pub fn get_pid_cwd(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let ret = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if ret <= 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let cstr = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    let s = cstr.to_str().ok()?;
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// 指定 PID の実行ファイルパスを macOS `proc_pidpath` で取得する
pub fn get_pid_exe_path(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    // macOS PROC_PIDPATHINFO_MAXSIZE = 4096
    let mut buf = [0u8; 4096];
    let ret = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if ret <= 0 {
        return None;
    }
    let len = (ret as usize).min(buf.len());
    let s = std::str::from_utf8(&buf[..len]).ok()?;
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// 指定 PID のコマンドライン引数（先頭数個）を macOS `KERN_PROCARGS2` で取得する
pub fn get_pid_cmdline_args(pid: i32) -> Vec<String> {
    if pid <= 0 {
        return Vec::new();
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size < 4 {
        return Vec::new();
    }
    let alloc_size = size.min(64 * 1024);
    let mut buf = vec![0u8; alloc_size];
    let mut actual_size = alloc_size;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &raw mut actual_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || actual_size < 4 {
        return Vec::new();
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    let data = &buf[4..actual_size];
    // exec_path (NUL 終端) をスキップし、後続の NUL パディングも飛ばす
    let Some(first_nul) = data.iter().position(|&b| b == 0) else {
        return Vec::new();
    };
    let mut pos = first_nul;
    while pos < data.len() && data[pos] == 0 {
        pos += 1;
    }
    let mut args = Vec::with_capacity(argc.min(8));
    for _ in 0..argc.min(8) {
        if pos >= data.len() {
            break;
        }
        let end = data[pos..]
            .iter()
            .position(|&b| b == 0)
            .map(|o| pos + o)
            .unwrap_or(data.len());
        if let Ok(s) = std::str::from_utf8(&data[pos..end]) {
            args.push(s.to_string());
        }
        pos = end + 1;
    }
    args
}

/// 実行ファイルパスと引数リストから監視対象 AI ツール（Claude Code / agy / Codex）を判別する。
/// 一般的なスクリプト引数（例: `node /Users/x/proj/claude` や `python3 -m http.server /srv/codex`）での
/// 誤検出を避けるため、実行ファイル名・ネイティブ版バージョンパス・`argv[0]`・`node_modules` 配下の公式パッケージのみを対象とする。
pub fn detect_ai_tool_from_process(
    exe_path: Option<&std::path::Path>,
    args: &[String],
) -> Option<crate::daemon::AiTool> {
    use crate::daemon::AiTool;

    let match_basename = |token: &str| -> Option<AiTool> {
        let base = std::path::Path::new(token)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(token)
            .to_ascii_lowercase();
        match base.as_str() {
            "claude" | "claude-code" => Some(AiTool::Claude),
            "agy" | "antigravity" | "antigravity-cli" => Some(AiTool::Agy),
            "codex" | "codex-cli" => Some(AiTool::Codex),
            _ => None,
        }
    };

    if let Some(p) = exe_path
        && let Some(s) = p.to_str()
    {
        if let Some(tool) = match_basename(s) {
            return Some(tool);
        }
        // Claude Code ネイティブ版は `~/.local/share/claude/versions/<version>` へのシンボリックリンク
        let lower = s.to_ascii_lowercase();
        if lower.contains("/claude/versions/") {
            return Some(AiTool::Claude);
        }
    }

    // シンボリックリンク起動（例: `~/.local/bin/claude -> .../versions/2.1.x`）を argv[0] で判別
    if let Some(argv0) = args.first()
        && let Some(tool) = match_basename(argv0)
    {
        return Some(tool);
    }

    // node / bun / deno 経由の起動時は node_modules 配下の公式パッケージパスのみを対象にする
    let is_js_runtime = exe_path
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .is_some_and(|name| matches!(name.to_ascii_lowercase().as_str(), "node" | "bun" | "deno"));
    if is_js_runtime {
        for arg in args.iter().skip(1).take(3) {
            let lower = arg.to_ascii_lowercase();
            if lower.contains("node_modules/@anthropic-ai/claude-code") {
                return Some(AiTool::Claude);
            }
            if lower.contains("node_modules/@openai/codex") {
                return Some(AiTool::Codex);
            }
        }
    }
    None
}

/// 指定 PID のプロセスが監視対象 AI ツールか判別する
pub fn detect_ai_tool_for_pid(pid: i32) -> Option<crate::daemon::AiTool> {
    let exe = get_pid_exe_path(pid);
    if let Some(tool) = detect_ai_tool_from_process(exe.as_deref(), &[]) {
        return Some(tool);
    }
    let args = get_pid_cmdline_args(pid);
    detect_ai_tool_from_process(exe.as_deref(), &args)
}

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

/// macOS クリップボードからテキストを取得する（Finder/Dock 起動時も UTF-8 保証）
pub fn read_clipboard_text() -> Option<String> {
    let output = Command::new("pbpaste")
        .env("LC_CTYPE", "UTF-8")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// ターミナルへの `Cmd+V` ペースト内容を決定する。
/// テキストが存在する場合はテキストを優先し、空または未取得のときだけクリップボード画像パスへフォールバックする。
pub fn resolve_terminal_paste_payload<F>(
    clipboard_text: Option<String>,
    save_clipboard_image: F,
) -> Option<String>
where
    F: FnOnce() -> Option<PathBuf>,
{
    if let Some(text) = clipboard_text
        && !text.is_empty()
    {
        return Some(text);
    }
    save_clipboard_image().map(|path| format!("{} ", shell_escape_path(&path)))
}

fn save_temp_image_bytes(prefix: &str, bytes: &[u8], ext: &str) -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let drop_dir = PathBuf::from("/tmp/tent-drops");
    std::fs::create_dir_all(&drop_dir).ok()?;
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = drop_dir.join(format!("{prefix}-{ms}-{seq}.{ext}"));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

#[cfg(target_os = "macos")]
unsafe fn extract_pasteboard_image_to_temp(
    pb: *mut objc2::runtime::AnyObject,
    prefix: &str,
) -> Option<PathBuf> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSData, NSDictionary, NSString};

    if pb.is_null() {
        return None;
    }

    let png_type = NSString::from_str("public.png");
    let png_data: Option<Retained<NSData>> = unsafe { msg_send![pb, dataForType: &*png_type] };
    if let Some(data) = png_data {
        let bytes = unsafe { data.as_bytes_unchecked() };
        if !bytes.is_empty()
            && let Some(path) = save_temp_image_bytes(prefix, bytes, "png")
        {
            return Some(path);
        }
    }

    let tiff_type = NSString::from_str("public.tiff");
    let tiff_data: Option<Retained<NSData>> = unsafe { msg_send![pb, dataForType: &*tiff_type] };
    if let Some(data) = tiff_data {
        if let Some(cls) = AnyClass::get(c"NSBitmapImageRep") {
            let rep: Option<Retained<AnyObject>> =
                unsafe { msg_send![cls, imageRepWithData: &*data] };
            if let Some(rep) = rep {
                let empty_props: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
                // NSBitmapImageFileTypePNG = 4
                let png: Option<Retained<NSData>> = unsafe {
                    msg_send![
                        &*rep,
                        representationUsingType: 4usize,
                        properties: &*empty_props
                    ]
                };
                if let Some(png) = png {
                    let bytes = unsafe { png.as_bytes_unchecked() };
                    if !bytes.is_empty()
                        && let Some(path) = save_temp_image_bytes(prefix, bytes, "png")
                    {
                        return Some(path);
                    }
                }
            }
        }
        let bytes = unsafe { data.as_bytes_unchecked() };
        if !bytes.is_empty() {
            return save_temp_image_bytes(prefix, bytes, "tiff");
        }
    }

    None
}

/// macOS クリップボードに画像（PNG / TIFF）が入っていれば `/tmp/tent-drops/` へ保存してパスを返す。
#[cfg(target_os = "macos")]
pub fn save_clipboard_image_to_temp() -> Option<PathBuf> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    unsafe {
        let cls = AnyClass::get(c"NSPasteboard")?;
        let pb: *mut AnyObject = msg_send![cls, generalPasteboard];
        extract_pasteboard_image_to_temp(pb, "tent-paste")
    }
}

#[cfg(not(target_os = "macos"))]
pub fn save_clipboard_image_to_temp() -> Option<PathBuf> {
    None
}

/// macOS クリップボードへテキストを書き込む（UTF-8 保証）
pub fn write_clipboard_text(text: &str) {
    if let Ok(mut child) = Command::new("pbcopy")
        .env("LC_CTYPE", "UTF-8")
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
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

/// macOS の Wi-Fi 電波状態
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiStatus {
    Disconnected,
    Connected { bars: u8, rssi: i32 },
}

/// `system_profiler SPAirPortDataType` から現在の Wi-Fi 電波強度を取得する
pub fn read_wifi_status() -> WifiStatus {
    let Ok(out) = Command::new("system_profiler")
        .arg("SPAirPortDataType")
        .output()
    else {
        return WifiStatus::Disconnected;
    };
    if !out.status.success() {
        return WifiStatus::Disconnected;
    }
    parse_airport_wifi(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_airport_wifi(text: &str) -> WifiStatus {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Signal / Noise:") {
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(rssi) = token.parse::<i32>() {
                let bars = if rssi >= -55 {
                    4
                } else if rssi >= -67 {
                    3
                } else if rssi >= -78 {
                    2
                } else {
                    1
                };
                return WifiStatus::Connected { bars, rssi };
            }
        }
    }
    WifiStatus::Disconnected
}

/// macOS のバッテリー状態
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
}

/// `pmset -g batt` を実行して現在のバッテリー残量と充電状態を取得する
pub fn read_battery_status() -> Option<BatteryStatus> {
    let out = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_pmset_batt(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_pmset_batt(text: &str) -> Option<BatteryStatus> {
    for line in text.lines() {
        let Some(pct_pos) = line.find('%') else {
            continue;
        };
        let before = &line[..pct_pos];
        let digits: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let Ok(percent) = digits.parse::<u8>() else {
            continue;
        };
        let lower = line.to_ascii_lowercase();
        let charging = lower.contains("charging;") && !lower.contains("discharging");
        return Some(BatteryStatus {
            percent: percent.min(100),
            charging,
        });
    }
    None
}

/// D&D されたファイルパスを zsh / bash / Codex / Claude Code でそのまま扱えるよう
/// バックスラッシュエスケープする（iTerm2 / Ghostty / Terminal.app 互換）
pub fn shell_escape_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len() + 8);
    for ch in raw.chars() {
        if matches!(
            ch,
            ' ' | '\t'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '&'
                | ';'
                | '\''
                | '"'
                | '\\'
                | '$'
                | '`'
                | '*'
                | '?'
                | '<'
                | '>'
                | '|'
                | '!'
                | '#'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// macOS の Dock / Cmd+Tab に TenT のカスタムアイコンを設定する（bare binary 起動時用）
#[cfg(target_os = "macos")]
pub fn install_app_icon() {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSData;

    const ICON_PNG: &[u8] = include_bytes!("../assets/tent-icon-256.png");
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

/// アプリ側で指定するカーソル種別（`WinitView::resetCursorRects` 差し替え用）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AppCursorKind {
    Default = 0,
    Pointer = 1,
    EwResize = 2,
    NsResize = 3,
}

#[cfg(target_os = "macos")]
static APP_CURSOR_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `WinitView::resetCursorRects` で使用するカーソル種別を更新する
pub fn set_app_cursor_kind(kind: AppCursorKind) {
    #[cfg(target_os = "macos")]
    APP_CURSOR_KIND.store(kind as u8, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_os = "macos"))]
    let _ = kind;
}

/// 2D 軸平行矩形 `bounds` から複数の除外矩形 `cuts` を差し引いた非重複矩形群を返す
/// （`WinitView::resetCursorRects` で表示中の `WKWebView` 領域を除外するため）
fn subtract_cursor_rects(
    bounds: (f64, f64, f64, f64),
    cuts: &[(f64, f64, f64, f64)],
) -> Vec<(f64, f64, f64, f64)> {
    let mut regions = vec![bounds];
    for &(cx, cy, cw, ch) in cuts {
        if cw <= 0.0 || ch <= 0.0 {
            continue;
        }
        let cx1 = cx + cw;
        let cy1 = cy + ch;
        let mut next = Vec::with_capacity(regions.len() + 3);
        for (rx, ry, rw, rh) in regions {
            let rx1 = rx + rw;
            let ry1 = ry + rh;
            let ix0 = rx.max(cx);
            let iy0 = ry.max(cy);
            let ix1 = rx1.min(cx1);
            let iy1 = ry1.min(cy1);
            if ix0 >= ix1 || iy0 >= iy1 {
                next.push((rx, ry, rw, rh));
                continue;
            }
            if ry1 > iy1 {
                next.push((rx, iy1, rw, ry1 - iy1));
            }
            if iy0 > ry {
                next.push((rx, ry, rw, iy0 - ry));
            }
            if ix0 > rx {
                next.push((rx, iy0, ix0 - rx, iy1 - iy0));
            }
            if rx1 > ix1 {
                next.push((ix1, iy0, rx1 - ix1, iy1 - iy0));
            }
        }
        regions = next;
    }
    regions
}

/// macOS の `WinitView::resetCursorRects` を差し替え、表示中の子 `WKWebView` 上には
/// 親ビューの `addCursorRect:` を張らないようにする（Web ページの CSS カーソル切替を阻害しないため）。
#[cfg(target_os = "macos")]
pub fn install_cursor_rect_passthrough(window: &winit::window::Window) {
    use std::ffi::c_char;

    use objc2::ffi::{class_replaceMethod, object_getClass};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_foundation::NSArray;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    unsafe extern "C" fn reset_cursor_rects_excluding_subviews(this: *mut AnyObject, _cmd: Sel) {
        if this.is_null() {
            return;
        }
        let bounds: CGRect = unsafe { msg_send![this, bounds] };
        let subviews: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![this, subviews] };
        let mut excluded = Vec::new();
        if let Some(subs) = subviews {
            for sub in subs.iter() {
                let hidden: Bool = unsafe { msg_send![&*sub, isHidden] };
                if hidden.as_bool() {
                    continue;
                }
                let frame: CGRect = unsafe { msg_send![&*sub, frame] };
                if frame.size.width > 0.0 && frame.size.height > 0.0 {
                    excluded.push((
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    ));
                }
            }
        }

        // 注: winit の resetCursorRects を直接差し替えるため、TenT が使用する
        // Default / Pointer / EwResize / NsResize を APP_CURSOR_KIND で反映している。
        // 今後ほかのカーソルアイコンや set_cursor_visible(false) を使う場合はここも合わせて拡張する。
        let Some(cursor_cls) = AnyClass::get(c"NSCursor") else {
            return;
        };
        let cursor: *mut AnyObject =
            match APP_CURSOR_KIND.load(std::sync::atomic::Ordering::Relaxed) {
                1 => unsafe { msg_send![cursor_cls, pointingHandCursor] },
                2 => unsafe { msg_send![cursor_cls, resizeLeftRightCursor] },
                3 => unsafe { msg_send![cursor_cls, resizeUpDownCursor] },
                _ => unsafe { msg_send![cursor_cls, arrowCursor] },
            };
        if cursor.is_null() {
            return;
        }

        let regions = subtract_cursor_rects(
            (
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
            ),
            &excluded,
        );
        for (x, y, w, h) in regions {
            let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
            let _: () = unsafe { msg_send![this, addCursorRect: rect, cursor: cursor] };
        }
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
        let view_cls = object_getClass(view.cast()).cast_mut();
        if !view_cls.is_null() {
            let imp: Imp = std::mem::transmute::<unsafe extern "C" fn(*mut AnyObject, Sel), Imp>(
                reset_cursor_rects_excluding_subviews,
            );
            let _ = class_replaceMethod(
                view_cls,
                sel!(resetCursorRects),
                imp,
                c"v@:".as_ptr().cast::<c_char>(),
            );
        }
        let ns_win: *mut AnyObject = msg_send![view, window];
        if !ns_win.is_null() {
            let _: () = msg_send![ns_win, invalidateCursorRectsForView: view];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_cursor_rect_passthrough(_window: &winit::window::Window) {}

/// 子 `WKWebView` の表示・位置変化後に `WinitView` のカーソル矩形を再計算させる
#[cfg(target_os = "macos")]
pub fn invalidate_cursor_rects(window: &winit::window::Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

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
        let ns_win: *mut AnyObject = msg_send![view, window];
        if !ns_win.is_null() {
            let _: () = msg_send![ns_win, invalidateCursorRectsForView: view];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn invalidate_cursor_rects(_window: &winit::window::Window) {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_current_process_cwd() {
        let pid = std::process::id() as i32;
        let cwd = get_pid_cwd(pid).expect("proc_pidinfo should return cwd for current process");
        let expected = std::env::current_dir().unwrap();
        assert_eq!(
            cwd.canonicalize().unwrap_or(cwd),
            expected.canonicalize().unwrap_or(expected)
        );
    }

    #[test]
    fn test_shell_escape_path() {
        assert_eq!(
            shell_escape_path(std::path::Path::new(
                "/Users/tappoy/Desktop/Screenshot 2026-11-26 at 09.55.00.png"
            )),
            r"/Users/tappoy/Desktop/Screenshot\ 2026-11-26\ at\ 09.55.00.png"
        );
        assert_eq!(
            shell_escape_path(std::path::Path::new("/tmp/スクショ (1).png")),
            r"/tmp/スクショ\ \(1\).png"
        );
    }

    #[test]
    fn test_parse_pmset_batt() {
        let discharging = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t84%; discharging; 5:12 remaining present: true\n";
        assert_eq!(
            parse_pmset_batt(discharging),
            Some(BatteryStatus {
                percent: 84,
                charging: false
            })
        );
        let charging = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t76%; charging; 0:45 remaining present: true\n";
        assert_eq!(
            parse_pmset_batt(charging),
            Some(BatteryStatus {
                percent: 76,
                charging: true
            })
        );
    }

    #[test]
    fn test_parse_airport_wifi() {
        let strong = "          Status: Connected\n          Signal / Noise: -52 dBm / -92 dBm\n";
        assert_eq!(
            parse_airport_wifi(strong),
            WifiStatus::Connected { bars: 4, rssi: -52 }
        );
        let fair = "          Signal / Noise: -72 dBm / -90 dBm\n";
        assert_eq!(
            parse_airport_wifi(fair),
            WifiStatus::Connected { bars: 2, rssi: -72 }
        );
        assert_eq!(
            parse_airport_wifi("Status: Off\n"),
            WifiStatus::Disconnected
        );
    }

    #[test]
    fn test_resolve_terminal_paste_payload() {
        // テキストがあるときは画像フォールバックを呼ばずテキストを優先する
        let mut called = false;
        let out = resolve_terminal_paste_payload(Some("hello".to_string()), || {
            called = true;
            Some(PathBuf::from("/tmp/tent-drops/a.png"))
        });
        assert_eq!(out.as_deref(), Some("hello"));
        assert!(!called);

        // pbpaste が空文字を返したとき（画像のみコピー時）はエスケープ済みパス＋末尾スペースへフォールバック
        let out = resolve_terminal_paste_payload(Some(String::new()), || {
            Some(PathBuf::from("/tmp/tent-drops/shot (1).png"))
        });
        assert_eq!(out.as_deref(), Some(r"/tmp/tent-drops/shot\ \(1\).png "));

        // テキスト未取得かつ画像あり
        let out = resolve_terminal_paste_payload(None, || {
            Some(PathBuf::from("/tmp/tent-drops/paste.png"))
        });
        assert_eq!(out.as_deref(), Some("/tmp/tent-drops/paste.png "));

        // どちらも空
        let out = resolve_terminal_paste_payload(Some(String::new()), || None);
        assert_eq!(out, None);
    }

    #[test]
    fn test_subtract_cursor_rects() {
        let bounds = (0.0, 0.0, 1000.0, 800.0);
        // ブラウザペイン（右半分）を除外したとき、左半分の 1 矩形だけが残ること
        let res = subtract_cursor_rects(bounds, &[(500.0, 0.0, 500.0, 800.0)]);
        assert_eq!(res, vec![(0.0, 0.0, 500.0, 800.0)]);

        // 中央の矩形を除外すると上下左右の 4 矩形へ分割され、総面積が一致すること
        let res = subtract_cursor_rects(bounds, &[(200.0, 100.0, 600.0, 500.0)]);
        assert_eq!(res.len(), 4);
        let area: f64 = res.iter().map(|(_, _, w, h)| w * h).sum();
        assert!((area - (1000.0 * 800.0 - 600.0 * 500.0)).abs() < 1e-6);

        // 除外なしなら元の矩形のまま
        assert_eq!(subtract_cursor_rects(bounds, &[]), vec![bounds]);
    }
}
