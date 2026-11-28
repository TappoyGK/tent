# TenT

macOS 向けの 10 スロット制ターミナル＆内蔵ブラウザワークスペースです。  
`ghostty-vt`・`wgpu` / `glyphon`・`wry`（`WKWebView`）と常駐 PTY デーモンを組み合わせています。

## 主な機能

- **10 スロット管理（`Cmd+1..9` / `0`）**: サイドバーにライブサムネイル・作業ディレクトリ名・出力中／入力待ち LED を表示（Claude Code / agy / Codex のフック連携は [docs/ai-hooks.md](docs/ai-hooks.md) を参照）。ドラッグ＆ドロップでスロット順の入れ替えや折りたたみが可能
- **ペイン分割 & フォーカス追従**: 左右・上下分割（`Cmd+D` / `Cmd+Shift+D`）、マウスホバーおよびファイル D&D 時のリアルタイムフォーカス切替
- **内蔵ブラウザ（`WKWebView`）**: ターミナルと同じスロット／分割ツリー内でブラウザを管理（`Cmd+B` / `Cmd+Shift+B`）。2 本指スワイプの戻る／進む、再生中オーディオ表示、URL の `Cmd+Click` 起動に対応
- **常駐 PTY デーモン**: GUI を再起動（`Cmd+Opt+R`）しても各ペインのシェル・TUI セッション（画面状態・代替スクリーン・モード）を維持したまま自動再接続
- **日本語・macOS 統合**: 日本語 IME プリエディット、Hiragino Sans フォールバック、DEC Mode 2026 同期描画、Finder ファイルおよびスクショ右下サムネイルの D&D に対応
- **ステータスバー**: バッテリー・Wi-Fi・バージョン表示、ドラッグ操作でセットできる簡易タイマー

## 動作要件

- macOS
- Rust ツールチェーン（[rust-toolchain.toml](rust-toolchain.toml) で指定）

## ビルド・実行

```bash
# 開発ビルドで起動
cargo run

# リリースビルド
cargo build --release

# macOS アプリバンドル（dist/TenT.app）の生成
./scripts/bundle-macos.sh
# /Applications へ配置して起動する例:
# cp -R dist/TenT.app /Applications/ && open /Applications/TenT.app

# テスト・フォーマット・Lint
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## 主なショートカット

| ショートカット | 動作 |
| --- | --- |
| `Cmd+1..9` / `Cmd+0` | スロット切替（同じ番号の再押しで隣列と往復） |
| `Cmd+Opt+1` / `Cmd+Opt+2` | 1-5 列 / 6-0 列の折りたたみ |
| `Cmd+\` | サイドバー全体の表示切替 |
| `Cmd+D` / `Cmd+Shift+D` | 左右分割 / 上下分割 |
| `Cmd+W` | フォーカス中のペイン／ブラウザを閉じる |
| `Cmd+[` / `Cmd+]` | 前後のペインへ移動 |
| `Cmd+Opt+Arrow` | 方向指定でペイン移動 |
| `Cmd+Ctrl+Arrow` | 分割ペイン境界をリサイズ（境界線のマウスドラッグにも対応） |
| `Cmd+B` / `Cmd+Shift+B` | ターミナル ⇔ ブラウザ切替 / ブラウザを左右分割で追加 |
| `Cmd+L` / `Cmd+R` | URL バーへフォーカス / ページ再読み込み |
| `Cmd+Left` / `Cmd+Right` | ブラウザ戻る / 進む（2 本指スワイプ対応） |
| `Cmd+Click` / `Cmd+Shift+Click` | URL を内蔵ブラウザ / 外部ブラウザで開く |
| `Cmd+C` / `Cmd+V` | コピー / ペースト |
| `Cmd+F` | ペイン内検索（ターミナル・内蔵ブラウザ両対応、`Enter` / `Shift+Enter` / `Esc`） |
| `Cmd+K` | 画面・スクロールバックをクリア |
| `Cmd+Opt+R` | TenT 本体再起動（デーモン上のセッションは維持） |
| `Cmd+/` / `Cmd+?` | ショートカットヘルプ表示 |

## 構成

```text
src/
├── main.rs            # エントリポイント・イベントループ・アプリ状態
├── app_events.rs      # winit ウィンドウイベント処理
├── app_shortcuts.rs   # ショートカットディスパッチ
├── browser.rs         # WKWebView ライフサイクル・スワイプ・スナップショット
├── key_map.rs         # キー入力・US/JIS レイアウト対応
├── layout.rs          # ウィンドウ・サイドバー・ステータスバーのレイアウト計算
├── macos.rs           # macOS 固有処理（CWD 取得・D&D・バッテリー/Wi-Fi・通知）
├── state.rs           # セッション状態の永続化（state.conf）
├── daemon/            # 常駐 PTY デーモン（Unix ソケット・プロトコル v3）
├── renderer/          # wgpu / glyphon による描画（ペイン・サイドバー・バー・ヘルプ）
├── slot/              # 10 スロット・分割ツリー・セッション復旧
└── terminal_pane/     # ghostty-vt ラッパー・スナップショット・テキスト選択
```

## データ・ログの保存先

- セッション設定・ソケット: `~/Library/Application Support/TenT/`
- パニックログ: `~/Library/Logs/TenT/panic.log`

## リリース・開発手順

バージョニング規則とリリースブランチの運用手順は [docs/release.md](docs/release.md) を参照してください。

## ライセンス

Copyright (c) 2026 TAPPOY G.K.

TenT は [MIT](LICENSE-MIT) または [Apache-2.0](LICENSE-APACHE) のいずれかを選択して利用できます。
