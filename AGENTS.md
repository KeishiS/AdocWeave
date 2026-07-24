# Instructions

## Communication

- 日本語で対応する。
- 簡単な質問には簡潔に答え、必要性のない長い背景説明や過剰な箇条書きを避ける。
- 研究に関する文章は mathematical writing に準拠し、仮定、定義、主張、考察を区別する。

## Working agreement

- 原則として、プロジェクトの既存フォーマットと命名規則に従う。
- 変更後は関連する範囲の検証を実行する。
- 破壊的操作または外部へ影響する操作は事前に確認する。

## Git operations

- 環境変数に設定されている `GIT_AUTHOR_*` や `GIT_COMMITTER_*` は変更や削除をしてはいけない。
- `main`は保護branchである。直接push、force push、branch削除、および`main`上での作業commitを行わない。
- 変更は必ず最新の`origin/main`から短命な作業branchへ作成し、branchをpushしてpull requestで統合する。
- merge前に必須checkの成功と`origin/main`へのrebaseを確認する。releaseはmerge後の`main`で成功したcandidate commitだけをtag付けする。
- `git status`、`git diff`、`git log`などの読み取り操作は必要に応じて実行する。
- 可能な限り独立した単位で`git commit`を行う。

## Subagents and Git worktrees

- 単純な質問や、前の結果に依存する逐次作業にはサブエージェントを使用しない。
- 独立して実行できる境界の明確なタスク（調査、検証、テスト、レビュー等）にはサブエージェントを適切に使用する。
- エージェントがファイルの書き換えを行う場合はGit worktreeとbranchを活用する。
- branch名には作業主体と目的が分かる名前を使用する。
- worktree作成前に、未commit変更、既存worktreeを確認する。
- 親エージェントはサブエージェントの結果、差分、検証結果を確認し、統合と最終回答に責任を持つ。
