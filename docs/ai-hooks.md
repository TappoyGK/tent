# AI エージェント監視フック設定ガイド

TenT は **Claude Code / agy / Codex** の 3 ツールをフォアグラウンドプロセスから自動判別します。  
判別対象は、実行ファイル名（`claude` / `claude-code` / `agy` / `antigravity` / `antigravity-cli` / `codex` / `codex-cli`）、Claude Code ネイティブ版の `/claude/versions/` パス、`argv[0]` のベース名、および `node` / `bun` / `deno` 経由で起動された公式パッケージパス（`node_modules/@anthropic-ai/claude-code`・`node_modules/@openai/codex`）です。別名や独自ラッパー経由で起動した場合は判別されず一般ルールのまま動作します。

フックを設定した環境では、セッション開始（`SessionStart`）または最初のフック受信から AI フック駆動モードに入り、PTY 出力による画面差分・アイドル推測を完全に停止して、各ツールのライフサイクルフック（`tent hook`）のみで LED（緑／黄）・タイムスタンプ・OS 通知を駆動します。

- **フック未設定の環境**: フックを 1 度も受信していないペインでは従来の一般ルール（可視テキスト変化＋OSC 133）がそのまま動作します。
- **サブエージェント／バックグラウンドタスクの扱い**: `tent hook` は stdin JSON のトップレベルフィールドのみを解析し、ツールごとの公式フック仕様に合わせて不要なイベントを無視します（詳細は下記各ツールの節を参照）。
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
- **サブエージェントの扱い**: `SubagentStop` / `SubagentStart` は登録不要で、万が一登録されてもトップレベルの `hook_event_name` で自動無視します。また、`--tool claude` でトップレベルに非空の `agent_id` があるイベントのうち、`PermissionRequest` / `Notification` 以外の `waiting`・`idle` は無視します（`PermissionRequest` / `Notification` の許可待ちと `working` は親ペインへ反映します）。
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

- `--tool agy` を指定すると、`agy` のフック契約に合わせて stdout へ必ず JSON を返します（`--event pre-tool-use` 指定またはトップレベル `hookEventName: "PreToolUse"` 時は `{"decision":"allow"}`、それ以外は `{}`）。
- `Stop` 時のトップレベル `terminationReason` が `"USER_CANCELED"` の場合は、`fullyIdle` の値にかかわらず自動で `idle`（クリア）扱いになります。
- `Stop` 時のトップレベル `fullyIdle` が `false`（バックグラウンドタスク残存中）の場合は、入力待ち（黄）へ遷移させず無視します。
- **ユーザー質問（`ask_question`）の検知**: `ask_question` ツールは `PreToolUse` / `PostToolUse` フックで即時に入力待ち（黄）／作業中（緑）を切り替えます。
- **パーミッション確認待ちの画面補完検知**: `agy` のフック仕様には `PermissionRequest` イベントが存在しないため、`agy` が `Working` 中にアクティブ画面の最後の非空行から最大 16 行以内（折り返し行は連結）で、以下の決まった英語選択肢ペア（大文字・小文字区別なし）が「最後の拒否行とその直上の許可行ブロックが 10 論理行以内」に並んだまま 1.2 秒以上変化せずに留まった場合、自動で入力待ち（黄）へ遷移させます（見出しより上の行でスピナー更新が続いていても、ダイアログ署名対象行が 1.2 秒変わらなければ検知します）。
  - 選択肢の照合時は前後の空白・枠線（`│┃║`）・選択記号（`>❯›▸▶→●○◉•*`）・2 桁までの連番（`1.` / `1)`）を取り除いた行全体で判定します（行末に他の文字や飾りが付く場合は一致しません）。
  - **許可側**: `Yes, run command` / `Yes, allow access` / `Yes, allow creation` / `Yes, allow tool call` / `Yes, accept this change` / `Yes, elevate` / `Yes, grant permission` / `Yes, and always allow ...` / `Yes, grant permission for ...` / `Yes, and always grant permission for ...` / `Yes, use Auto mode until you exit...` / `Yes, and use Auto mode until you exit...` / `Yes, and tell ... what to do next`
  - **拒否側**: `No, deny` / `No, deny access` / `No, deny creation` / `No, deny tool call` / `No, deny permission` / `No, reject this change` / `No, and always deny ...` / `No, and tell ... what to do differently`
  - **署名に使う見出し行**: 許可行ブロックの直前から、ひとつ上のダイアログの選択肢行の手前まで遡って次の 10 見出しのいずれか（前後の空白と枠線 `│┃║` のみを除いた行全体の大文字・小文字区別なし一致。選択記号等は除かない）が見つかれば、その見出し行から最後の拒否行までを署名にします。見つからない場合は選択肢行のみで署名を作ります。
    - `Run this command?` / `Allow access to this URL?` / `Allow creation of this file?` / `Allow calling this tool?` / `Do you want to proceed?` / `Allow sandbox bypass for command execution?` / `Grant requested permission?` / `Accept this file edit?` / `Allow access to this file?` / `Allow administrator elevation?`
  - **通知とスロット切替**: 画面補完検知の OS 通知は裏スロットまたはウィンドウ非フォーカス時に作業時間にかかわらず送信されます（フック由来の `Waiting` は 5 秒以上の作業時のみ）。アクティブスロットで検知したまま別スロットへ移動すると、裏スロット側が黄色（入力待ち）になります。また、`Waiting` 中に別内容のダイアログへ切り替わった場合は新しい許可待ちとして通知します。
  - **復帰と再検知抑止**: 矢印キー（CSI / SS3・単発・連続・修飾付き）・Tab / Shift+Tab・マウスシーケンスでは `Waiting` を維持します。1 回の入力が `Enter`（`\r` / `\n` / `\r\n`）・`Esc`（`\x1b`）・`Ctrl+C`（`\x03`）と完全一致した場合に確定・キャンセル応答とみなして `Working` へ復帰し（`clear_attention` 後の応答・1.2 秒未満での即応答・フック由来 `Waiting` 中の応答も含む）、現在画面の署名を応答済みとして記録します。それ以外の入力（未対応文字・数字・`y`/`n`・スペース・貼り付け等）は応答済み署名を記録せず一時的に `Working` に戻すため、ダイアログがそのまま 1.2 秒残れば再び `Waiting` になります。また、画面検知による `Waiting` 中にマウス操作等でダイアログが 1.2 秒以上画面から消え続けた場合も自動で `Working` へ復帰します（一瞬の描き替えによる重複通知を防止）。
  - 応答済み署名は、検出不能（`None`：ダイアログ消去・末尾 3 非空行への `esc to interrupt` 出現・許可と拒否が 10 論理行超に乖離・16 行の検査範囲外へスクロール）が 1.2 秒以上継続したとき、別のダイアログを新規検知したとき、`Cmd+K`、またはシェル復帰時にクリアされます（`PreInvocation` 等の `Working` フックでは消えません）。また、画面検知の `Waiting` をフック（`Stop` 等）が上書きした場合も署名は応答済みへ引き継がれます。
- **制限事項**:
  - `agy` のフック仕様では、`fullyIdle: false` の `Stop` のあとにすべてのバックグラウンドタスクが完了しても改めて `fullyIdle: true` の `Stop` が発火するかは定められていません。改めて発火しない場合は、次のプロンプト送信または手動キャンセルまで緑表示が残ります。
  - パーミッション確認待ちの画面補完検知は `agy` の `Working` 状態のみが対象であり、上記の英語選択肢文言・並び条件およびアクティブ画面の最後の非空行から最大 16 行の検査範囲に依存します（行末に別表記が付く描き方や、見出し〜拒否行の内部で経過時間等が動き続けて 1.2 秒安定しない場合は検知できません）。
  - `esc to interrupt` による実行中除外は検査範囲の末尾 3 非空行（フッター）のみが対象であり、末尾から 4 行目以上にある実行中表示は除外されません。
  - ブラウザ側で開く `Allow remote debugging?` は端末画面に出ないため検知できません。また、`Allow sandbox bypass for command execution?` は許可側・拒否側ともに選択肢が未確認、`Allow administrator elevation?` は拒否側の選択肢が未確認のため、上記一覧に一致しない場合は検知できません。確定・キャンセルキー（`Enter` / `Esc` / `Ctrl+C`）の実際の挙動も `agy` 実機では未確認です。
  - 上記 10 見出し以外の見出しで選択肢行が同一のダイアログが続いた場合、またはまったく同じ見出し・内容・選択肢のダイアログが答えたダイアログの画面残存中（あるいは `None` が 1.2 秒継続する前）に再出現した場合は、応答済みとみなして取りこぼすことがあります。また、会話本文に上記一覧と完全一致する選択肢行が並んだ場合は、出力継続中であっても 1.2 秒後に誤検知が起こりえます。

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
`SubagentStop` / `SubagentStart` は登録不要で、登録してもトップレベルの `hook_event_name` で自動無視します（`--tool codex` でもトップレベルに非空の `agent_id` がある `PermissionRequest` / `Notification` 以外の `waiting`・`idle` は無視します）。なお Codex の仕様上、サブエージェントのフックも親の `session_id` を使用するため、ペイロードからメインと区別できないイベントがあります。

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
