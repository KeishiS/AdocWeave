# コントリビューションガイド

AdocWeaveへの変更は、短命なブランチで作業し、pull requestを通して`main`へ統合します。
個人開発を前提とするため、承認者によるレビューは必須としませんが、CIの必須checkはすべて成功させてください。

## 開発環境

対応する開発環境はNixで提供しています。リポジトリのルートで次を実行してください。

```console
nix develop
cargo make verify
```

`cargo make verify`は、整形、型検査、テスト、ブラウザー向けテスト、文書検査を実行します。
環境の詳細は[docs/nix-installation.adoc](docs/nix-installation.adoc)、個別のテストは
[docs/testing.adoc](docs/testing.adoc)を参照してください。

## 作業の進め方

1. `main`を最新にし、変更の目的に合う短命なブランチを作成します。

   ```console
   git switch main
   git pull --ff-only origin main
   git switch -c fix/browser-archive-import-check
   ```

2. 一つのブランチでは一つの目的に集中し、実装と対応するテスト・文書を更新します。
3. 変更をcommitする前に、原則として完全な検証を実行します。

   ```console
   cargo make verify
   ```

4. 最新の`origin/main`へrebaseし、ブランチをpushしてpull requestを作成します。

   ```console
   git fetch origin main
   git rebase origin/main
   git push -u origin fix/browser-archive-import-check
   gh pr create
   ```

5. pull requestの必須checkがすべて成功したことを確認し、rebase and mergeで`main`へ統合します。
   統合後は作業ブランチを削除します。

`main`へ直接pushしたり、merge commitを作成したりしないでください。競合の解消が難しい場合や変更の目的が
増えた場合は、pull requestを分割します。

## ブランチ名

変更内容に応じて、次の接頭辞を使います。

| 接頭辞 | 用途 |
| --- | --- |
| `feat/<topic>` | 利用者に見える機能の追加 |
| `fix/<topic>` | 不具合の修正 |
| `refactor/<topic>` | 外部の意味を変えない内部構造の整理 |
| `ci/<topic>` | CI、ビルド、リリース、開発環境の変更 |
| `docs/<topic>` | 文書だけの変更 |

## 変更時の確認

- 不具合修正には、可能な限り再発を防ぐテストを追加します。
- 公開API、構文、診断、HTML、WASM応答などの意味を変える場合は、関連する仕様文書、移行ガイド、
  テスト用入力を確認します。
- 依存関係を更新する場合は、対象のlockfileだけを更新し、`cargo make dependency-governance`を実行します。
- `.adoc`文書は既存の用語と書式に合わせます。文書検査の詳細は[docs/testing.adoc](docs/testing.adoc)を参照してください。

## Pull request

pull requestには、変更の目的、主な変更点、実行した検証を簡潔に記載します。大きな設計判断を含む場合は、
関連する文書やADRも同じpull requestで更新してください。

CIではdependency governance、source quality、fuzz quality、Nix package qualityを確認します。失敗した場合は、
[docs/continuous-integration.adoc](docs/continuous-integration.adoc)の切り分け方法に従ってください。

ブランチ運用の完全な規則は[docs/git-workflow.adoc](docs/git-workflow.adoc)にあります。
