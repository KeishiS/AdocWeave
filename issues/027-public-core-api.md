# 構文プロファイルとキャンセル可能な公開API

## 目的

CLI、サーバ、WASMおよびLSPが共有する、純粋で決定的なコアAPIを確立する。

## 実装範囲

- バージョン付き `SyntaxProfile`、`Limits`、`ParseOptions` および公開結果型を定義する。
- `parse(source, profile, limits)` 相当のAPIと、診断・CST・ASTを返す契約を定義する。
- 解析結果を出力形式非依存とし、HTMLその他のレンダーバックエンドを追加できる一方向の依存境界を定義する。
- 呼び出し側から注入するキャンセルトークンまたは定期検査フックを追加する。
- 公開型ごとに所有権、ライフタイム、`Send`・`Sync` およびグローバル可変状態を持たない要件を決める。
- キャンセル、上限超過、入力エラーおよび内部不整合を安定したエラーコードへ対応付ける。
- 同じ入力、プロファイル、設定および処理系バージョンに対する決定性をテストする。

## 完了条件

- コアAPIがI/O、DB、時刻、乱数および環境変数へアクセスしない。
- 複数スレッドから独立した解析を安全に実行できる。
- 大きな入力の解析を呼び出し側からキャンセルできる。
- CLIとLSPが同じ公開APIを利用する。

## 検証

```console
cargo test --workspace public_api
cargo test --workspace cancellation
cargo test --workspace send_sync
```

## 依存関係

- 026
