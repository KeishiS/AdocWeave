# 最小Formatter

## 目的

プレーンテキスト領域を安全かつ冪等に整形する。

## 実装範囲

- 改行コードを設定された形式へ統一する。
- 行末空白を削除する。
- 文書末尾の改行を統一する。
- ブロック間の過剰な空行を削減する。
- 整形結果またはText Editを返すAPIを定義する。

## 完了条件

- `format(format(source)) = format(source)` が成立する。
- 整形前後で段落ASTの意味が変わらない。
- `format` が標準出力へ結果を出せる。
- `--check` 相当の非破壊な確認方法がある。

## 検証

```console
cargo test formatter
cargo run -- format fixtures/format/basic.adoc
```

## 依存関係

- 005
- 007
