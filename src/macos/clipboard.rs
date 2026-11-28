use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
pub(super) unsafe fn extract_pasteboard_image_to_temp(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
