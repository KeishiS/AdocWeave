# 基本Lint

## 目的

共通診断モデルを使う最初のLintルールと安全な自動修正を実装する。

## 実装範囲

- `trailing-whitespace` を実装する。
- `excessive-blank-lines` を実装する。
- `line-too-long` を設定可能な規則として実装する。
- ルールごとの有効化、無効化、重要度変更の設定型を定義する。
- 安全なルールにText Editを付与する。

## 完了条件

- 各ルールが安定したIDと正確な範囲を持つ。
- 無効化したルールは診断を生成しない。
- `check` が診断を人間可読形式とJSON形式で出力できる。
- 自動修正で対象外の文字を変更しない。

## 検証

```console
cargo test lint
cargo run -- check fixtures/lint/basic.adoc
```

## 依存関係

- 003
- 004
