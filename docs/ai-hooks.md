# AI エージェント監視フック設定ガイド

TenT は **Claude Code / agy / Codex** の 3 ツールをフォアグラウンドプロセスから自動判別します。  
判別対象は、実行ファイル名（`claude` / `claude-code` / `agy` / `antigravity` / `antigravity-cli` / `codex` / `codex-cli`）、Claude Code ネイティブ版の `/claude/versions/` パス、`argv[0]` のベース名、および `node` / `bun` / `deno` 経由で起動された公式パッケージパス（`node_modules/@anthropic-ai/claude-code`・`node_modules/@openai/codex`）です。別名や独自ラッパー経由で起動した場合は判別されず一般ルールのまま動作します。

フックを設定した環境では、セッション開始（`SessionStart`）または最初のフック受信から AI フック駆動モードに入り、PTY 出力による画面差分・アイドル推測を完全に停止して、各ツールのライフサイクルフック（`tent hook`）のみで LED（緑／黄）・タイムスタンプ・OS 通知を駆動します。

- **フック未設定の環境**: フックを 1 度も受信していないペインでは従来の一般ルール（可視テキスト変化＋OSC 133）がそのまま動作します。
- **サブエージェント／バックグラウンドタスクの扱い**: `tent hook` はサブエージェント由来のペイロード（`SubagentStop` / `agent_id` / `parentConversationId` 等）や `agy` の `"fullyIdle": false`、Claude Code の `"stop_hook_active": true` を自動無視するため、サブエージェント完了で親ペインが誤って入力待ち（黄）になることはありません。
- **TenT 外で起動した場合**: `TENT_PANE_ID` 環境変数が未設定のため、`tent hook` は何もせず即座に正常終了（exit 0）します。他のターミナルで AI を使っても悪影響はありません。

---

## 事前準備（コマンドパス & デーモン再起動）

1. **バイナリパス**: 以下の例ではアプリバンドル標準パスの `/Applications/TenT.app/Contents/MacOS/tent` を使っています。PATH に通したい場合はシンボリックリンクを作成してください：
   ```bash
   ln -sf /Applications/TenT.app/Contents/MacOS/tent ~/.local/bin/tent
   ```
   （開発ビルドを直接使う場合は `<repo>/target/debug/tent` へ置き換えてください。）
2. **デーモンのアップグレード**: `TENT_PANE_ID` と `TENT_SOCKET` はデーモンプロトコル v5 でペインを起動するときに注入されます。旧バージョンのデーモンが動いている場合は、ステータスバー右上の **「Upgrade」** ボタンを 2 回クリックしてください（1 回目で「Confirm?」に変わり、2 回目で旧デーモンを停止して GUI ごと再起動し、全ペインが v5 デーモン上で起動し直されます）。

---

## 1. Claude Code (`~/.claude/settings.json`)

- `SessionStart` は `startup|resume|clear` に絞り、自動コンパクション（`compact`）時に作業中の緑表示が消えないようにします。
- `Notification` の `matcher` は `permission_prompt|elicitation_dialog|elicitation_url_dialog` に絞り、サンドボックス内ネットワーク許可待ちや MCP 問い合わせを拾いつつ、`idle_prompt` / `auth_success` での誤点灯を防ぎます（`PermissionRequest` の後に届く `permission_prompt` の重複は TenT 側で抑止されます）。
- **制限事項**: Claude Code の仕様上、`Esc` / `Ctrl+C` でユーザーが中断したときは `Stop` フックが発火しないため、次のプロンプト送信またはセッション終了まで緑表示が残ります。

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear",
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook idle --tool claude" }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool claude" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool claude" }
        ]
      }
    ],
    "PostToolUseFailure": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool claude" }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool claude" }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt|elicitation_dialog|elicitation_url_dialog",
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool claude" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool claude" }
        ]
      }
    ],
    "StopFailure": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool claude" }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook idle --tool claude" }
        ]
      }
    ]
  }
}
```

---

## 2. agy (`~/.gemini/config/hooks.json`)

- `--tool agy` を指定すると、`agy` のフック契約に合わせて stdout へ必ず JSON を返します（`--event pre-tool-use` 指定時は `{"decision":"allow"}`、それ以外は `{}`）。
- `Stop` 時の stdin JSON に `USER_CANCELED` が含まれる場合は自動で `idle`（クリア）扱いになります。

```json
{
  "tent-status": {
    "SessionStart": [
      { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook idle --tool agy" }
    ],
    "PreInvocation": [
      { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool agy" }
    ],
    "PreToolUse": [
      {
        "matcher": "ask_question",
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool agy --event pre-tool-use" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "ask_question",
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool agy" }
        ]
      }
    ],
    "Stop": [
      { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool agy" }
    ]
  }
}
```

---

## 3. Codex (`~/.codex/hooks.json`)

Codex ではフック機能は既定で有効です（旧バージョンで無効な場合のみ `~/.codex/config.toml` の `[features]` に `hooks = true` を設定してください）。

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool codex" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook working --tool codex" }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool codex" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook waiting --tool codex" }
        ]
      }
    ],
    "Interrupt": [
      {
        "hooks": [
          { "type": "command", "command": "/Applications/TenT.app/Contents/MacOS/tent hook idle --tool codex" }
        ]
      }
    ]
  }
}
```
