# バージョニングとリリース手順

TenT は [Semantic Versioning](https://semver.org/lang/ja/) とリリースブランチ運用を採用します。

## 1. バージョン表記の仕組み

- **`Cargo.toml` の `version`**:
  - 開発中のリリースブランチ（`release/vX.Y.Z`）ではプレリリース表記 **`X.Y.Z-dev`**（例: `0.2.0-dev`）にする。
  - リリース確定時に **`X.Y.Z`**（例: `0.2.0`）へ更新する。
- **ステータスバー表示（`TENT_DISPLAY_VERSION`）**:
  - [build.rs](../build.rs) がビルド時に自動生成する。
  - タグ `vX.Y.Z` と完全一致し作業ツリーがクリーンなビルド → **`v0.2.0`**
  - それ以外の開発ビルド → **`v0.2.0-dev@<7桁ハッシュ>`**（未コミット変更がある場合は末尾に `*` が付く）

これにより、PR ごとに `Cargo.toml` を書き換えてコンフリクトさせることなく、`Cmd+Opt+R` で最新ビルドへ切り替わったかをステータスバーから即座に判別できます。

## 2. 通常開発フロー

1. `main` から次期リリースブランチ `release/vX.Y.Z` を切る。
2. 最初の PR で `Cargo.toml` を `X.Y.Z-dev` に上げ、`CHANGELOG.md` に `## [X.Y.Z] - Unreleased` を追加する。
3. 各機能・修正は `release/vX.Y.Z` から作業ブランチ（`feat/*`, `fix/*`, `ci/*`, `chore/*`）を切り、`release/vX.Y.Z` 向けの PR を作成する。
4. CI（`check`）とレビューを通過したら **Squash and merge** で `release/vX.Y.Z` へ取り込む。

## 3. リリース手順

1. リリース対象の変更が `release/vX.Y.Z` へ揃ったら、確定用 PR（`chore/release-vX.Y.Z` → `release/vX.Y.Z`）で以下を更新してマージする:
   - `Cargo.toml`（および `Cargo.lock`）の `version` から `-dev` を外す（`X.Y.Z-dev` → `X.Y.Z`）
   - `CHANGELOG.md` の `## [X.Y.Z] - Unreleased` をリリース日（`## [X.Y.Z] - YYYY-MM-DD`）へ確定する
2. `release/vX.Y.Z` → `main` のリリース PR を作成し、CI（`check`）通過後に **Create a merge commit** でマージする（リリース内の履歴と共通祖先を保持するため Squash にしない）。
3. `main` を最新へ同期し、注釈付きタグを打ってプッシュする:
   ```bash
   git checkout main
   git pull
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z
   ```
4. タグプッシュにより [.github/workflows/release.yml](../.github/workflows/release.yml) が自動実行され、`TenT.app` の `.zip` と `.sha256` が GitHub Releases へ公開される。
5. `main` から次のリリースブランチ `release/vX.Y.Z` を切って次の開発へ移る。
