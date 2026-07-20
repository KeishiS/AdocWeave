# cargo-makeによる開発タスク統一

## 目的

format、構文・静的検査、テストおよびビルドの標準コマンドを `Makefile.toml` に集約し、ローカル開発とCIで同じ手順を実行できるようにする。

## 実装範囲

- `cargo-make` の `Makefile.toml` をリポジトリルートへ追加する。
- 変更を行うformatと、変更せず差分を検査するformat-checkを分離する。
- workspace全体のcheck、clippy、testおよびbuildタスクを定義する。
- format-checkからbuildまでを一括実行するverifyタスクを定義し、既定タスクにする。
- clippyは全target・全featureを対象とし、warningをエラーとして扱う。
- Nix開発環境へ `cargo-make` を追加する。
- タスク名、用途および直接対応するCargoコマンドを文書化する。

## 完了条件

- `cargo make format` でRustコードを整形できる。
- `cargo make format-check` がファイルを変更せず整形差分を検出する。
- `cargo make check`、`clippy`、`test`、`build` を個別に実行できる。
- `cargo make verify` と引数なしの `cargo make` が全検証を実行する。
- 開発者固有の環境変数またはグローバル設定に依存しない。

## 検証

```console
nix develop --command cargo make format
nix develop --command cargo make verify
nix develop --command cargo make
```

## 依存関係

- 001
