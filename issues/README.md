# AsciiLoom 実装Issue一覧

各Issueは、単独で動作確認できる最小限の変更単位を表す。原則として番号順に進めるが、依存関係を満たすIssueは並行して実施してよい。

## 運用方針

- 一つのIssueを、可能な限り一つの独立したコミットとして完了する。
- 構文追加では、解析だけでなくHTML、診断、LSP、整形のうち該当する機能まで縦に実装する。
- fixtureを用意し、正常系、不完全入力、境界値を確認する。
- 各Issueの完了時に `cargo fmt`、`cargo clippy`、関連テストを実行する。
- 未対応構文を推測して変換せず、安全なプレーンテキストまたは診断として扱う。

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

### M6: Zedと初期リリース

- [024: Zed Editor連携](024-zed-integration.md)
- [025: 初期リリース検証](025-initial-release.md)
