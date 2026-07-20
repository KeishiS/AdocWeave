# 初期リリース検証

## 目的

段階0から3までの機能を統合し、初期リリース候補として検証する。

## 実装範囲

- 段落、タイトル、見出し、等幅、強調、斜体を統合確認する。
- ソースコードブロックとリテラルブロックを統合確認する。
- CLI、HTML、Lint、Formatter、LSP、Zedの受け入れfixtureを追加する。
- 初期対象外と意図的な互換性差異を文書化する。
- crate、CLI、Language Serverのバージョンと名称を確認する。

## 完了条件

- `convert`、`check`、`format` が代表fixtureで動作する。
- Formatterの冪等性と意味保存のテストが通る。
- 不完全入力、境界値、Unicode、資源上限のテストが通る。
- LSPの診断、シンボル、Code Action、Formattingが動作する。
- Zed上の手動確認項目が完了している。
- 全workspaceのfmt、clippy、testが成功する。

## 検証

```console
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

## 依存関係

- 018
- 024
