use std::path::Path;
use std::process::Command;

fn git_stdout(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn emit_git_rerun_hints() {
    let head_path = Path::new(".git/HEAD");
    if !head_path.exists() {
        return;
    }
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(head_path)
        && let Some(ref_rel) = head.trim().strip_prefix("ref: ")
    {
        let ref_path = Path::new(".git").join(ref_rel);
        if ref_path.exists() {
            println!("cargo:rerun-if-changed={}", ref_path.display());
        }
    }
    if Path::new(".git/packed-refs").exists() {
        println!("cargo:rerun-if-changed=.git/packed-refs");
    }
}

fn main() {
    emit_git_rerun_hints();

    let pkg_ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let expected_tag = format!("v{pkg_ver}");

    let is_dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|o| o.status.success() && !o.stdout.is_empty());

    let exact_tag = git_stdout(&["describe", "--tags", "--exact-match", "HEAD"]);
    let display_version = if !is_dirty && exact_tag.as_deref() == Some(expected_tag.as_str()) {
        expected_tag
    } else if let Some(hash) = git_stdout(&["rev-parse", "--short=7", "HEAD"]) {
        let dirty_mark = if is_dirty { "*" } else { "" };
        format!("v{pkg_ver}@{hash}{dirty_mark}")
    } else {
        expected_tag
    };

    println!("cargo:rustc-env=TENT_DISPLAY_VERSION={display_version}");
}
