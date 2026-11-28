use crate::terminal_pane::PaneId;

pub const DEFAULT_BROWSER_URL: &str = "https://www.google.com";

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

/// JS 文字列リテラル（二重引用符内）へ安全に埋め込むためのエスケープ
pub fn escape_js_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
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
    }

    #[test]
    fn test_normalize_url_input_extra() {
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
}
