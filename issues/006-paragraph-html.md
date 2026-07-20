# 段落のHTML変換

## 目的

段落ASTを安全なHTML断片へ変換する。

## 実装範囲

- HTMLに依存しないASTを走査するレンダラーを追加する。
- HTML固有の設定、エスケープおよび出力型を専用moduleへ隔離する。
- パーサーからHTMLレンダラーへの依存を作らず、HTMLバックエンドだけがsemantic ASTへ依存する。
- 段落を `p` 要素へ変換する。
- 通常テキストのHTML特殊文字を必ずエスケープする。
- 変換結果、診断、文書属性を返すAPIの骨格を定義する。

## 完了条件

- `<`、`>`、`&`、引用符を含む入力が安全に出力される。
- 入力中の生HTMLが要素として通過しない。
- 同じ入力と設定から同じHTMLを生成する。
- CLIの `convert` でHTML断片を取得できる。
- HTML以外のバックエンドを追加するとき、CST・AST・パーサーの変更を必要としない。

## 検証

```console
cargo test html
cargo run -- convert fixtures/plain/escaping.adoc
```

## 依存関係

- 005
