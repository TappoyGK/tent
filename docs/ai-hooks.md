# AI エージェント監視フック設定ガイド

TenT は **Claude Code / agy / Codex** の 3 ツールをフォアグラウンドプロセスから自動判別します。  
これら 3 ツールの稼働中は PTY 出力による画面差分・アイドル推測を完全に停止し、各ツールのライフサイクルフックから送られる明示的なイベント（`tent hook`）のみで LED（緑／黄）・タイムスタンプ・OS 通知を駆動します。

> **TenT 外で起動した場合**: `TENT_PANE_ID` 環境変数が未設定のため、`tent hook` は何もせず即座に正常終了（exit 0）します。他のターミナルで AI を使っても悪影響はありません。

以下の例では `tent` コマンドに PATH が通っている想定です（開発ビルドを直接使う場合は `/path/to/myterm/target/debug/tent` などの絶対パスを指定してください）。

---

## 1. Claude Code (`~/.claude/settings.json`)

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "tent hook working --tool claude" }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "tent hook waiting --tool claude" }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "tent hook waiting --tool claude" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "tent hook waiting --tool claude" }
        ]
      }
    ]
  }
}
```

---

## 2. agy (`~/.gemini/config/hooks.json`)

`--tool agy` を指定すると、`agy` のフック契約に合わせて stdout へ `{}`（`PreToolUse` 時は `{"decision":"allow"}`）を返します。また `Stop` 時に `USER_CANCELED` を受け取った場合は自動で `idle`（クリア）扱いになります。

```json
{
  "tent-status": {
    "PreInvocation": [
      { "type": "command", "command": "tent hook working --tool agy" }
    ],
    "PreToolUse": [
      {
        "matcher": "ask_question",
        "hooks": [
          { "type": "command", "command": "tent hook waiting --tool agy" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "ask_question",
        "hooks": [
          { "type": "command", "command": "tent hook working --tool agy" }
        ]
      }
    ],
    "Stop": [
      { "type": "command", "command": "tent hook waiting --tool agy" }
    ]
  }
}
```

---

## 3. Codex (`~/.codex/config.toml` + `~/.codex/hooks.json`)

`~/.codex/config.toml` でフックを有効化します：

```toml
[features]
codex_hooks = true
```

`~/.codex/hooks.json` にフックを登録します：

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "tent hook working --tool codex" }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          { "type": "command", "command": "tent hook waiting --tool codex" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "tent hook waiting --tool codex" }
        ]
      }
    ]
  }
}
```
