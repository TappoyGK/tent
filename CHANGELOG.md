# Changelog

このプロジェクトの主な変更履歴を記録します。  
フォーマットは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に準拠し、バージョニングは [Semantic Versioning](https://semver.org/lang/ja/) に従います。

## [0.1.22] - 2026-11-27

### Added
- TenT カスタムアプリアイコンを追加（`assets/tent-icon.png`・`assets/tent-icon-256.png`・`assets/TenT.icns`）
- macOS の Dock / `Cmd+Tab` へ起動時にカスタムアイコンを自動適用
- GitHub Actions CI（`.github/workflows/ci.yml`）を追加（`macos-latest` で `fmt` / `clippy` / `test` を自動実行）
- macOS `.app` バンドル生成スクリプト（`scripts/bundle-macos.sh`）とタグ連動の GitHub Releases 配布ワークフロー（`.github/workflows/release.yml`）を追加

## [0.1.21] - 2026-11-26

### Added
- 初回公開リリース
- 10 スロット制ターミナル＆内蔵ブラウザ（`WKWebView`）ワークスペース
- `ghostty-vt`・`wgpu` / `glyphon`・常駐 PTY デーモン（プロトコル v3）によるセッション維持
- 左右・上下のペイン分割、マウスホバー／D&D 時のリアルタイムフォーカス追従
- 日本語 IME・Hiragino Sans フォールバック・DEC Mode 2026 同期描画
- Finder ファイルおよび macOS スクショ右下サムネイルの D&D 対応（`/tmp/tent-drops/` 自動退避）
- 2 本指スワイプによるブラウザ戻る／進む、再生中オーディオ表示
- ステータスバー（バッテリー・Wi-Fi・バージョン表示・ドラッグ操作タイマー）
- デュアルライセンス（MIT / Apache-2.0）
