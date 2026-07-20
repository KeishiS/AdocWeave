# Code ActionとFormatting

## 目的

既存の安全なFixとFormatterをLanguage Serverから利用可能にする。

## 実装範囲

- 診断に付属する安全なFixをCode Actionへ変換する。
- `textDocument/codeAction` を実装する。
- `textDocument/formatting` を実装する。
- 文書バージョンとEdit適用範囲を検証する。
- 重複または交差するEditを返さない。

## 完了条件

- 行末空白、見出し空白、文書末尾改行を修正できる。
- Formatting結果を再適用しても追加変更が発生しない。
- 未対応領域とコードブロック本文を変更しない。
- 古い解析結果からEditを返さない。

## 検証

```console
cargo test -p asciiloom-lsp code_action
cargo test -p asciiloom-lsp formatting
```

## 依存関係

- 008
- 011
- 020
