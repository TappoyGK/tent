use std::ffi::CStr;
use std::path::PathBuf;

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
}
