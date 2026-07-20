# LSP診断とUnicode位置変換

## 目的

共通診断をLSP診断へ変換し、最新文書について通知する。

## 実装範囲

- パーサー診断とLint診断をLSP形式へ変換する。
- 内部バイト範囲をLSPのUTF-16位置へ変換する。
- 解析結果と文書バージョンを対応付ける。
- 閉じた文書の診断を消去する。
- 不完全入力を受けてもサーバーを終了しない。

## 完了条件

- ASCII、日本語、絵文字、結合文字で診断位置が正しい。
- 古い文書バージョンの診断を送信しない。
- changeごとに最新の診断へ更新される。
- 診断コードと重要度が共通モデルと一致する。

## 検証

```console
cargo test -p asciiloom-lsp diagnostics
cargo test -p asciiloom-lsp unicode_positions
```

## 依存関係

- 019
