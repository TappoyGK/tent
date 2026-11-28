use std::path::PathBuf;

use crate::daemon::{self, default_socket_path};
use crate::terminal_pane::PaneId;

#[derive(Debug, PartialEq, Eq)]
enum TopLevelJsonValue {
    Str(String),
    Bool(bool),
    Null,
    Other,
}

fn skip_json_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// JSON 文字列リテラル（開始の `"` 位置から）を読み取り、デコード済み文字列と終了の次のバイト位置を返す
fn parse_json_string(input: &str, start: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                i += 1;
                let esc = *bytes.get(i)?;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = input.get(i + 1..i + 5)?;
                        let code = u16::from_str_radix(hex, 16).ok()?;
                        if let Some(ch) = char::from_u32(u32::from(code)) {
                            out.push(ch);
                        }
                        i += 4;
                    }
                    _ => return None,
                }
                i += 1;
            }
            _ => {
                let ch = input[i..].chars().next()?;
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    None
}

/// ネストされた JSON オブジェクトまたは配列（開始の `{` / `[` 位置から）を対応する閉じ括弧の次までスキップする
fn skip_json_nested(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let (_, next) = parse_json_string(input, i)?;
                i = next;
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// JSON オブジェクトのトップレベルフィールドのみを解析する軽量パーサ（ネスト内の同名キーは拾わない）
fn parse_top_level_json_object(input: &str) -> Vec<(String, TopLevelJsonValue)> {
    let bytes = input.as_bytes();
    let mut i = skip_json_ws(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return Vec::new();
    }
    i = skip_json_ws(bytes, i + 1);
    let mut fields = Vec::new();
    if bytes.get(i) == Some(&b'}') {
        return fields;
    }

    while i < bytes.len() {
        let Some((key, next)) = parse_json_string(input, i) else {
            break;
        };
        i = skip_json_ws(bytes, next);
        if bytes.get(i) != Some(&b':') {
            break;
        }
        i = skip_json_ws(bytes, i + 1);

        let val = match bytes.get(i).copied() {
            Some(b'"') => {
                let Some((s, next)) = parse_json_string(input, i) else {
                    break;
                };
                i = next;
                TopLevelJsonValue::Str(s)
            }
            Some(b'{') | Some(b'[') => {
                let Some(next) = skip_json_nested(input, i) else {
                    break;
                };
                i = next;
                TopLevelJsonValue::Other
            }
            Some(_) => {
                let val_start = i;
                while i < bytes.len()
                    && !matches!(bytes[i], b',' | b'}' | b' ' | b'\t' | b'\r' | b'\n')
                {
                    i += 1;
                }
                match &input[val_start..i] {
                    "true" => TopLevelJsonValue::Bool(true),
                    "false" => TopLevelJsonValue::Bool(false),
                    "null" => TopLevelJsonValue::Null,
                    _ => TopLevelJsonValue::Other,
                }
            }
            None => break,
        };
        fields.push((key, val));

        i = skip_json_ws(bytes, i);
        match bytes.get(i).copied() {
            Some(b',') => i = skip_json_ws(bytes, i + 1),
            Some(b'}') => break,
            _ => break,
        }
    }
    fields
}

fn json_top_str<'a>(fields: &'a [(String, TopLevelJsonValue)], key: &str) -> Option<&'a str> {
    fields.iter().find_map(|(k, v)| match v {
        TopLevelJsonValue::Str(s) if k == key => Some(s.as_str()),
        _ => None,
    })
}

fn json_top_bool(fields: &[(String, TopLevelJsonValue)], key: &str) -> Option<bool> {
    fields.iter().find_map(|(k, v)| match v {
        TopLevelJsonValue::Bool(b) if k == key => Some(*b),
        _ => None,
    })
}

fn agy_hook_stdout_json(event_override: Option<&str>, stdin_payload: &str) -> &'static str {
    // agy の PreToolUse 契約は decision フィールドを必須とするため、PreToolUse 時は {"decision":"allow"} を返す
    let is_pre_tool_use = event_override.is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "pre-tool-use" | "pretooluse"
        )
    }) || {
        let fields = parse_top_level_json_object(stdin_payload);
        json_top_str(&fields, "hookEventName") == Some("PreToolUse")
    };
    if is_pre_tool_use {
        "{\"decision\":\"allow\"}"
    } else {
        "{}"
    }
}

fn resolve_hook_state_from_stdin(
    base_state: daemon::AiHookState,
    tool: Option<daemon::AiTool>,
    stdin_payload: &str,
) -> Option<daemon::AiHookState> {
    let fields = parse_top_level_json_object(stdin_payload);

    // agy の Stop フックはユーザー自身が Ctrl+C 等でキャンセルした場合にも発火するため、
    // terminationReason が USER_CANCELED なら fullyIdle の値にかかわらず Idle としてクリアする
    if base_state == daemon::AiHookState::Waiting
        && tool == Some(daemon::AiTool::Agy)
        && json_top_str(&fields, "terminationReason") == Some("USER_CANCELED")
    {
        return Some(daemon::AiHookState::Idle);
    }

    // Claude Code / Codex のサブエージェント専用イベント（SubagentStop / SubagentStart）は無視する
    let hook_event_name = json_top_str(&fields, "hook_event_name");
    if matches!(hook_event_name, Some("SubagentStop" | "SubagentStart")) {
        return None;
    }

    // Claude Code / Codex でトップレベルに非空の agent_id があるサブエージェント由来イベントは、
    // ユーザー許可待ち（PermissionRequest / Notification）と作業中（Working）は親ペインへ通し、
    // サブエージェントの完了／終了（Stop / StopFailure / Idle 等）のみ無視する
    if matches!(tool, Some(daemon::AiTool::Claude | daemon::AiTool::Codex))
        && json_top_str(&fields, "agent_id").is_some_and(|id| !id.is_empty())
    {
        let is_user_prompt = matches!(hook_event_name, Some("PermissionRequest" | "Notification"));
        if !is_user_prompt && base_state != daemon::AiHookState::Working {
            return None;
        }
    }

    // agy の Stop フックでバックグラウンドタスクがまだ残っている（fullyIdle: false）場合は
    // 入力待ち（黄）にせず作業中（緑）のまま維持する
    if base_state == daemon::AiHookState::Waiting
        && tool == Some(daemon::AiTool::Agy)
        && json_top_bool(&fields, "fullyIdle") == Some(false)
    {
        return None;
    }

    Some(base_state)
}

/// AI フックから呼び出される軽量 CLI (`tent hook <working|waiting|idle> [--tool claude|agy|codex] [--event pre-tool-use]`)。
/// TenT 外で実行された場合やデーモン未接続時は何もせず正常終了（exit 0）し、AI の動作を絶対に妨げない。
pub(crate) fn run_hook_cli(mut args: impl Iterator<Item = String>) {
    use std::io::{IsTerminal, Read};

    let state_arg = args.next();
    let base_state = state_arg
        .as_deref()
        .and_then(daemon::AiHookState::parse_name);
    let mut tool: Option<daemon::AiTool> = None;
    let mut event_override: Option<String> = None;
    let mut pane_id_override: Option<PaneId> = None;
    let mut sock_override: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tool" => {
                if let Some(val) = args.next() {
                    tool = daemon::AiTool::parse_name(&val);
                }
            }
            "--event" => {
                event_override = args.next();
            }
            "--pane-id" => {
                if let Some(val) = args.next() {
                    pane_id_override = val.parse::<PaneId>().ok();
                }
            }
            "--socket" => {
                if let Some(val) = args.next() {
                    sock_override = Some(PathBuf::from(val));
                }
            }
            _ => {}
        }
    }

    // パイプ経由で渡されたフック JSON を読み込む（TTY からの手動実行時はブロックしない）
    let mut stdin_payload = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin()
            .take(64 * 1024)
            .read_to_string(&mut stdin_payload);
    }

    if let Some(base_state) = base_state
        && let Some(state) = resolve_hook_state_from_stdin(base_state, tool, &stdin_payload)
    {
        let pane_id = pane_id_override.or_else(|| {
            std::env::var("TENT_PANE_ID")
                .ok()
                .and_then(|s| s.parse::<PaneId>().ok())
        });
        if let Some(pane_id) = pane_id {
            let sock_path = sock_override
                .or_else(|| std::env::var_os("TENT_SOCKET").map(PathBuf::from))
                .unwrap_or_else(default_socket_path);
            let _ = daemon::send_hook_event(&sock_path, pane_id, state, tool);
        }
    }

    // agy のフック契約は stdout に有効な JSON オブジェクトを返すことを要求する（不正引数時も含む）
    if tool == Some(daemon::AiTool::Agy) {
        println!(
            "{}",
            agy_hook_stdout_json(event_override.as_deref(), &stdin_payload)
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::daemon::{AiHookState, AiTool};
    use crate::macos;

    #[test]
    fn test_hook_cli_helpers_and_ai_process_detection() {
        // agy の Stop フックは USER_CANCELED なら fullyIdle に関わらず Idle、
        // それ以外で fullyIdle: false なら無視（None）、fullyIdle: true なら Waiting
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Agy),
                r#"{"terminationReason":"USER_CANCELED","fullyIdle":false}"#,
            ),
            Some(AiHookState::Idle)
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Agy),
                r#"{"terminationReason":"NO_TOOL_CALL","fullyIdle":true}"#,
            ),
            Some(AiHookState::Waiting)
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Agy),
                r#"{"terminationReason":"NO_TOOL_CALL","fullyIdle": false}"#,
            ),
            None
        );

        // Claude Code / Codex の SubagentStop / SubagentStart は無視（None）
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"SubagentStop","agent_id":"sub-123"}"#,
            ),
            None
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Codex),
                r#"{"hook_event_name":"SubagentStop","agent_id":"agent-9"}"#,
            ),
            None
        );
        // サブエージェントの許可待ち（PermissionRequest / Notification）は黄色（Waiting）として通す
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"PermissionRequest","agent_id":"a1b2","agent_type":"general-purpose","tool_name":"Bash"}"#,
            ),
            Some(AiHookState::Waiting)
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"Notification","agent_id":"a1b2"}"#,
            ),
            Some(AiHookState::Waiting)
        );
        // サブエージェントの PostToolUse（working）はそのまま作業中として通す
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Working,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"PostToolUse","agent_id":"a1b2","tool_name":"Bash"}"#,
            ),
            Some(AiHookState::Working)
        );
        // Claude Code の stop_hook_active: true は「継続後の最終 Stop」なので Waiting として通す
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"Stop","stop_hook_active":true,"last_assistant_message":"done"}"#,
            ),
            Some(AiHookState::Waiting)
        );
        // agent_id が null の場合はメインエージェント扱いとして通る
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"Stop","agent_id": null}"#,
            ),
            Some(AiHookState::Waiting)
        );
        // ネスト内の同名キー・文字列（tool_input / tool_response / meta）には誤反応しないこと
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"PermissionRequest","tool_name":"mcp__x__call","tool_input":{"agent_id":"bot-42"}}"#,
            ),
            Some(AiHookState::Waiting)
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Working,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"PostToolUse","tool_name":"Agent","tool_response":{"status":"completed","agentId":"a9f"}}"#,
            ),
            Some(AiHookState::Working)
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Claude),
                r#"{"hook_event_name":"Stop","tool_input":{"event":"SubagentStop"}}"#,
            ),
            Some(AiHookState::Waiting)
        );
        assert_eq!(
            resolve_hook_state_from_stdin(
                AiHookState::Waiting,
                Some(AiTool::Agy),
                r#"{"meta":{"fullyIdle":false},"fullyIdle":true}"#,
            ),
            Some(AiHookState::Waiting)
        );

        // agy の PreToolUse だけは {"decision":"allow"} を返す（明示 --event または stdin）
        assert_eq!(
            agy_hook_stdout_json(Some("pre-tool-use"), ""),
            r#"{"decision":"allow"}"#
        );
        assert_eq!(
            agy_hook_stdout_json(
                None,
                r#"{"hookEventName":"PreToolUse","toolCall":{"name":"ask_question"}}"#,
            ),
            r#"{"decision":"allow"}"#
        );
        assert_eq!(
            agy_hook_stdout_json(None, r#"{"hookEventName":"Stop"}"#),
            "{}"
        );

        // プロセス名・ネイティブ版バージョンパス・コマンドラインからの AI 3 ツール判別
        assert_eq!(
            macos::detect_ai_tool_from_process(Some(Path::new("/Users/me/.local/bin/agy")), &[]),
            Some(AiTool::Agy)
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(Some(Path::new("/opt/homebrew/bin/codex")), &[]),
            Some(AiTool::Codex)
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(
                Some(Path::new("/Users/x/.local/share/claude/versions/2.1.332")),
                &[],
            ),
            Some(AiTool::Claude)
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(
                Some(Path::new("/bin/sleep")),
                &["/Users/x/.local/bin/claude".to_string(), "5".to_string()],
            ),
            Some(AiTool::Claude)
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(
                Some(Path::new("/opt/homebrew/bin/node")),
                &[
                    "node".to_string(),
                    "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js".to_string(),
                ],
            ),
            Some(AiTool::Claude)
        );
        // 誤検出しないこと（一般スクリプトや別プロジェクトパス）
        assert_eq!(
            macos::detect_ai_tool_from_process(
                Some(Path::new("/opt/homebrew/bin/node")),
                &["node".to_string(), "/Users/x/proj/claude".to_string()],
            ),
            None
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(
                Some(Path::new("/usr/bin/python3")),
                &[
                    "python3".to_string(),
                    "-m".to_string(),
                    "http.server".to_string(),
                    "/srv/codex".to_string(),
                ],
            ),
            None
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(
                Some(Path::new("/Users/x/src/claude-code/target/debug/mytool")),
                &[],
            ),
            None
        );
        assert_eq!(
            macos::detect_ai_tool_from_process(Some(Path::new("/bin/zsh")), &[]),
            None
        );
    }
}
