# AsciiLoom 実装Issue一覧

各Issueは、単独で動作確認できる最小限の変更単位を表す。原則として番号順に進めるが、依存関係を満たすIssueは並行して実施してよい。

本計画では、段階0から3までを「コア初期リリース」、属性、アンカー、リンク、リスト、ノート参照および数式までを「ノートアプリ互換プロファイル」として分ける。コア初期リリースはノートアプリ対応完了を意味しない。

## 運用方針

- 一つのIssueを、可能な限り一つの独立したコミットとして完了する。
- 構文追加では、解析だけでなくHTML、診断、LSP、整形のうち該当する機能まで縦に実装する。
- fixtureを用意し、正常系、不完全入力、境界値を確認する。
- 各Issueの完了時に `cargo fmt`、`cargo clippy`、関連テストを実行する。
- 未対応構文を推測して変換せず、安全なプレーンテキストまたは診断として扱う。
- 文法、置換、HTML出力または公開APIを変更するIssueでは、対応する規範的仕様と互換性fixtureも同じIssueで更新する。
- ネイティブ、WASM、CLI、LSPおよびサーバ統合で別々の簡易パーサーを実装しない。
- 公開APIはファイル、ネットワーク、環境変数、時刻、UUID解決等のホスト機能へ暗黙にアクセスしない。
- Issue番号は依存順を示す。実装開始前に各Issueの未決事項が解消されていることを確認する。

## マイルストーン

### M1: プレーンテキスト変換

- [001: Cargo workspaceとCLIの初期化](001-workspace-and-cli.md)
- [002: ソース位置と行索引](002-source-position-and-line-index.md)
- [003: 共通診断モデル](003-diagnostic-model.md)
- [004: 入力行と改行の認識](004-source-lines.md)
- [005: 段落の構文解析](005-paragraph-parser.md)
- [006: 段落のHTML変換](006-paragraph-html.md)
- [007: 基本Lint](007-basic-lint.md)
- [008: 最小Formatter](008-basic-formatter.md)

### M2: 文書タイトルと見出し

- [009: タイトルと見出しの解析](009-heading-parser.md)
- [010: 見出しHTMLとID生成](010-heading-html-and-id.md)
- [011: 見出しLintと文書シンボル](011-heading-lint-and-symbols.md)

### M3: 基本インライン構文

- [012: インライン構文の基盤](012-inline-foundation.md)
- [013: 等幅文字](013-monospace-inline.md)
- [014: 強調](014-strong-inline.md)
- [015: 斜体とインラインエラー回復](015-emphasis-and-recovery.md)

### M4: コードブロック

- [016: リテラルブロック](016-literal-block.md)
- [017: ソースコードブロック](017-source-block.md)
- [018: 上限とセキュリティ制約](018-limits-and-security.md)

### M5: Language Server

- [019: LSP起動と文書同期](019-lsp-document-sync.md)
- [020: LSP診断とUnicode位置変換](020-lsp-diagnostics.md)
- [021: LSP文書シンボル](021-lsp-document-symbols.md)
- [022: Code ActionとFormatting](022-lsp-actions-and-formatting.md)
- [023: ホバーと補完](023-lsp-hover-and-completion.md)

### M6: Zedとコア初期リリース

- [024: Zed Editor連携](024-zed-integration.md)
- [025: コア初期リリース検証](025-initial-release.md)

### M7: 規範的契約と公開コア

- [026: 規範的文法と置換モデル](026-normative-grammar-and-substitutions.md)
- [027: 構文プロファイルとキャンセル可能な公開API](027-public-core-api.md)
- [028: HTML出力契約とレンダーポリシー](028-html-contract-and-render-policy.md)

### M8: ノートアプリ互換構文

- [029: 文書属性と保護メタデータ](029-document-attributes.md)
- [030: 明示アンカーと安定ID](030-anchors-and-stable-ids.md)
- [031: 通常リンクとURL検証](031-links-and-url-policy.md)
- [032: リストとブロック継続](032-lists-and-continuations.md)
- [033: note参照マクロとResolver境界](033-note-reference-macro.md)
- [034: LaTeX STEMと数式拡張境界](034-latex-stem.md)

### M9: ノートアプリ統合

- [035: メタデータ・参照・検索テキスト抽出](035-note-projections.md)
- [036: サーバ実行・キャッシュ・観測性](036-server-execution.md)
- [037: WASM APIとWeb Worker統合](037-wasm-worker-integration.md)
- [038: Language Serverプロトコル完成](038-lsp-protocol-completion.md)

### M10: 品質保証と互換プロファイル公開

- [039: Fuzzing・property test・セキュリティ試験](039-fuzz-property-security-tests.md)
- [040: ネイティブ・WASM共通fixtureと互換性試験](040-cross-runtime-conformance.md)
- [041: ノートアプリ互換プロファイルのリリース検証](041-note-profile-release.md)
