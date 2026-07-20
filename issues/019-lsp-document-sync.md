# LSP起動と文書同期

## 目的

エディター非依存の解析器を利用する最小Language Serverを実装する。

## 実装範囲

- `asciiloom-lsp` workspace memberを追加する。
- `initialize`、`shutdown`、`exit` を実装する。
- `didOpen`、`didChange`、`didClose` を処理する。
- URI、バージョン、テキスト、行索引、解析結果を文書状態として保持する。
- 初期実装では変更ごとに全文を再解析する。

## 完了条件

- 標準入出力でLanguage Serverを起動・終了できる。
- 複数文書の状態を独立に保持できる。
- 古いバージョンの変更を結果へ反映しない。
- 文書を閉じた後に状態を破棄する。

## 検証

```console
cargo test -p asciiloom-lsp document_sync
```

## 依存関係

- 018
